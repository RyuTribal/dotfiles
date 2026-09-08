
//! `mach note` — jot a quick note; one haiku call classifies it into 1-N
//! self-contained durable facts under a single topic, stores each directly
//! (bypassing the save-time supersession classifier `mach kb add` runs —
//! a note is fresh, deliberate input, not a digest candidate; supersession
//! against it is `mach kb reflect`'s job later), and prints a short "refer
//! to it as" summary back so the user knows how to recall it in a future
//! session.
//!
//! Mirrors `classify::Classifier` / `reflect::ReflectLlm`: behind a
//! `NoteLlm` trait so the prompt-building/parsing logic here is
//! unit-testable without spawning a real `claude` process; `ProcessNoteLlm`
//! reuses `classify::run_claude` (same invocation flags, same
//! `MACH_KB_DIGEST=1` recursion guard) is the real implementation.
//!
//! Capture must never lose the note: a malformed or missing classifier
//! reply falls back to storing the raw note verbatim as one fact (topic
//! "notes", title from its first few words), and a missing embedding
//! (ollama down) stores the fact anyway, unembedded, with a warning that
//! recall will only find it by keyword until it's re-embedded.
//!
//! `--image PATH` attaches an image to the note: the file is copied into a
//! permanent, content-addressed store (`store_image`) before anything else
//! is attempted, then described with one vision-capable `claude -p` call
//! (`describe_image`). That call deliberately does *not* go through the
//! same `NoteLlm`/`run_claude` invocation the text classifier uses: it
//! needs its Read tool to reach a path outside whatever directory `claude`
//! happens to be launched from (permission prompts are disabled, so a
//! denied Read is silent -- the model just narrates the denial in prose,
//! a non-empty reply easily mistaken for a real description), so
//! `describe_image` adds one extra `--add-dir` scoped to exactly the
//! image's own directory. The user's typed text (if any) plus the image
//! description are combined into one note string and run through the
//! *same* classification pipeline below, so an image note gets a topic and
//! title exactly like a text one. "Never lose the note" extends to the
//! image: a failed or empty vision reply falls back to a fixed placeholder
//! description rather than aborting, and the copied image file is never
//! deleted by anything in this crate (see `store::delete`'s doc comment).
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::classify::run_claude;
use crate::embed::{Embedder, OllamaEmbedder};
use crate::store::{self, KbError};

const TIMEOUT: Duration = Duration::from_secs(30);
/// Exposed (rather than kept private) so other callers of `file_note` /
/// `file_note_with_image` -- namely `engines/telegram`'s bridge -- default
/// to the exact same importance a plain `mach note` would.
pub const DEFAULT_IMPORTANCE: i64 = 6;

pub trait NoteLlm {
    fn call(&self, prompt: &str) -> Result<String, String>;
}

/// Shells out to `claude -p --model haiku`, same invocation `classify` and
/// `reflect` already use.
pub struct ProcessNoteLlm {
    claude_bin: String,
}

impl ProcessNoteLlm {
    /// Uses `CLAUDE_BIN` if set (same env var the rest of the kb engine
    /// honors), otherwise plain `claude` from `PATH`.
    pub fn new() -> Self {
        ProcessNoteLlm { claude_bin: std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()) }
    }
}

impl Default for ProcessNoteLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl NoteLlm for ProcessNoteLlm {
    fn call(&self, prompt: &str) -> Result<String, String> {
        run_claude(&self.claude_bin, "haiku", TIMEOUT, prompt)
    }
}

/// Desktop-notification urgency, mirroring `notify-send -u`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Normal,
    Critical,
}

impl Urgency {
    fn as_str(self) -> &'static str {
        match self {
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        }
    }
}

/// Sends a desktop notification. Behind a trait (same shape as `NoteLlm`)
/// so the decision logic in `maybe_notify` is unit-testable with a fake
/// that records calls instead of ever spawning a real `notify-send`.
pub trait Notifier {
    fn notify(&self, urgency: Urgency, summary: &str);
}

/// Shells out to `notify-send`, fire-and-forget: `--quiet` mode is a
/// detached background process by the time this ever fires (the panel has
/// already closed), so nothing here waits on the child or treats a failed
/// or missing `notify-send` binary as fatal — a lost notification must
/// never take the note down with it.
pub struct ProcessNotifier;

impl Notifier for ProcessNotifier {
    fn notify(&self, urgency: Urgency, summary: &str) {
        let _ = Command::new("notify-send")
            .arg("-u")
            .arg(urgency.as_str())
            .arg("-a")
            .arg("mach note")
            .arg(summary)
            .spawn();
    }
}

/// What became of one `mach note` invocation, for notification purposes
/// only. `Success` covers the ordinary case (classified normally, fully
/// embedded) and notifies nothing — silence is the success signal per
/// `--quiet`'s whole point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteOutcome {
    Success,
    /// Nothing could be stored at all — this should be nearly impossible
    /// given the fallbacks elsewhere in this module (empty note, or a
    /// hard DB error opening/reading/writing the store).
    HardFailure { reason: String },
    /// Stored, but the classifier's reply was empty or malformed, so the
    /// note was filed raw (verbatim, topic "notes") instead of classified.
    FallbackRaw { reason: String },
    /// Stored and classified normally, but at least one fact's embedding
    /// could not be computed (e.g. ollama unreachable) — recall for it
    /// falls back to keyword search until it is re-embedded. Distinct from
    /// `FallbackRaw`: the note text itself was never at risk here.
    MissingEmbedding { reason: String },
    /// Stored and classified normally, but the `WORTH` verdict was `noise`
    /// — filed unreviewed at importance 2 rather than lost. Surfaced (only
    /// under `--quiet`, and only when this was the sole degradation — see
    /// `run`'s priority ordering) so rapid-fire jotting stays silent for
    /// real notes but the user still learns when something was junk-binned.
    /// `dubious` gets no notification at all in `--quiet` mode: the review
    /// queue already catches it, and it isn't worth an interruption.
    WorthNoise,
}

/// Takes the first `n` words of `s`, or "untitled note" if there are none
/// — shared by `fallback_classification`'s title and the "Note lost" hard-
/// failure notification, both of which need a short, referable snippet of
/// a note that (in the failure case) was never even classified.
fn first_words(s: &str, n: usize) -> String {
    let words: String = s.trim().split_whitespace().take(n).collect::<Vec<_>>().join(" ");
    if words.is_empty() {
        "untitled note".to_string()
    } else {
        words
    }
}

/// Builds the exact notification (urgency, summary text) for a given
/// outcome, or `None` for `Success` — pure and side-effect-free so it's
/// testable without a `Notifier` at all. `note` is only used to build the
/// "Note lost: <first words>" snippet for `HardFailure`.
pub fn build_notification(outcome: &NoteOutcome, note: &str) -> Option<(Urgency, String)> {
    match outcome {
        NoteOutcome::Success => None,
        NoteOutcome::HardFailure { reason } => {
            Some((Urgency::Critical, format!("Note lost: {} — {}", first_words(note, 6), reason)))
        }
        NoteOutcome::FallbackRaw { reason } => {
            Some((Urgency::Normal, format!("Note saved unclassified ({})", reason)))
        }
        NoteOutcome::MissingEmbedding { reason } => {
            Some((Urgency::Normal, format!("Note saved unembedded ({})", reason)))
        }
        NoteOutcome::WorthNoise => {
            Some((Urgency::Normal, format!("Note saved to review queue (looked like noise): {}", first_words(note, 6))))
        }
    }
}

/// Fires `build_notification`'s result through `notifier` — but only in
/// `--quiet` mode. Interactive terminal use keeps its stdout/stderr output
/// exactly as before and never sends a notification instead; a
/// notification is `--quiet`'s replacement for output the user isn't
/// there at a terminal to see, not an addition on top of it.
pub fn maybe_notify(notifier: &dyn Notifier, quiet: bool, outcome: &NoteOutcome, note: &str) {
    if !quiet {
        return;
    }
    if let Some((urgency, summary)) = build_notification(outcome, note) {
        notifier.notify(urgency, &summary);
    }
}

/// The classifier's judgment of whether a note is worth surfacing as a
/// reviewed memory or filing quietly to the review queue instead — one
/// verdict per whole note (mirrors `TOPIC`/`TITLE`, not per-fact). Judged
/// as: `Durable` would matter in a future, unrelated session (real
/// preferences, project facts, people, commitments); `Dubious` might
/// matter, unclear (vague fragments, context-free references); `Noise` is
/// test strings, gibberish, greetings, or obviously transient ("remind me
/// in 5 min" style) ephemera. See `file_note`'s mapping of each to
/// `reviewed`/importance, and this type's `Copy` derive means reading
/// `classification.worth` multiple times (mapping it, then reporting it on
/// `FiledNote`) never fights the moves of `topic`/`title`/`facts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Worth {
    Durable,
    Dubious,
    Noise,
}

/// Parses one `WORTH:` line's value (already stripped of the prefix),
/// case-insensitively. `None` for anything else — including empty or
/// garbled text — so the caller's fail-open default (`Durable`) applies
/// exactly like a missing line: a broken classifier must not demote a real
/// note.
fn parse_worth(s: &str) -> Option<Worth> {
    match s.trim().to_ascii_lowercase().as_str() {
        "durable" => Some(Worth::Durable),
        "dubious" => Some(Worth::Dubious),
        "noise" => Some(Worth::Noise),
        _ => None,
    }
}

/// The result of classifying one note: one topic for the whole note, a
/// short referable title, 1-N self-contained durable facts, and a `worth`
/// triage verdict for the whole note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub topic: String,
    pub title: String,
    pub facts: Vec<String>,
    pub worth: Worth,
}

/// Builds the classification prompt: the note, the existing topics to
/// anchor against (topic reuse beats topic drift), and the strict output
/// format `parse_classification` expects back.
pub fn build_prompt(note: &str, existing_topics: &[String], today: &str) -> String {
    let mut s = String::new();
    s.push_str(
        "You are triaging a quick note for a personal knowledge bank. Split it into 1 or more \
         self-contained, durable facts, each understandable on its own without the rest of the \
         note. Convert any relative dates (\"tomorrow\", \"next month\", \"in two weeks\") into \
         absolute dates -- today is ",
    );
    s.push_str(today);
    s.push_str(
        ". Assign ONE short kebab-case topic for the whole note -- reuse one of the existing \
         topics below if it genuinely fits the note, otherwise invent a new short one. Also \
         produce a short, referable title (3-6 words) the user could use to recall this note in \
         a later session.\n\n",
    );
    s.push_str(
        "When the note attributes a want, statement, or commitment to a named person or project \
         ('Moses wants X', 'for the umoja project'), PRESERVE that attribution and project name \
         inside each fact -- 'Moses wants satellite ownership delegation in Umoja (2026-09-07)' \
         beats 'satellite delegation should exist'. Prefer the mentioned project as the topic.\n\n",
    );
    s.push_str(
        "Also judge, once for the whole note (not per fact), how likely it is to matter in a \
         future, unrelated session: WORTH durable means real preferences, project facts, \
         people, or commitments that would still matter later; WORTH dubious means a vague \
         fragment or context-free reference whose future relevance is unclear; WORTH noise \
         means a test string, gibberish, a greeting, or an obviously transient one-off (\"remind \
         me in 5 min\" style ephemera).\n\n",
    );
    if existing_topics.is_empty() {
        s.push_str("Existing topics: (none yet)\n\n");
    } else {
        s.push_str("Existing topics: ");
        s.push_str(&existing_topics.join(", "));
        s.push_str("\n\n");
    }
    s.push_str("Note:\n");
    s.push_str(note);
    s.push_str(
        "\n\nReply in EXACTLY this format and nothing else, one FACT line per fact (at least \
         one), no numbering, no other text:\n\
         TOPIC: <topic>\n\
         TITLE: <title>\n\
         WORTH: durable|dubious|noise\n\
         FACT: <fact one>\n\
         FACT: <fact two>\n",
    );
    s
}

fn strip_prefix_ci<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    if line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&line[prefix.len()..])
    } else {
        None
    }
}

/// Parses the classifier's reply into a `Classification`. Scans line by
/// line for `TOPIC:`/`TITLE:`/`FACT:` (case-insensitive prefix, tolerating
/// stray leading/trailing whitespace and chatter around them); anything
/// that fails to yield a non-empty topic, title, and at least one fact
/// falls back to `fallback_classification` — the raw note is never lost to
/// a flaky or malformed reply.
pub fn parse_classification(output: &str, note: &str) -> Classification {
    parse_classification_verbose(output, note).0
}

/// Same as `parse_classification`, but also reports whether the well-
/// formed direct parse succeeded (`false`) or the reply was empty/
/// malformed and `fallback_classification` had to be used instead
/// (`true`) — the caller uses this to decide whether to notify that the
/// note was filed raw instead of classified.
pub fn parse_classification_verbose(output: &str, note: &str) -> (Classification, bool) {
    let mut topic: Option<String> = None;
    let mut title: Option<String> = None;
    let mut worth: Option<Worth> = None;
    let mut facts: Vec<String> = Vec::new();

    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = strip_prefix_ci(line, "TOPIC:") {
            if topic.is_none() {
                let t = rest.trim();
                if !t.is_empty() {
                    topic = Some(t.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(line, "TITLE:") {
            if title.is_none() {
                let t = rest.trim();
                if !t.is_empty() {
                    title = Some(t.to_string());
                }
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(line, "WORTH:") {
            // Same "first well-formed value wins" rule as TOPIC/TITLE; an
            // unrecognized value (garbled or missing entirely) leaves
            // `worth` at `None`, which the match below fails open to
            // `Durable` on -- a broken classifier must not demote a real
            // note.
            if worth.is_none() {
                worth = parse_worth(rest);
            }
            continue;
        }
        if let Some(rest) = strip_prefix_ci(line, "FACT:") {
            let fact = rest.trim();
            if !fact.is_empty() {
                facts.push(fact.to_string());
            }
            continue;
        }
    }

    match (topic, title) {
        (Some(topic), Some(title)) if !facts.is_empty() => {
            (Classification { topic, title, facts, worth: worth.unwrap_or(Worth::Durable) }, false)
        }
        _ => (fallback_classification(note), true),
    }
}

/// The "never lose the note" fallback: stores the raw note verbatim as one
/// fact, topic "notes", title built from its first few words, worth
/// `Durable` -- a classifier reply too malformed to parse at all must not
/// also demote the note out of the reviewed set; fail open on both fronts.
fn fallback_classification(note: &str) -> Classification {
    let trimmed = note.trim();
    Classification { topic: "notes".to_string(), title: first_words(trimmed, 6), facts: vec![trimmed.to_string()], worth: Worth::Durable }
}

/// Kebab-cases arbitrary text for use in a `source` value (`note:<slug>`):
/// lowercase, ASCII alphanumerics kept, every run of anything else
/// collapsed to a single `-`, no leading/trailing `-`. Never empty — falls
/// back to "note" if the input has no alphanumeric characters at all.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true; // suppresses a leading dash
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "note".to_string()
    } else {
        out
    }
}

fn to_io(e: KbError) -> io::Error {
    io::Error::other(e.to_string())
}

fn print_help() {
    println!("mach note — jot a quick note; it becomes classified memories");
    println!();
    println!("usage: mach note \"<text>\" [-i N]");
    println!("       mach note [-i N]           reads stdin if piped, else opens $EDITOR");
    println!("       mach note - [-i N]         reads stdin explicitly (never $EDITOR)");
    println!("       mach note --image PATH [\"<text>\"] [-i N]");
    println!("                                   attaches an image, described and filed");
    println!("                                   alongside any text (text is optional)");
    println!("       note \"<text>\" [-i N]       same, via the `note` symlink");
    println!();
    println!("  -i, --importance N   1-10, default 6");
    println!("  --image PATH         attach an image (copied to a permanent store)");
    println!("  --quiet              suppress success stdout; on failure or a raw/");
    println!("                       unembedded fallback, send a desktop notification");
    println!("                       instead (via notify-send) rather than printing");
}

/// Opens `$EDITOR` (falling back to `vi`) on an empty temp file and returns
/// whatever was saved. The editor inherits this process's stdio, so an
/// interactive editor works normally in a terminal.
fn read_from_editor() -> io::Result<String> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let mut path = std::env::temp_dir();
    path.push(format!("mach-note-{}.md", std::process::id()));
    std::fs::write(&path, b"")?;
    let status = Command::new(&editor).arg(&path).status();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let _ = std::fs::remove_file(&path);
    match status {
        Ok(s) if !s.success() => {
            eprintln!(
                "mach note: warning: editor '{}' exited with a non-zero status — using whatever was saved",
                editor
            );
        }
        Err(e) => {
            eprintln!("mach note: could not launch editor '{}': {}", editor, e);
            std::process::exit(1);
        }
        _ => {}
    }
    Ok(content)
}

/// Directory under `~/.local/share/mach` where images attached to notes
/// live permanently, addressed by content hash. Nothing in this crate ever
/// deletes from here (see `store::delete`'s doc comment) — a note going
/// dormant or being forgotten only ever touches the `memories` table.
fn images_dir() -> io::Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| io::Error::other("HOME not set"))?;
    let dir = PathBuf::from(home).join(".local/share/mach/kb-images");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Hex-encodes a byte slice by hand (lowercase, no separators) — avoids
/// pulling in a whole crate just for this.
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Copies `src` into the permanent image store under a content-addressed
/// name (first 16 hex chars of its sha256 digest, plus its original
/// extension when it has one, else `.bin`) and returns the stored path.
/// Content addressing means re-attaching the same bytes twice reuses one
/// file rather than duplicating it. Copies, never moves — the source (e.g.
/// a scratch clipboard-paste temp file) is left for its caller to clean up.
pub fn store_image(src: &Path) -> io::Result<PathBuf> {
    let bytes = std::fs::read(src)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let hash16 = to_hex(&digest[..8]); // 8 bytes = 16 hex chars
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    let dest = images_dir()?.join(format!("{}.{}", hash16, ext));
    if !dest.exists() {
        std::fs::write(&dest, &bytes)?;
    }
    Ok(dest)
}

/// The fallback used whenever the vision call can't produce a description
/// -- spawn error, timeout, non-zero exit, or an empty reply. The image is
/// already safely copied by this point either way; this only affects the
/// text of the filed fact.
const UNDESCRIBED_IMAGE: &str = "image note (undescribed)";

/// Vision needs more time than the plain-text classifier: reading and
/// transcribing an image genuinely takes longer than parsing a few lines
/// -- doubly so now that this call runs on sonnet rather than haiku (see
/// `describe_image`'s comment on why).
const VISION_TIMEOUT: Duration = Duration::from_secs(75);

/// Describes the image at `path` in one brief sentence with a
/// vision-capable, non-interactive `claude -p --model haiku` call --
/// mirroring `classify::run_claude`'s invocation flags (same disallowed
/// tools, same `MACH_KB_DIGEST=1` recursion guard) but NOT going through
/// that shared function, because this call needs one flag `run_claude`
/// doesn't take: `--add-dir <path's parent>`. Without it, `claude`'s Read
/// tool -- the only way a non-interactive session can look at an image
/// file, there being no dedicated image-attachment flag as of this
/// writing -- silently denies reading anything outside its own working
/// directory (permission prompts are disabled), and the model narrates
/// that denial in prose instead of describing the image: a non-empty
/// reply the caller would otherwise mistake for a real description.
/// `--add-dir` here is scoped to exactly the one directory this call needs
/// to read from (the image's own), never a blanket grant.
///
/// Returns `None` on a spawn error, timeout, non-zero exit, or an empty
/// reply, so the caller can fall back to `UNDESCRIBED_IMAGE` -- describing
/// the image is never allowed to fail the note.
///
/// `caption` is whatever text the user typed alongside the image (empty if
/// none) -- it is folded into the prompt as context, not just concatenated
/// after the fact: a caption tells the model *why* the image was saved, so
/// it should steer what gets extracted (a desk-setup photo captioned "my
/// new desk setup" should yield facts about the setup; a latency graph
/// captioned "zenith latency after the fix" should yield the finding it
/// shows), not just get appended alongside an unrelated scene description.
fn describe_image(claude_bin: &str, path: &Path, caption: &str) -> Option<String> {
    let caption_context = if caption.trim().is_empty() {
        String::new()
    } else {
        format!(" The user captioned this image: \"{}\".", caption.trim())
    };
    let prompt = format!(
        "Use the Read tool to view the image at {}.{} Interpret the image in that context: if it \
         contains readable text, notes, diagrams, code, or chat/document content, transcribe/\
         extract that content faithfully and completely -- it is the primary payload, not the \
         fact that a picture exists. Also incorporate what the image IS and MEANS given the \
         caption -- the caption tells you why this was saved; let it steer what you extract, \
         merging caption-meaning with image-content rather than just describing a scene. If there \
         is genuinely no meaningful readable content and no caption context to interpret from, \
         describe the scene concisely instead. No preamble, no markdown formatting.",
        path.display(),
        caption_context
    );

    // Deliberately sonnet, not haiku, unlike every other call in this
    // module: transcribing real handwritten whiteboard/document photos
    // needs real OCR-grade vision quality, and haiku was verified (by
    // hand, against a real photo) to confidently hallucinate plausible-
    // looking but wrong technical content instead of admitting it
    // couldn't read the handwriting -- sonnet read the same photo
    // correctly. A note capture happens rarely enough per image that the
    // extra latency/cost here is worth transcription actually being
    // trustworthy instead of quietly wrong.
    let mut cmd = Command::new(claude_bin);
    cmd.arg("-p")
        .arg("--model")
        .arg("sonnet")
        .arg("--permission-prompts")
        .arg("none")
        .arg("--disallowedTools")
        .arg("Bash Edit Write NotebookEdit WebFetch WebSearch Agent");
    if let Some(dir) = path.parent() {
        cmd.arg("--add-dir").arg(dir);
    }
    cmd.env("MACH_KB_DIGEST", "1").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());

    let mut child = cmd.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort, same as `run_claude`: a write error here isn't
        // fatal on its own -- the exit-status check below decides.
        let _ = stdin.write_all(prompt.as_bytes());
    }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut stdout) = child.stdout.take() {
                    let _ = stdout.read_to_string(&mut out);
                }
                if !status.success() {
                    return None;
                }
                let t = out.trim();
                return if t.is_empty() { None } else { Some(t.to_string()) };
            }
            Ok(None) => {
                if start.elapsed() >= VISION_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return None,
        }
    }
}

/// Outcome of running one note through the classify+embed+store pipeline —
/// the reusable core of `run` below, factored out so a caller other than
/// this CLI (the `engines/telegram` bridge) can file a note programmatically
/// and compose its own confirmation/failure message instead of `run`'s
/// stdout/notify-send behavior. Mirrors the fields `run` used to compute
/// inline: classification result plus whether either soft-degradation path
/// (`FallbackRaw` / `MissingEmbedding`) was hit and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiledNote {
    pub topic: String,
    pub title: String,
    pub facts: Vec<String>,
    pub used_fallback: bool,
    pub fallback_reason: Option<String>,
    pub any_missing_embedding: bool,
    pub embed_reason: Option<String>,
    /// The `WORTH` verdict this note was filed under -- see `file_note`'s
    /// mapping to `reviewed`/importance. Callers (this module's own `run`,
    /// and the telegram bridge) use it to decide whether to mention the
    /// review-queue demotion back to the user.
    pub worth: Worth,
}

/// Classifies `note` (via `llm`), embeds each resulting fact (via
/// `embedder`), and stores each directly on `conn` — exactly the pipeline
/// `run` drives inline, extracted so it's callable without going through
/// argv/stdin/$EDITOR at all. `image_stored_path`, when given, is appended
/// as an `[image: PATH]` provenance suffix on each stored fact's content
/// (never on the embedded text, nor on the facts reported back in
/// `FiledNote` — those stay the classifier's plain wording); the image
/// itself must already be copied into the permanent store by the caller
/// (see `store_image`) before this is called.
///
/// A hard error here (`conn`/store failure) is the only way a note can be
/// lost from this function's own logic — everything the classifier or
/// embedder can do short of that degrades to `used_fallback` /
/// `any_missing_embedding` instead, same as `run`'s inline version always
/// guaranteed.
///
/// The classification's `worth` verdict maps to `reviewed`/importance as
/// follows: `Durable` (the unchanged, pre-existing behavior) stores
/// reviewed at the caller-supplied `importance` (the `-i` flag, default
/// `DEFAULT_IMPORTANCE`); `Dubious` stores unreviewed at importance 4;
/// `Noise` stores unreviewed at importance 2. Every path still stores —
/// this triages for `mach kb review`, it never refuses or drops a note.
pub fn file_note(
    conn: &Connection,
    llm: &dyn NoteLlm,
    embedder: &dyn Embedder,
    note: &str,
    image_stored_path: Option<&Path>,
    importance: i64,
) -> Result<FiledNote, KbError> {
    let existing_topics = store::distinct_projects(conn)?;
    let today_full = store::now_rfc3339();
    let today = today_full.get(0..10).unwrap_or(&today_full);

    // A failed classifier call (spawn error, timeout, non-zero exit) is
    // captured here (rather than discarded) purely so a "filed raw" report
    // can say why — `parse_classification_verbose` treats the resulting
    // empty output exactly like a malformed reply either way, so the note
    // itself is never at risk from this.
    let (output, llm_err) = match llm.call(&build_prompt(note, &existing_topics, today)) {
        Ok(s) => (s, None),
        Err(e) => (String::new(), Some(e)),
    };
    let (classification, used_fallback) = parse_classification_verbose(&output, note);

    let slug = slugify(&classification.title);
    let source = format!("note:{}", slug);

    // WORTH -> reviewed/importance mapping (see this function's doc
    // comment). `Durable` is exactly the old, unconditional behavior
    // (reviewed, caller's `importance`) -- `Dubious`/`Noise` route to the
    // unreviewed review queue instead, at a fixed importance that reflects
    // how little the classifier trusted them, never the caller's flag.
    let (reviewed, effective_importance) = match classification.worth {
        Worth::Durable => (true, importance),
        Worth::Dubious => (false, 4),
        Worth::Noise => (false, 2),
    };

    let mut any_missing_embedding = false;
    let mut embed_err: Option<String> = None;
    for fact in &classification.facts {
        // Embed the fact's own semantic text (a trailing path would only
        // dilute the vector), but store it with the image's permanent path
        // appended for provenance -- an image-derived fact must always be
        // traceable back to the source picture, whether or not the
        // classifier's own wording happened to mention it.
        let embedding = match embedder.embed(fact) {
            Ok(v) => Some(v),
            Err(e) => {
                any_missing_embedding = true;
                if embed_err.is_none() {
                    embed_err = Some(e.to_string());
                }
                None
            }
        };
        let content = match image_stored_path {
            Some(stored) => format!("{} [image: {}]", fact, stored.display()),
            None => fact.clone(),
        };
        // Direct store call, not `mach kb add`'s subprocess/classifier
        // path: a note is fresh, deliberate input and the save-time
        // supersession classifier is deliberately skipped — that's `mach
        // kb reflect`'s job later. `reviewed`/`effective_importance` come
        // from the WORTH mapping above -- durable notes keep the old
        // "reviewed at the caller's importance" behavior; dubious/noise
        // ones go to the unreviewed review queue instead.
        // A note is the user's own words: basis `stated`.
        store::insert_with_basis(
            conn,
            &content,
            Some(&source),
            Some(&classification.topic),
            reviewed,
            embedding.as_deref(),
            effective_importance,
            Some(store::BASIS_STATED),
        )?;
    }

    Ok(FiledNote {
        topic: classification.topic,
        title: classification.title,
        facts: classification.facts,
        used_fallback,
        fallback_reason: if used_fallback {
            Some(llm_err.unwrap_or_else(|| "classifier reply was empty or malformed".to_string()))
        } else {
            None
        },
        any_missing_embedding,
        embed_reason: if any_missing_embedding {
            Some(embed_err.unwrap_or_else(|| "could not reach ollama for embeddings".to_string()))
        } else {
            None
        },
        worth: classification.worth,
    })
}

/// Same pipeline as `file_note`, but for an image note: describes
/// `stored_image_path` (already copied into the permanent store by the
/// caller — see `store_image`) with one vision call, folds `caption` (the
/// text alongside the image, empty if none) into the same combined-note
/// text `run`'s `--image` branch builds, and files the result through
/// `file_note`. Used by the telegram bridge's photo handling so it gets a
/// topic/title/facts exactly like `mach note --image` does, without going
/// through argv/stdin at all.
pub fn file_note_with_image(
    conn: &Connection,
    llm: &dyn NoteLlm,
    embedder: &dyn Embedder,
    claude_bin: &str,
    stored_image_path: &Path,
    caption: &str,
    importance: i64,
) -> Result<FiledNote, KbError> {
    let description = describe_image(claude_bin, stored_image_path, caption).unwrap_or_else(|| UNDESCRIBED_IMAGE.to_string());
    let mut combined = String::new();
    if !caption.trim().is_empty() {
        combined.push_str(caption.trim());
        combined.push_str("\n\n");
    }
    combined.push_str(&format!("Image note: {}. Image stored at {}.", description, stored_image_path.display()));
    file_note(conn, llm, embedder, &combined, Some(stored_image_path), importance)
}

/// Runs `mach note ...` (also reachable as `note ...` via the argv0
/// dispatch in `mach`'s main). Content comes from a positional argument, or
/// — bare `mach note` — from stdin if piped, else `$EDITOR`. `--image PATH`
/// attaches an image; text is then optional (an image alone is a complete
/// note), and stdin is still consulted if piped, but `$EDITOR` never opens
/// for an image note — that fallback exists for an interactive human typing
/// at a terminal, not for a paste-driven capture flow.
pub fn run(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut content_arg: Option<String> = None;
    let mut importance: i64 = DEFAULT_IMPORTANCE;
    let mut image_arg: Option<String> = None;
    let mut quiet = false;
    // A bare "-" positional is an explicit "read stdin" request, mirroring
    // `mach kb add -`'s convention — distinct from the implicit stdin-read
    // that already happens below when no positional is given at all and
    // stdin isn't a terminal. Kept separate from `content_arg` so it can
    // demand stdin unconditionally (even from a terminal, same as `kb add
    // -`) rather than falling through to $EDITOR.
    let mut explicit_stdin = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-i" | "--importance" => {
                importance = args.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(DEFAULT_IMPORTANCE).clamp(1, 10);
            }
            "--image" => {
                image_arg = match args.next() {
                    Some(p) => Some(p),
                    None => {
                        eprintln!("mach note: --image requires a path argument");
                        std::process::exit(1);
                    }
                };
            }
            "--quiet" => {
                quiet = true;
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-" => {
                if content_arg.is_some() || explicit_stdin {
                    eprintln!("mach note: unexpected argument '-'");
                    std::process::exit(1);
                }
                explicit_stdin = true;
            }
            other => {
                if content_arg.is_none() {
                    content_arg = Some(other.to_string());
                } else {
                    eprintln!("mach note: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }

    let notifier = ProcessNotifier;

    // Copy the image first, before anything else is attempted: whatever
    // happens next (vision call failure, classifier failure), the bytes
    // are already durably saved.
    let image_stored_path: Option<PathBuf> = match image_arg {
        Some(ref img) => match store_image(Path::new(img)) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("mach note: warning: could not save image '{}': {} — filing text only", img, e);
                None
            }
        },
        None => None,
    };

    let raw = if explicit_stdin {
        // Unconditional, exactly like `mach kb add -`: reads whatever is
        // piped, or blocks waiting for terminal input + EOF — never
        // $EDITOR.
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        match content_arg {
            Some(c) => c,
            None if image_stored_path.is_some() => {
                if !io::stdin().is_terminal() {
                    let mut buf = String::new();
                    io::stdin().read_to_string(&mut buf)?;
                    buf
                } else {
                    String::new()
                }
            }
            None if !io::stdin().is_terminal() => {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf)?;
                buf
            }
            None => read_from_editor()?,
        }
    };

    let user_text = raw.trim().to_string();

    let note = match &image_stored_path {
        Some(stored) => {
            let claude_bin = std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
            let description = describe_image(&claude_bin, stored, &user_text).unwrap_or_else(|| UNDESCRIBED_IMAGE.to_string());
            let mut combined = String::new();
            if !user_text.is_empty() {
                combined.push_str(&user_text);
                combined.push_str("\n\n");
            }
            combined.push_str(&format!("Image note: {}. Image stored at {}.", description, stored.display()));
            combined
        }
        None => user_text,
    };

    if note.is_empty() {
        maybe_notify(&notifier, quiet, &NoteOutcome::HardFailure { reason: "empty note — nothing to save".to_string() }, "");
        eprintln!("mach note: nothing to save — empty note");
        std::process::exit(1);
    }

    let conn = match store::open() {
        Ok(c) => c,
        Err(e) => {
            maybe_notify(&notifier, quiet, &NoteOutcome::HardFailure { reason: e.to_string() }, &note);
            return Err(to_io(e));
        }
    };

    let llm = ProcessNoteLlm::new();
    let embedder = OllamaEmbedder::new();
    let filed = match file_note(&conn, &llm, &embedder, &note, image_stored_path.as_deref(), importance) {
        Ok(f) => f,
        Err(e) => {
            maybe_notify(&notifier, quiet, &NoteOutcome::HardFailure { reason: e.to_string() }, &note);
            return Err(to_io(e));
        }
    };

    // Exactly one of these fires (or none, on a clean success): a raw-
    // storage fallback is the more significant degradation, so it takes
    // priority over a same-note missing-embedding notice if somehow both
    // happened at once; a WORTH-noise notice is the least urgent of the
    // three (informational, not a degradation of the note itself) so it
    // only fires when neither of the other two already did. WORTH-dubious
    // never notifies at all, even here — the review queue catches it,
    // silently.
    if filed.used_fallback {
        let reason = filed.fallback_reason.clone().unwrap_or_else(|| "classifier reply was empty or malformed".to_string());
        maybe_notify(&notifier, quiet, &NoteOutcome::FallbackRaw { reason }, &note);
    } else if filed.any_missing_embedding {
        let reason = filed.embed_reason.clone().unwrap_or_else(|| "could not reach ollama for embeddings".to_string());
        maybe_notify(&notifier, quiet, &NoteOutcome::MissingEmbedding { reason }, &note);
    } else if filed.worth == Worth::Noise {
        maybe_notify(&notifier, quiet, &NoteOutcome::WorthNoise, &note);
    }

    // Success = silence under --quiet: none of the stdout below runs, and
    // (per the branch above) nothing was notified either.
    if !quiet {
        if filed.any_missing_embedding {
            println!(
                "warning: could not reach ollama for embeddings — stored without them; \
                 recall will find these by keyword only until re-embedded"
            );
        }

        // A demoted note is still filed in full below (topic, title, every
        // fact) -- this suffix is purely informational, telling an
        // interactive user it landed in the unreviewed review queue
        // instead of the reviewed set.
        let worth_suffix = match filed.worth {
            Worth::Dubious => " (review queue — looked dubious)",
            Worth::Noise => " (review queue — looked like noise)",
            Worth::Durable => "",
        };
        let n = filed.facts.len();
        println!("filed {} {} under \"{}\"{}", n, if n == 1 { "memory" } else { "memories" }, filed.topic, worth_suffix);
        println!("  refer to it as: \"{}\"", filed.title);
        for fact in &filed.facts {
            // Mirrors exactly what got stored (see `file_note`'s insert
            // loop): an image-derived fact's provenance suffix included.
            match &image_stored_path {
                Some(stored) => println!("  - {} [image: {}]", fact, stored.display()),
                None => println!("  - {}", fact),
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeNoteLlm {
        reply: String,
    }

    impl NoteLlm for FakeNoteLlm {
        fn call(&self, _prompt: &str) -> Result<String, String> {
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn parses_well_formed_single_fact_reply() {
        let out = "TOPIC: helios-rendering\nTITLE: Helios RHI descriptor design\nFACT: Look into bindless descriptors for the Helios Vulkan backend, target 2026-10-07.\n";
        let c = parse_classification(out, "irrelevant raw note");
        assert_eq!(c.topic, "helios-rendering");
        assert_eq!(c.title, "Helios RHI descriptor design");
        assert_eq!(c.facts, vec!["Look into bindless descriptors for the Helios Vulkan backend, target 2026-10-07.".to_string()]);
    }

    #[test]
    fn parses_well_formed_multi_fact_reply() {
        let out = "TOPIC: dotfiles\nTITLE: chezmoi migration notes\nFACT: fact one here.\nFACT: fact two here.\nFACT: fact three here.\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.topic, "dotfiles");
        assert_eq!(c.title, "chezmoi migration notes");
        assert_eq!(c.facts, vec!["fact one here.".to_string(), "fact two here.".to_string(), "fact three here.".to_string()]);
    }

    #[test]
    fn tolerates_leading_chatter_and_lowercase_prefixes() {
        let out = "Sure, here you go:\ntopic: notes\ntitle: a title\nfact: the one fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.topic, "notes");
        assert_eq!(c.title, "a title");
        assert_eq!(c.facts, vec!["the one fact".to_string()]);
    }

    #[test]
    fn malformed_reply_falls_back_to_raw_note_verbatim() {
        let note = "Remember to look into bindless descriptors for the helios vulkan backend, target next month";
        for bad in ["", "I couldn't parse this note.", "TOPIC: only-a-topic\n", "TOPIC: x\nTITLE: y\n"] {
            let c = parse_classification(bad, note);
            assert_eq!(c.topic, "notes");
            assert_eq!(c.title, "Remember to look into bindless descriptors");
            assert_eq!(c.facts, vec![note.to_string()]);
        }
    }

    #[test]
    fn fallback_on_empty_note_uses_untitled_placeholder_title() {
        let c = parse_classification("garbage", "   ");
        assert_eq!(c.topic, "notes");
        assert_eq!(c.title, "untitled note");
        assert_eq!(c.facts, vec!["".to_string()]);
    }

    #[test]
    fn build_prompt_includes_existing_topics_for_anchoring() {
        let topics = vec!["helios-rendering".to_string(), "dotfiles".to_string()];
        let prompt = build_prompt("a note", &topics, "2026-09-07");
        assert!(prompt.contains("helios-rendering"));
        assert!(prompt.contains("dotfiles"));
        assert!(prompt.contains("2026-09-07"));
    }

    #[test]
    fn build_prompt_with_no_existing_topics_says_so() {
        let prompt = build_prompt("a note", &[], "2026-09-07");
        assert!(prompt.contains("(none yet)"));
    }

    #[test]
    fn build_prompt_instructs_to_preserve_named_attribution_and_project() {
        // Prompt-contract test: the classifier must be told, in-prompt, to
        // keep a named person's/project's attribution inside each fact
        // instead of flattening it away (e.g. "Moses wants X" must not
        // become "X should exist") -- and to prefer a mentioned project as
        // the topic. Guards against this instruction silently regressing
        // out of `build_prompt` later.
        let prompt = build_prompt("a note", &[], "2026-09-07");
        assert!(prompt.contains("PRESERVE that attribution and project name"));
        assert!(prompt.contains("Moses wants satellite ownership delegation in Umoja"));
        assert!(prompt.contains("Prefer the mentioned project as the topic"));
    }

    #[test]
    fn slugify_lowercases_and_hyphenates() {
        assert_eq!(slugify("Helios RHI descriptor design"), "helios-rhi-descriptor-design");
    }

    #[test]
    fn slugify_collapses_punctuation_and_trims_edges() {
        assert_eq!(slugify("  --Weird!! Title??-- "), "weird-title");
    }

    #[test]
    fn slugify_never_empty() {
        assert_eq!(slugify("!!!"), "note");
        assert_eq!(slugify(""), "note");
    }

    #[test]
    fn fake_note_llm_reply_round_trips_through_parse() {
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: only fact\n".to_string() };
        let out = llm.call("prompt").unwrap();
        let c = parse_classification(&out, "note");
        assert_eq!(c.topic, "t");
        assert_eq!(c.facts, vec!["only fact".to_string()]);
    }

    // ---------- parse_classification_verbose: fallback flag ----------

    #[test]
    fn verbose_parse_reports_no_fallback_on_well_formed_reply() {
        let out = "TOPIC: t\nTITLE: a title\nFACT: only fact\n";
        let (c, used_fallback) = parse_classification_verbose(out, "irrelevant");
        assert!(!used_fallback);
        assert_eq!(c.topic, "t");
    }

    #[test]
    fn verbose_parse_reports_fallback_on_malformed_reply() {
        for bad in ["", "I couldn't parse this.", "TOPIC: only-a-topic\n"] {
            let (c, used_fallback) = parse_classification_verbose(bad, "raw note text");
            assert!(used_fallback, "expected fallback for {:?}", bad);
            assert_eq!(c.topic, "notes");
        }
    }

    // ---------- WORTH parsing ----------

    #[test]
    fn parses_worth_durable() {
        let out = "TOPIC: t\nTITLE: a title\nWORTH: durable\nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Durable);
    }

    #[test]
    fn parses_worth_dubious() {
        let out = "TOPIC: t\nTITLE: a title\nWORTH: dubious\nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Dubious);
    }

    #[test]
    fn parses_worth_noise() {
        let out = "TOPIC: t\nTITLE: a title\nWORTH: noise\nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Noise);
    }

    #[test]
    fn parses_worth_case_insensitively_and_tolerates_surrounding_whitespace() {
        let out = "TOPIC: t\nTITLE: a title\nworth:   Dubious  \nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Dubious);
    }

    #[test]
    fn missing_worth_line_fails_open_to_durable() {
        let out = "TOPIC: t\nTITLE: a title\nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Durable);
    }

    #[test]
    fn garbled_worth_value_fails_open_to_durable() {
        let out = "TOPIC: t\nTITLE: a title\nWORTH: maybe??\nFACT: only fact\n";
        let c = parse_classification(out, "irrelevant");
        assert_eq!(c.worth, Worth::Durable);
    }

    #[test]
    fn a_fully_malformed_reply_still_falls_open_to_durable_worth() {
        // The overall-malformed-reply fallback (topic/title/facts missing
        // entirely) must not ALSO demote the note out of the reviewed set
        // -- fail open on every front at once.
        for bad in ["", "I couldn't parse this.", "TOPIC: only-a-topic\n"] {
            let c = parse_classification(bad, "raw note text");
            assert_eq!(c.worth, Worth::Durable, "expected durable fail-open for {:?}", bad);
        }
    }

    // ---------- first_words ----------

    #[test]
    fn first_words_takes_up_to_n_words() {
        assert_eq!(first_words("one two three four five six seven", 6), "one two three four five six");
        assert_eq!(first_words("one two", 6), "one two");
    }

    #[test]
    fn first_words_of_blank_text_is_untitled_placeholder() {
        assert_eq!(first_words("   ", 6), "untitled note");
        assert_eq!(first_words("", 6), "untitled note");
    }

    // ---------- build_notification ----------

    #[test]
    fn success_outcome_builds_no_notification() {
        assert_eq!(build_notification(&NoteOutcome::Success, "anything"), None);
    }

    #[test]
    fn hard_failure_builds_critical_note_lost_notification() {
        let outcome = NoteOutcome::HardFailure { reason: "database error: disk full".to_string() };
        let (urgency, summary) = build_notification(&outcome, "remember to buy milk and eggs tomorrow").unwrap();
        assert_eq!(urgency, Urgency::Critical);
        assert_eq!(summary, "Note lost: remember to buy milk and eggs — database error: disk full");
    }

    #[test]
    fn fallback_raw_builds_normal_unclassified_notification() {
        let outcome = NoteOutcome::FallbackRaw { reason: "'claude' timed out after 30s".to_string() };
        let (urgency, summary) = build_notification(&outcome, "irrelevant").unwrap();
        assert_eq!(urgency, Urgency::Normal);
        assert_eq!(summary, "Note saved unclassified ('claude' timed out after 30s)");
    }

    #[test]
    fn missing_embedding_builds_normal_unembedded_notification() {
        let outcome = NoteOutcome::MissingEmbedding { reason: "cannot reach ollama at http://127.0.0.1:1".to_string() };
        let (urgency, summary) = build_notification(&outcome, "irrelevant").unwrap();
        assert_eq!(urgency, Urgency::Normal);
        assert_eq!(summary, "Note saved unembedded (cannot reach ollama at http://127.0.0.1:1)");
    }

    #[test]
    fn worth_noise_builds_normal_review_queue_notification() {
        let (urgency, summary) = build_notification(&NoteOutcome::WorthNoise, "asdf test qwerty gibberish here").unwrap();
        assert_eq!(urgency, Urgency::Normal);
        assert_eq!(summary, "Note saved to review queue (looked like noise): asdf test qwerty gibberish here");
    }

    // ---------- maybe_notify: quiet gating, mock notifier ----------

    /// Records every `notify` call instead of ever spawning a real
    /// `notify-send` — the same "fake instead of a real subprocess" shape
    /// as `FakeNoteLlm` above.
    #[derive(Default)]
    struct FakeNotifier {
        calls: std::cell::RefCell<Vec<(Urgency, String)>>,
    }

    impl Notifier for FakeNotifier {
        fn notify(&self, urgency: Urgency, summary: &str) {
            self.calls.borrow_mut().push((urgency, summary.to_string()));
        }
    }

    #[test]
    fn maybe_notify_is_silent_on_success_even_when_quiet() {
        let notifier = FakeNotifier::default();
        maybe_notify(&notifier, true, &NoteOutcome::Success, "a note");
        assert!(notifier.calls.borrow().is_empty());
    }

    #[test]
    fn maybe_notify_fires_on_hard_failure_only_when_quiet() {
        let outcome = NoteOutcome::HardFailure { reason: "boom".to_string() };

        let loud = FakeNotifier::default();
        maybe_notify(&loud, false, &outcome, "a note");
        assert!(loud.calls.borrow().is_empty(), "interactive/non-quiet use must never notify");

        let quiet = FakeNotifier::default();
        maybe_notify(&quiet, true, &outcome, "a note");
        let calls = quiet.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, Urgency::Critical);
        assert!(calls[0].1.starts_with("Note lost: a note"));
    }

    #[test]
    fn maybe_notify_fires_on_fallback_raw_only_when_quiet() {
        let outcome = NoteOutcome::FallbackRaw { reason: "malformed reply".to_string() };

        let loud = FakeNotifier::default();
        maybe_notify(&loud, false, &outcome, "a note");
        assert!(loud.calls.borrow().is_empty());

        let quiet = FakeNotifier::default();
        maybe_notify(&quiet, true, &outcome, "a note");
        let calls = quiet.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (Urgency::Normal, "Note saved unclassified (malformed reply)".to_string()));
    }

    #[test]
    fn maybe_notify_fires_on_missing_embedding_only_when_quiet() {
        let outcome = NoteOutcome::MissingEmbedding { reason: "ollama down".to_string() };

        let loud = FakeNotifier::default();
        maybe_notify(&loud, false, &outcome, "a note");
        assert!(loud.calls.borrow().is_empty());

        let quiet = FakeNotifier::default();
        maybe_notify(&quiet, true, &outcome, "a note");
        let calls = quiet.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (Urgency::Normal, "Note saved unembedded (ollama down)".to_string()));
    }

    #[test]
    fn maybe_notify_fires_on_worth_noise_only_when_quiet() {
        let loud = FakeNotifier::default();
        maybe_notify(&loud, false, &NoteOutcome::WorthNoise, "asdf test qwerty");
        assert!(loud.calls.borrow().is_empty(), "interactive/non-quiet use must never notify");

        let quiet = FakeNotifier::default();
        maybe_notify(&quiet, true, &NoteOutcome::WorthNoise, "asdf test qwerty");
        let calls = quiet.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], (Urgency::Normal, "Note saved to review queue (looked like noise): asdf test qwerty".to_string()));
    }

    // ---------- file_note: the reusable programmatic pipeline ----------

    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().map(|b| b as f32).collect())
        }
    }

    struct FailingEmbedder;

    impl Embedder for FailingEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, KbError> {
            Err(KbError::Embed("ollama unreachable".to_string()))
        }
    }

    fn scratch_conn() -> Connection {
        store::open_with_path(Path::new(":memory:")).unwrap()
    }

    #[test]
    fn file_note_stores_every_fact_and_reports_no_degradation_on_a_clean_reply() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: fact one\nFACT: fact two\n".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "irrelevant raw note", None, 6).unwrap();

        assert_eq!(filed.topic, "t");
        assert_eq!(filed.title, "a title");
        assert_eq!(filed.facts, vec!["fact one".to_string(), "fact two".to_string()]);
        assert!(!filed.used_fallback);
        assert!(filed.fallback_reason.is_none());
        assert!(!filed.any_missing_embedding);
        assert!(filed.embed_reason.is_none());

        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|m| m.project.as_deref() == Some("t")));
        assert!(stored.iter().all(|m| m.source.as_deref() == Some("note:a-title")));
    }

    #[test]
    fn file_note_preserves_named_attribution_and_project_from_a_mocked_classifier_reply() {
        // Mocked e2e test for the attribution fix: a voice note attributing
        // a want to a named person and project ("Moses wants satellite
        // delegation in umoja") must survive into the stored fact and the
        // fact's topic -- this exercises the full file_note pipeline
        // (classify reply -> parse -> embed -> store) with a FakeNoteLlm
        // standing in for a classifier that already followed the new
        // attribution-preserving instruction, the same shape a real
        // `claude -p --model haiku` reply would take.
        let conn = scratch_conn();
        let llm = FakeNoteLlm {
            reply: "TOPIC: umoja\nTITLE: Moses wants satellite delegation\n\
                    FACT: Moses wants satellite ownership delegation in Umoja (2026-09-07).\n"
                .to_string(),
        };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "Moses wants satellite delegation in umoja", None, 6).unwrap();

        assert_eq!(filed.topic, "umoja");
        assert_eq!(filed.facts, vec!["Moses wants satellite ownership delegation in Umoja (2026-09-07).".to_string()]);

        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.contains("Moses"));
        assert!(stored[0].content.contains("Umoja"));
        assert_eq!(stored[0].project.as_deref(), Some("umoja"));
    }

    #[test]
    fn file_note_appends_image_provenance_to_stored_content_but_not_to_reported_facts() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: the fact\n".to_string() };
        let image_path = Path::new("/tmp/some-image.png");
        let filed = file_note(&conn, &llm, &FakeEmbedder, "a note", Some(image_path), 6).unwrap();

        assert_eq!(filed.facts, vec!["the fact".to_string()]);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.contains("the fact"));
        assert!(stored[0].content.contains("/tmp/some-image.png"));
    }

    #[test]
    fn file_note_reports_fallback_on_a_malformed_classifier_reply_but_still_stores_the_raw_note() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "not a valid reply".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "remember to buy milk", None, 6).unwrap();

        assert!(filed.used_fallback);
        assert_eq!(filed.fallback_reason.as_deref(), Some("classifier reply was empty or malformed"));
        assert_eq!(filed.topic, "notes");
        assert_eq!(filed.facts, vec!["remember to buy milk".to_string()]);

        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].content, "remember to buy milk");
    }

    #[test]
    fn file_note_reports_missing_embedding_but_still_stores_unembedded() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: the fact\n".to_string() };
        let filed = file_note(&conn, &llm, &FailingEmbedder, "a note", None, 6).unwrap();

        assert!(filed.any_missing_embedding);
        assert_eq!(filed.embed_reason.as_deref(), Some("ollama unreachable"));
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].embedding.is_none());
    }

    // ---------- file_note: WORTH -> reviewed/importance mapping ----------

    #[test]
    fn worth_durable_stores_reviewed_at_the_callers_importance() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nWORTH: durable\nFACT: the fact\n".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "a note", None, 8).unwrap();

        assert_eq!(filed.worth, Worth::Durable);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].reviewed);
        assert_eq!(stored[0].importance, 8);
    }

    #[test]
    fn worth_dubious_stores_unreviewed_at_importance_four_regardless_of_the_callers_importance() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nWORTH: dubious\nFACT: the fact\n".to_string() };
        // Pass a caller importance far from 4, to prove the fixed mapping
        // wins rather than the flag.
        let filed = file_note(&conn, &llm, &FakeEmbedder, "a note", None, 9).unwrap();

        assert_eq!(filed.worth, Worth::Dubious);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].reviewed);
        assert_eq!(stored[0].importance, 4);
    }

    #[test]
    fn worth_noise_stores_unreviewed_at_importance_two_regardless_of_the_callers_importance() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nWORTH: noise\nFACT: the fact\n".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "a note", None, 9).unwrap();

        assert_eq!(filed.worth, Worth::Noise);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].reviewed);
        assert_eq!(stored[0].importance, 2);
    }

    #[test]
    fn missing_worth_line_files_exactly_like_pre_worth_behavior() {
        // No WORTH line at all -- the fail-open default (Durable) must
        // reproduce the exact old unconditional "reviewed, caller's
        // importance" behavior, unchanged.
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: the fact\n".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "a note", None, 7).unwrap();

        assert_eq!(filed.worth, Worth::Durable);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].reviewed);
        assert_eq!(stored[0].importance, 7);
    }

    #[test]
    fn a_malformed_reply_falls_back_raw_but_still_reviewed_at_the_callers_importance() {
        // used_fallback (raw storage) is a distinct degradation path from
        // WORTH, but both must fail open the same way: a broken classifier
        // never demotes a real note out of the reviewed set.
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "not a valid reply".to_string() };
        let filed = file_note(&conn, &llm, &FakeEmbedder, "remember to buy milk", None, 6).unwrap();

        assert!(filed.used_fallback);
        assert_eq!(filed.worth, Worth::Durable);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].reviewed);
        assert_eq!(stored[0].importance, 6);
    }

    #[test]
    fn file_note_with_image_combines_caption_and_description_and_files_through_file_note() {
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: the fact\n".to_string() };
        // No real `claude` binary needed: an unresolvable path makes
        // `describe_image`'s spawn fail, which is exactly the "never lose
        // the note" fallback path (`UNDESCRIBED_IMAGE`) this test exercises.
        let filed = file_note_with_image(
            &conn,
            &llm,
            &FakeEmbedder,
            "/nonexistent/definitely-not-claude-xyz",
            Path::new("/tmp/photo.jpg"),
            "my new desk setup",
            6,
        )
        .unwrap();

        assert_eq!(filed.topic, "t");
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.contains("/tmp/photo.jpg"));
    }

    #[test]
    fn file_note_with_image_applies_the_worth_verdict_to_the_extracted_content_the_same_way() {
        // The classifier only ever sees the combined caption+description
        // text (image notes don't get their own separate WORTH path) --
        // this confirms the verdict still reaches `reviewed`/importance
        // exactly like a plain text note's does.
        let conn = scratch_conn();
        let llm = FakeNoteLlm { reply: "TOPIC: t\nTITLE: a title\nWORTH: noise\nFACT: the fact\n".to_string() };
        let filed = file_note_with_image(
            &conn,
            &llm,
            &FakeEmbedder,
            "/nonexistent/definitely-not-claude-xyz",
            Path::new("/tmp/photo.jpg"),
            "my new desk setup",
            6,
        )
        .unwrap();

        assert_eq!(filed.worth, Worth::Noise);
        let stored = store::list(&conn, None, false).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].reviewed);
        assert_eq!(stored[0].importance, 2);
    }
}
