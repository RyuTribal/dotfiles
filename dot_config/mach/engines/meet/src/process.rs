//! Orchestrates one meeting directory through phase B:
//! `needs-processing` -> transcribe + merge -> `needs-summary` -> summarize
//! + extract facts -> `processed`. `process_dir` is the imperative shell
//! (real filesystem, real subprocesses, real kb store); `run_transcribe_stage`
//! and `run_summarize_stage` are its two testable halves, taking every
//! external dependency (transcriber, converter, claude call, kb
//! connection, embedder) as a trait object so they run against fakes in
//! tests -- no real whisper.cpp binary, `claude` process, or ollama
//! connection is ever required to exercise this module's own logic.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;

use kb::embed::Embedder;
use kb::store;
use telegram::voice::Converter;

use crate::markers::{decide_stage, StageAction};
use crate::meeting::{Meeting, Mode};
use crate::segment::{merge_dual, merge_solo, Segment};
use crate::summarize::{self, ClaudeLlm, ParseOutcome, ParsedSummary};
use crate::transcribe::{self, SegmentTranscriber};

pub const NEEDS_PROCESSING: &str = "needs-processing";
pub const NEEDS_SUMMARY: &str = "needs-summary";
pub const PROCESSED: &str = "processed";

fn marker_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

pub fn has_marker(dir: &Path, name: &str) -> bool {
    marker_path(dir, name).exists()
}

fn display_title(meeting: &Meeting) -> &str {
    meeting.title.as_deref().unwrap_or("untitled meeting")
}

/// Sends a desktop notification -- same trait-behind-a-fake shape as
/// `kb::note::Notifier`, but two separate summary/body arguments (this
/// task's spec calls for `notify-send "<title>" "<body>"`, not the single
/// combined string `kb::note::ProcessNotifier` sends).
pub trait Notifier {
    fn notify(&self, title: &str, body: &str);
}

/// Fire-and-forget, same as `kb::note::ProcessNotifier`: a missing or
/// failing `notify-send` must never take a meeting's processing down with
/// it, so this never waits on or checks the child.
pub struct ProcessNotifier;

impl Notifier for ProcessNotifier {
    fn notify(&self, title: &str, body: &str) {
        let _ = Command::new("notify-send").arg("-a").arg("mach meet").arg(title).arg(body).spawn();
    }
}

/// Transcribes one track: downsamples to 16kHz mono (see
/// `transcribe::DownsampleConverter`), transcribes the result, then removes
/// the downsampled temp file regardless of outcome -- it's a transcription
/// artifact, not something worth keeping alongside `transcript.md`.
fn transcribe_track(wav_path: &Path, converter: &dyn Converter, transcriber: &dyn SegmentTranscriber) -> Result<Vec<Segment>, String> {
    let wav16 = converter.convert(wav_path)?;
    let result = transcriber.transcribe_segments(&wav16);
    let _ = fs::remove_file(&wav16);
    result
}

/// Transcribes every track `meeting.mode` records and merges them into one
/// transcript string (not yet written to disk -- the caller decides where).
/// A failure transcribing ANY one track fails the whole stage rather than
/// writing a partial transcript: the marker stays `needs-processing` either
/// way (nothing on disk is lost -- the raw audio and the marker are
/// untouched), so this is retried whole next time rather than left
/// half-merged.
pub fn run_transcribe_stage(dir: &Path, meeting: &Meeting, converter: &dyn Converter, transcriber: &dyn SegmentTranscriber) -> Result<String, String> {
    match meeting.mode {
        Mode::Dual => {
            let mine = transcribe_track(&dir.join("mic.wav"), converter, transcriber)?;
            let theirs = transcribe_track(&dir.join("system.wav"), converter, transcriber)?;
            Ok(merge_dual(&mine, &theirs))
        }
        Mode::Solo => {
            let mine = transcribe_track(&dir.join("mic.wav"), converter, transcriber)?;
            Ok(merge_solo(&mine))
        }
    }
}

fn render_summary_md(title: &str, started_at: &str, parsed: &ParsedSummary) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {} ({})\n\n", title, started_at));
    s.push_str("## Summary\n");
    s.push_str(parsed.summary.trim());
    s.push_str("\n\n## Names\n");
    if parsed.names.is_empty() {
        s.push_str("No names inferred from the transcript.\n");
    } else {
        for n in &parsed.names {
            s.push_str(&format!("- {} = {} (inferred)\n", n.label, n.name));
        }
    }
    s.push_str("\n## Facts filed\n");
    if parsed.facts.is_empty() {
        s.push_str("No durable facts extracted.\n");
    } else {
        for f in &parsed.facts {
            s.push_str(&format!("- {}\n", f));
        }
    }
    s
}

#[derive(Debug)]
pub struct SummarizeOutcome {
    pub title: String,
    pub facts_filed: usize,
    pub used_fallback: bool,
}

/// Reads `transcript.md`, runs the one sonnet call, writes `summary.md`,
/// and files any extracted facts into the kb store (unreviewed, source
/// `meeting:<dir_name>`, importance `summarize::FACT_IMPORTANCE`). A
/// malformed/unparseable reply (`ParseOutcome::Fallback`) still writes
/// `summary.md` (the model's raw output) and simply files zero facts --
/// per the task's "parse defensively... never lose the transcript" rule,
/// which this upholds by construction: the transcript was already written
/// to disk in the prior stage and is never touched here.
pub fn run_summarize_stage(
    dir: &Path,
    dir_name: &str,
    meeting: &Meeting,
    llm: &dyn ClaudeLlm,
    conn: &Connection,
    embedder: &dyn Embedder,
) -> Result<SummarizeOutcome, String> {
    let transcript =
        fs::read_to_string(dir.join("transcript.md")).map_err(|e| format!("could not read transcript.md: {}", e))?;
    let prompt = summarize::build_prompt(meeting, &transcript);
    let output = llm.call(summarize::CLAUDE_MODEL, &prompt, summarize::CLAUDE_TIMEOUT)?;
    let title = display_title(meeting).to_string();

    match summarize::parse_summary_output(&output) {
        ParseOutcome::Fallback(raw) => {
            fs::write(dir.join("summary.md"), raw).map_err(|e| format!("could not write summary.md: {}", e))?;
            Ok(SummarizeOutcome { title, facts_filed: 0, used_fallback: true })
        }
        ParseOutcome::Parsed(parsed) => {
            let summary_md = render_summary_md(&title, &meeting.started_at, &parsed);
            fs::write(dir.join("summary.md"), summary_md).map_err(|e| format!("could not write summary.md: {}", e))?;

            let source = format!("meeting:{}", dir_name);
            let mut filed = 0usize;
            for fact in &parsed.facts {
                let embedding = embedder.embed(fact).ok();
                match store::insert(conn, fact, Some(&source), None, false, embedding.as_deref(), summarize::FACT_IMPORTANCE) {
                    Ok(_) => filed += 1,
                    Err(e) => eprintln!("mach meet process: warning: could not file a fact into the kb ({:?}): {}", fact, e),
                }
            }
            Ok(SummarizeOutcome { title, facts_filed: filed, used_fallback: false })
        }
    }
}

/// Resolves a segmented whisper.cpp transcriber, mirroring
/// `telegram`'s own (private) `build_transcriber`: `None` (with a one-line
/// log) means `transcription pending tooling` -- no binary on `PATH`, or
/// the model couldn't be found/downloaded.
fn build_segment_transcriber() -> Option<Box<dyn SegmentTranscriber>> {
    let binary = transcribe::find_binary()?;
    match transcribe::ensure_model() {
        Ok(model) => Some(Box::new(transcribe::WhisperCliSegmentTranscriber::new(binary, model))),
        Err(e) => {
            eprintln!("mach meet process: whisper model unavailable ({}) -- transcription pending tooling", e);
            None
        }
    }
}

/// Runs `run_transcribe_stage`, writes `transcript.md`, and swaps the
/// `needs-processing` marker for `needs-summary` on success. Factored out
/// of `process_dir` (rather than inlined in its `match`) so the marker
/// transition itself -- not just the transcription logic -- is unit
/// testable against fakes, independent of whether whisper.cpp is actually
/// installed on the machine running the tests.
fn advance_from_needs_processing(dir: &Path, meeting: &Meeting, converter: &dyn Converter, transcriber: &dyn SegmentTranscriber) -> Result<String, String> {
    let transcript = run_transcribe_stage(dir, meeting, converter, transcriber)?;
    fs::write(dir.join("transcript.md"), &transcript).map_err(|e| format!("could not write transcript.md: {}", e))?;
    let _ = fs::remove_file(marker_path(dir, NEEDS_PROCESSING));
    fs::write(marker_path(dir, NEEDS_SUMMARY), b"").map_err(|e| format!("could not write needs-summary marker: {}", e))?;
    Ok(transcript)
}

/// Runs `run_summarize_stage` and swaps `needs-summary` for `processed`
/// (with a facts-filed count) on success -- same "marker transition
/// factored out for testability" reasoning as `advance_from_needs_processing`.
fn advance_from_needs_summary(
    dir: &Path,
    dir_name: &str,
    meeting: &Meeting,
    llm: &dyn ClaudeLlm,
    conn: &Connection,
    embedder: &dyn Embedder,
) -> Result<SummarizeOutcome, String> {
    let outcome = run_summarize_stage(dir, dir_name, meeting, llm, conn, embedder)?;
    let _ = fs::remove_file(marker_path(dir, NEEDS_SUMMARY));
    let body = format!("processed at {}\nfacts filed: {}\n", store::now_rfc3339(), outcome.facts_filed);
    fs::write(marker_path(dir, PROCESSED), body).map_err(|e| format!("could not write processed marker: {}", e))?;
    Ok(outcome)
}

fn run_and_report_summary(dir: &Path, dir_name: &str, meeting: &Meeting, notifier: &dyn Notifier) {
    let conn = match store::open() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mach meet process: {}: could not open kb store: {}", dir_name, e);
            notifier.notify(&format!("Meeting processing failed: {}", display_title(meeting)), &format!("could not open knowledge bank: {}", e));
            return;
        }
    };
    let embedder = kb::embed::OllamaEmbedder::new();
    let llm = summarize::ProcessClaudeLlm::new();

    match advance_from_needs_summary(dir, dir_name, meeting, &llm, &conn, &embedder) {
        Ok(outcome) => {
            let fallback_note = if outcome.used_fallback { " (unparseable reply -- raw output saved)" } else { "" };
            println!("mach meet process: {}: {} facts filed, summary ready{}", dir_name, outcome.facts_filed, fallback_note);
            notifier.notify(
                &format!("Meeting processed: {}", outcome.title),
                &format!("{} facts filed, summary ready{}", outcome.facts_filed, fallback_note),
            );
        }
        Err(e) => {
            eprintln!("mach meet process: {}: summarize step failed: {}", dir_name, e);
            notifier.notify(&format!("Meeting processing deferred: {}", display_title(meeting)), &format!("summarize step failed: {} -- will retry", e));
        }
    }
}

/// Processes one meeting directory through whichever stage(s) currently
/// apply, per `markers::decide_stage`. When transcription finishes AND
/// `claude` is reachable, the summarize stage runs immediately after in the
/// same invocation (the common case: `mach meet stop`'s detached spawn,
/// everything available) rather than waiting for a separate wakeup --
/// `needs-summary` only actually persists across invocations when offline.
pub fn process_dir(dir: &Path, notifier: &dyn Notifier) -> io::Result<()> {
    let dir_name = dir.file_name().and_then(|f| f.to_str()).unwrap_or("meeting").to_string();

    let meeting = match Meeting::read(dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("mach meet process: {}: could not read meeting.json: {}", dir_name, e);
            return Ok(());
        }
    };

    let has_np = has_marker(dir, NEEDS_PROCESSING);
    let has_ns = has_marker(dir, NEEDS_SUMMARY);
    let whisper_available = build_segment_transcriber().is_some();
    let claude_online = kb::reflect::claude_reachable();

    match decide_stage(has_np, has_ns, whisper_available, claude_online) {
        StageAction::Done => {}
        StageAction::TranscriptionToolingMissing => {
            println!("mach meet process: {}: transcription pending tooling (no whisper.cpp binary/model found)", dir_name);
            notifier.notify(
                &format!("Meeting processing paused: {}", display_title(&meeting)),
                "transcription pending tooling -- install whisper.cpp to continue",
            );
        }
        StageAction::Transcribe => {
            let transcriber = match build_segment_transcriber() {
                Some(t) => t,
                None => return Ok(()), // raced with the availability check above; retried next time
            };
            let converter = transcribe::DownsampleConverter;
            match advance_from_needs_processing(dir, &meeting, &converter, transcriber.as_ref()) {
                Ok(_transcript) => {
                    println!("mach meet process: {}: transcription complete", dir_name);
                    if claude_online {
                        run_and_report_summary(dir, &dir_name, &meeting, notifier);
                    } else {
                        println!("mach meet process: {}: offline, deferring summarize step", dir_name);
                    }
                }
                Err(e) => {
                    eprintln!("mach meet process: {}: transcription failed: {}", dir_name, e);
                    notifier.notify(&format!("Meeting processing failed: {}", display_title(&meeting)), &format!("transcription failed: {}", e));
                }
            }
        }
        StageAction::SummarizeDeferred => {
            println!("mach meet process: {}: offline, deferring", dir_name);
        }
        StageAction::Summarize => {
            run_and_report_summary(dir, &dir_name, &meeting, notifier);
        }
    }
    Ok(())
}

/// `mach meet process` with no `DIR`: every meeting directory under `root`
/// carrying a `needs-processing` or `needs-summary` marker, oldest first --
/// directory names are timestamp-prefixed (`YYYY-MM-DD-HHMM[-slug]`), so a
/// plain lexicographic sort is already chronological.
pub fn process_queue(root: &Path, notifier: &dyn Notifier) -> io::Result<()> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if has_marker(&path, NEEDS_PROCESSING) || has_marker(&path, NEEDS_SUMMARY) {
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() {
        println!("mach meet process: nothing to process");
        return Ok(());
    }
    for dir in dirs {
        process_dir(&dir, notifier)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::time::Duration;

    use crate::meeting::Pids;

    fn sample_meeting(mode: Mode) -> Meeting {
        Meeting {
            started_at: "2026-09-07T17:33:00Z".to_string(),
            ended_at: Some("2026-09-07T18:03:00Z".to_string()),
            mode,
            title: Some("Weekly Standup".to_string()),
            sample_rate: 48000,
            capture_binary: "pw-record".to_string(),
            pids: Pids { mic: 1, system: Some(2) },
            tracks: vec![],
            duration_secs: Some(1800),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mach-meet-process-test-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // --- run_transcribe_stage: transcriber mocked via the shared trait ---

    struct FakeConverter;
    impl Converter for FakeConverter {
        fn convert(&self, input: &Path) -> Result<PathBuf, String> {
            Ok(input.to_path_buf()) // identity -- no real ffmpeg spawned
        }
    }

    struct FailingConverter;
    impl Converter for FailingConverter {
        fn convert(&self, _input: &Path) -> Result<PathBuf, String> {
            Err("no ffmpeg".to_string())
        }
    }

    /// Returns canned segments keyed by the input path's file name, so a
    /// single fake can stand in for both the mic and system tracks with
    /// distinguishable content.
    struct FakeTranscriber {
        by_file: HashMap<String, Vec<Segment>>,
    }

    impl SegmentTranscriber for FakeTranscriber {
        fn transcribe_segments(&self, wav_path: &Path) -> Result<Vec<Segment>, String> {
            let name = wav_path.file_name().unwrap().to_string_lossy().to_string();
            self.by_file.get(&name).cloned().ok_or_else(|| format!("no fixture for {}", name))
        }
    }

    struct FailingTranscriber;
    impl SegmentTranscriber for FailingTranscriber {
        fn transcribe_segments(&self, _wav_path: &Path) -> Result<Vec<Segment>, String> {
            Err("whisper crashed".to_string())
        }
    }

    fn seg(start_ms: u64, text: &str) -> Segment {
        Segment { start_ms, text: text.to_string() }
    }

    #[test]
    fn run_transcribe_stage_dual_merges_both_tracks() {
        let dir = temp_dir("dual");
        let meeting = sample_meeting(Mode::Dual);
        let transcriber = FakeTranscriber {
            by_file: [
                ("mic.wav".to_string(), vec![seg(0, "hello")]),
                ("system.wav".to_string(), vec![seg(5_000, "hi back")]),
            ]
            .into_iter()
            .collect(),
        };
        let out = run_transcribe_stage(&dir, &meeting, &FakeConverter, &transcriber).unwrap();
        assert_eq!(out, "**You** (00:00): hello\n**Them** (00:05): hi back\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_transcribe_stage_solo_only_reads_mic_and_drops_the_label() {
        let dir = temp_dir("solo");
        let meeting = sample_meeting(Mode::Solo);
        let transcriber = FakeTranscriber { by_file: [("mic.wav".to_string(), vec![seg(0, "just me")])].into_iter().collect() };
        let out = run_transcribe_stage(&dir, &meeting, &FakeConverter, &transcriber).unwrap();
        assert_eq!(out, "(00:00): just me\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_transcribe_stage_propagates_a_converter_failure() {
        let dir = temp_dir("conv-fail");
        let meeting = sample_meeting(Mode::Solo);
        let transcriber = FakeTranscriber { by_file: HashMap::new() };
        let err = run_transcribe_stage(&dir, &meeting, &FailingConverter, &transcriber).unwrap_err();
        assert_eq!(err, "no ffmpeg");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_transcribe_stage_propagates_a_transcriber_failure() {
        let dir = temp_dir("trans-fail");
        let meeting = sample_meeting(Mode::Solo);
        let err = run_transcribe_stage(&dir, &meeting, &FakeConverter, &FailingTranscriber).unwrap_err();
        assert_eq!(err, "whisper crashed");
        let _ = fs::remove_dir_all(&dir);
    }

    // --- run_summarize_stage ---

    struct FakeClaudeLlm {
        reply: String,
    }
    impl ClaudeLlm for FakeClaudeLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            Ok(self.reply.clone())
        }
    }

    struct FailingClaudeLlm;
    impl ClaudeLlm for FailingClaudeLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            Err("offline mid-call".to_string())
        }
    }

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, kb::KbError> {
            Ok(vec![0.1, 0.2, 0.3])
        }
    }

    fn memory_conn() -> Connection {
        store::open_with_path(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn run_summarize_stage_files_facts_and_writes_summary_md() {
        let dir = temp_dir("summarize-ok");
        fs::write(dir.join("transcript.md"), "**You** (00:00): let's ship Friday\n").unwrap();
        let meeting = sample_meeting(Mode::Dual);
        let llm = FakeClaudeLlm {
            reply: "SUMMARY:\nAgreed to ship Friday.\n\nNAMES:\nnone\n\nFACTS:\n- On 2026-09-07, the team agreed to ship Friday.\n".to_string(),
        };
        let conn = memory_conn();
        let outcome = run_summarize_stage(&dir, "2026-09-07-1733-weekly-standup", &meeting, &llm, &conn, &FakeEmbedder).unwrap();

        assert_eq!(outcome.facts_filed, 1);
        assert!(!outcome.used_fallback);
        assert_eq!(outcome.title, "Weekly Standup");

        let summary = fs::read_to_string(dir.join("summary.md")).unwrap();
        assert!(summary.contains("Agreed to ship Friday."));
        assert!(summary.contains("No names inferred"));

        let filed = store::unreviewed(&conn).unwrap();
        assert_eq!(filed.len(), 1);
        assert_eq!(filed[0].source.as_deref(), Some("meeting:2026-09-07-1733-weekly-standup"));
        assert_eq!(filed[0].importance, summarize::FACT_IMPORTANCE);
        assert!(!filed[0].reviewed);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_summarize_stage_falls_back_to_raw_output_on_malformed_reply_and_skips_facts() {
        let dir = temp_dir("summarize-malformed");
        fs::write(dir.join("transcript.md"), "some transcript\n").unwrap();
        let meeting = sample_meeting(Mode::Solo);
        let llm = FakeClaudeLlm { reply: "I couldn't quite parse that into sections.".to_string() };
        let conn = memory_conn();
        let outcome = run_summarize_stage(&dir, "some-dir", &meeting, &llm, &conn, &FakeEmbedder).unwrap();

        assert!(outcome.used_fallback);
        assert_eq!(outcome.facts_filed, 0);
        let summary = fs::read_to_string(dir.join("summary.md")).unwrap();
        assert_eq!(summary, "I couldn't quite parse that into sections.");
        assert!(store::unreviewed(&conn).unwrap().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_summarize_stage_never_touches_the_transcript_on_a_claude_failure() {
        let dir = temp_dir("summarize-claude-fail");
        fs::write(dir.join("transcript.md"), "irreplaceable transcript text\n").unwrap();
        let meeting = sample_meeting(Mode::Solo);
        let conn = memory_conn();
        let err = run_summarize_stage(&dir, "some-dir", &meeting, &FailingClaudeLlm, &conn, &FakeEmbedder).unwrap_err();
        assert_eq!(err, "offline mid-call");
        // The transcript must survive untouched -- this is the whole point
        // of writing it in a prior, independent stage.
        assert_eq!(fs::read_to_string(dir.join("transcript.md")).unwrap(), "irreplaceable transcript text\n");
        assert!(!dir.join("summary.md").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_summarize_stage_errors_clearly_on_a_missing_transcript() {
        let dir = temp_dir("summarize-no-transcript");
        let meeting = sample_meeting(Mode::Solo);
        let conn = memory_conn();
        let err = run_summarize_stage(&dir, "some-dir", &meeting, &FakeClaudeLlm { reply: "x".to_string() }, &conn, &FakeEmbedder).unwrap_err();
        assert!(err.contains("could not read transcript.md"));
        let _ = fs::remove_dir_all(&dir);
    }

    // --- process_dir: marker transitions end to end, real filesystem, fakes for everything external ---

    struct RecordingNotifier {
        calls: RefCell<Vec<(String, String)>>,
    }
    impl RecordingNotifier {
        fn new() -> Self {
            RecordingNotifier { calls: RefCell::new(Vec::new()) }
        }
    }
    impl Notifier for RecordingNotifier {
        fn notify(&self, title: &str, body: &str) {
            self.calls.borrow_mut().push((title.to_string(), body.to_string()));
        }
    }

    fn write_meeting(dir: &Path, meeting: &Meeting) {
        meeting.write(dir).unwrap();
    }

    #[test]
    fn process_dir_with_no_marker_is_a_silent_no_op() {
        let dir = temp_dir("no-marker");
        write_meeting(&dir, &sample_meeting(Mode::Solo));
        let notifier = RecordingNotifier::new();
        process_dir(&dir, &notifier).unwrap();
        assert!(notifier.calls.borrow().is_empty());
        assert!(!dir.join(PROCESSED).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn process_dir_an_unreadable_meeting_json_is_skipped_without_panicking() {
        let dir = temp_dir("bad-json");
        fs::write(dir.join(NEEDS_PROCESSING), b"").unwrap();
        // No meeting.json at all -- Meeting::read must fail cleanly.
        let notifier = RecordingNotifier::new();
        process_dir(&dir, &notifier).unwrap();
        assert!(notifier.calls.borrow().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    // --- marker transitions: needs-processing -> needs-summary -> processed,
    // fully independent of whether whisper.cpp/claude are actually
    // installed on the machine running the tests (both stages driven by
    // fakes, exactly like the plain `run_*_stage` tests above -- these two
    // just also exercise the marker-file swap that wraps them).

    #[test]
    fn advance_from_needs_processing_writes_transcript_and_swaps_the_marker() {
        let dir = temp_dir("advance-transcribe");
        fs::write(dir.join(NEEDS_PROCESSING), b"").unwrap();
        let meeting = sample_meeting(Mode::Solo);
        let transcriber = FakeTranscriber { by_file: [("mic.wav".to_string(), vec![seg(0, "hello")])].into_iter().collect() };

        let transcript = advance_from_needs_processing(&dir, &meeting, &FakeConverter, &transcriber).unwrap();

        assert_eq!(transcript, "(00:00): hello\n");
        assert_eq!(fs::read_to_string(dir.join("transcript.md")).unwrap(), "(00:00): hello\n");
        assert!(!has_marker(&dir, NEEDS_PROCESSING), "needs-processing must be cleared");
        assert!(has_marker(&dir, NEEDS_SUMMARY), "needs-summary must now be set");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn advance_from_needs_processing_leaves_needs_processing_untouched_on_failure() {
        let dir = temp_dir("advance-transcribe-fail");
        fs::write(dir.join(NEEDS_PROCESSING), b"").unwrap();
        let meeting = sample_meeting(Mode::Solo);

        let err = advance_from_needs_processing(&dir, &meeting, &FakeConverter, &FailingTranscriber).unwrap_err();

        assert_eq!(err, "whisper crashed");
        assert!(has_marker(&dir, NEEDS_PROCESSING), "must stay needs-processing -- nothing lost, retried whole next time");
        assert!(!has_marker(&dir, NEEDS_SUMMARY));
        assert!(!dir.join("transcript.md").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn advance_from_needs_summary_writes_summary_files_facts_and_swaps_to_processed() {
        let dir = temp_dir("advance-summarize");
        fs::write(dir.join(NEEDS_SUMMARY), b"").unwrap();
        fs::write(dir.join("transcript.md"), "(00:00): let's ship Friday\n").unwrap();
        let meeting = sample_meeting(Mode::Solo);
        let llm = FakeClaudeLlm {
            reply: "SUMMARY:\nAgreed to ship Friday.\n\nFACTS:\n- On 2026-09-07, the team agreed to ship Friday.\n".to_string(),
        };
        let conn = memory_conn();

        let outcome = advance_from_needs_summary(&dir, "some-dir", &meeting, &llm, &conn, &FakeEmbedder).unwrap();

        assert_eq!(outcome.facts_filed, 1);
        assert!(!has_marker(&dir, NEEDS_SUMMARY), "needs-summary must be cleared");
        assert!(has_marker(&dir, PROCESSED), "processed must now be set");
        let processed_body = fs::read_to_string(dir.join(PROCESSED)).unwrap();
        assert!(processed_body.contains("facts filed: 1"));
        assert_eq!(store::unreviewed(&conn).unwrap().len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn advance_from_needs_summary_leaves_needs_summary_untouched_on_a_claude_failure() {
        let dir = temp_dir("advance-summarize-fail");
        fs::write(dir.join(NEEDS_SUMMARY), b"").unwrap();
        fs::write(dir.join("transcript.md"), "irreplaceable transcript text\n").unwrap();
        let meeting = sample_meeting(Mode::Solo);
        let conn = memory_conn();

        let err = advance_from_needs_summary(&dir, "some-dir", &meeting, &FailingClaudeLlm, &conn, &FakeEmbedder).unwrap_err();

        assert_eq!(err, "offline mid-call");
        assert!(has_marker(&dir, NEEDS_SUMMARY), "must stay needs-summary -- fully retry-able next time");
        assert!(!has_marker(&dir, PROCESSED));
        assert_eq!(fs::read_to_string(dir.join("transcript.md")).unwrap(), "irreplaceable transcript text\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
