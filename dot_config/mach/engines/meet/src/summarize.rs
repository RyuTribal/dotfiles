//! The one sonnet `claude -p` call `mach meet process` makes per meeting:
//! summary + speaker-name inference + durable-fact extraction, all from a
//! single strictly-formatted prompt/reply -- mirrors `kb::reflect`'s
//! `ReflectLlm` trait (a call behind a trait for testability) and reuses
//! `kb::classify::run_claude` directly for the actual subprocess spawn --
//! same flags, same `MACH_KB_DIGEST=1` recursion guard every other
//! non-interactive `claude -p` call in this codebase uses, rather than a
//! second copy of that plumbing.
//!
//! Parsing is defensive by design: a reply that doesn't carry the SUMMARY/
//! FACTS headers this module asks for is never discarded -- see
//! `ParseOutcome::Fallback`, which the caller (`process::run_summarize_stage`)
//! saves verbatim as `summary.md` rather than losing the model's output (or
//! the transcript, which is written to disk in the prior stage regardless
//! of how this one goes).
use std::time::Duration;

use kb::classify::run_claude;

use crate::meeting::{Meeting, Mode};

/// Sonnet, per the task spec -- this call does real reasoning over a full
/// transcript, not a one-word classification.
pub const CLAUDE_MODEL: &str = "sonnet";
pub const CLAUDE_TIMEOUT: Duration = Duration::from_secs(120);
/// `mach note`'s own default importance for a freshly-captured fact --
/// meeting facts are stored at the same weight.
pub const FACT_IMPORTANCE: i64 = 6;
/// Transcript cap before it goes into the prompt -- long enough for a real
/// meeting's substance, short enough to keep the sonnet call's cost and
/// latency bounded regardless of how long the meeting ran.
pub const TRANSCRIPT_CAP_BYTES: usize = 50 * 1024;
/// FACTS section hard cap, per the task spec.
const MAX_FACTS: usize = 8;

pub trait ClaudeLlm {
    fn call(&self, model: &str, prompt: &str, timeout: Duration) -> Result<String, String>;
}

pub struct ProcessClaudeLlm {
    claude_bin: String,
}

impl ProcessClaudeLlm {
    /// Uses `CLAUDE_BIN` if set (same env var every other `claude -p`
    /// caller in this codebase honors), otherwise plain `claude` from
    /// `PATH`.
    pub fn new() -> Self {
        ProcessClaudeLlm { claude_bin: std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()) }
    }
}

impl Default for ProcessClaudeLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeLlm for ProcessClaudeLlm {
    fn call(&self, model: &str, prompt: &str, timeout: Duration) -> Result<String, String> {
        run_claude(&self.claude_bin, model, timeout, prompt)
    }
}

/// One `"You"`/`"Them"` -> real-name mapping the model inferred from
/// transcript context. Only ever constructed for a line the model marked
/// `(inferred)` -- see `parse_names` -- so every `NameMapping` that exists
/// is, by construction, an inferred one; there is no other kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMapping {
    pub label: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSummary {
    pub summary: String,
    pub names: Vec<NameMapping>,
    pub facts: Vec<String>,
}

/// Outcome of parsing one sonnet reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Parsed(ParsedSummary),
    /// The reply didn't carry both a SUMMARY and a FACTS section in the
    /// exact format asked for -- the caller saves `raw` verbatim as
    /// `summary.md` and files zero facts (never invents structure that
    /// isn't there, never drops the model's actual words either).
    Fallback(String),
}

fn format_duration(secs: u64) -> String {
    format!("{}m{:02}s", secs / 60, secs % 60)
}

/// Caps `transcript` at `TRANSCRIPT_CAP_BYTES`, tail-biased (keeps the
/// *end* of the meeting, on the theory that decisions/action items tend to
/// land late) with a leading note so the model knows it's seeing a partial
/// transcript rather than silently reasoning over a truncated one. A
/// no-op when already under the cap. Splits on a UTF-8 char boundary so a
/// multi-byte character straddling the cut point is never sliced in half.
pub fn cap_transcript(transcript: &str) -> String {
    if transcript.len() <= TRANSCRIPT_CAP_BYTES {
        return transcript.to_string();
    }
    let mut start = transcript.len() - TRANSCRIPT_CAP_BYTES;
    while start < transcript.len() && !transcript.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[transcript truncated -- showing roughly the final {}KB of a longer meeting; earlier \
         portions were dropped for length]\n\n{}",
        TRANSCRIPT_CAP_BYTES / 1024,
        &transcript[start..]
    )
}

/// Builds the summarize/label/extract prompt: meeting metadata, the
/// (capped) transcript, and the exact STRICT-section output format
/// `parse_summary_output` expects back.
pub fn build_prompt(meeting: &Meeting, transcript: &str) -> String {
    let capped = cap_transcript(transcript);
    let title = meeting.title.as_deref().unwrap_or("untitled meeting");
    let mode_desc = match meeting.mode {
        Mode::Dual => "dual-track (\"You\" = mic, \"Them\" = the other participant(s), captured via system audio)",
        Mode::Solo => "solo (mic only -- a voice memo, not a two-party conversation)",
    };
    let duration = meeting.duration_secs.map(format_duration).unwrap_or_else(|| "unknown".to_string());

    let mut s = String::new();
    s.push_str("You are summarizing a recorded meeting transcript for a personal knowledge system.\n\n");
    s.push_str("Meeting metadata:\n");
    s.push_str(&format!("- Date/time started: {}\n", meeting.started_at));
    s.push_str(&format!("- Title: {}\n", title));
    s.push_str(&format!("- Mode: {}\n", mode_desc));
    s.push_str(&format!("- Duration: {}\n\n", duration));
    s.push_str("Transcript:\n");
    s.push_str(&capped);
    s.push_str("\n\n");
    s.push_str(
        "Produce your analysis in EXACTLY this format -- these three section headers, each on \
         their own line, nothing before the first header and nothing after the last section:\n\n\
         SUMMARY:\n\
         <topics discussed, decisions made, and action items with owners, as prose or a short list>\n\n\
         NAMES:\n\
         <map \"You\"/\"Them\" to real names ONLY when the transcript context makes it clear who they \
         are -- one line per mapping, exactly \"You = <name> (inferred)\" or \"Them = <name> \
         (inferred)\". NEVER invent a name you cannot support from the transcript. If no name can be \
         inferred for either, write exactly \"none\".>\n\n\
         FACTS:\n\
         <up to 8 durable facts worth remembering long-term from this meeting, each a self-contained \
         bullet point starting with \"-\", each naming the meeting's date so it still makes sense out \
         of context. ATTRIBUTE statements to people by name inside the fact itself whenever the \
         transcript supports it -- \"Moses said the Q4 report should focus on retention (2026-09-05 \
         meeting)\", \"Ivar committed to shipping the RHI redesign\" -- appending \"(name inferred)\" \
         when the identification comes from context rather than an explicit introduction. A fact that \
         says who said it is worth far more to a future reader than an anonymous one; fall back to \
         \"the user\"/\"the other participant\" only when no name is supportable. If nothing here is \
         durable enough to remember, write exactly \"none\".>\n",
    );
    s
}

/// Recognizes a header line -- `SUMMARY`, `NAMES`, or `FACTS` -- tolerating
/// markdown decoration (`## SUMMARY`, `**SUMMARY:**`) a model might add
/// despite being told the exact format, but requiring the WHOLE line
/// (after stripping that decoration) to be just the header word: a line
/// that merely *mentions* "summary" in prose must never be mistaken for
/// the section boundary.
fn normalize_header(line: &str) -> Option<&'static str> {
    let s = line.trim();
    let s = s.trim_matches(|c: char| c == '#' || c == '*' || c.is_whitespace());
    let s = s.trim_end_matches(':').trim();
    match s.to_uppercase().as_str() {
        "SUMMARY" => Some("SUMMARY"),
        "NAMES" => Some("NAMES"),
        "FACTS" => Some("FACTS"),
        _ => None,
    }
}

fn join_trim(lines: &[&str]) -> String {
    lines.join("\n").trim().to_string()
}

/// Case-insensitively removes the first occurrence of `needle` from `s`
/// (`needle` is plain ASCII, so byte-length-preserving slicing is safe).
fn strip_ci(s: &str, needle: &str) -> String {
    let lower = s.to_lowercase();
    match lower.find(&needle.to_lowercase()) {
        Some(idx) => format!("{}{}", &s[..idx], &s[idx + needle.len()..]),
        None => s.to_string(),
    }
}

/// Parses the NAMES section body. Empty or an explicit `none` -> no
/// mappings (the ordinary case for a solo meeting, or one where no name
/// surfaced). Otherwise, each non-empty line must be `<You|Them> = <name>`
/// AND contain `(inferred)` (case-insensitive) -- a line missing that
/// marker is silently dropped rather than accepted as an unmarked guess,
/// per the task's "never invent" rule: the marker isn't decoration, it's
/// the only thing distinguishing an evidenced inference from a fabrication.
fn parse_names(text: &str) -> Vec<NameMapping> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in t.lines() {
        let line = raw_line.trim().trim_start_matches(['-', '*', '\u{2022}']).trim();
        if line.is_empty() {
            continue;
        }
        let Some((label_part, value_part)) = line.split_once('=') else { continue };
        let label = label_part.trim();
        let label_norm = if label.eq_ignore_ascii_case("you") {
            "You"
        } else if label.eq_ignore_ascii_case("them") {
            "Them"
        } else {
            continue;
        };
        let value = value_part.trim();
        if !value.to_lowercase().contains("(inferred)") {
            continue;
        }
        let name = strip_ci(value, "(inferred)");
        let name = name.trim().trim_end_matches(['.', ',']).trim().to_string();
        if name.is_empty() {
            continue;
        }
        out.push(NameMapping { label: label_norm.to_string(), name });
    }
    out
}

/// Strips a leading list marker (`-`, `*`, `•`, or `N.`/`N)` numbering) --
/// same tolerant shape as `kb::reflect`'s own `strip_list_marker`.
fn strip_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start_matches(['-', '*', '\u{2022}']).trim_start();
    if let Some(rest) = trimmed.split_once('.').or_else(|| trimmed.split_once(')')) {
        if !rest.0.is_empty() && rest.0.chars().all(|c| c.is_ascii_digit()) {
            return rest.1.trim_start();
        }
    }
    trimmed
}

/// Parses the FACTS section body. Empty or an explicit `none` -> no facts
/// (not a failure -- a short/uneventful meeting can genuinely have nothing
/// durable to file). Capped at `MAX_FACTS`, keeping the first ones in the
/// model's own order if it ignored the "up to 8" instruction.
fn parse_facts(text: &str) -> Vec<String> {
    let t = text.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw_line in t.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let stripped = strip_list_marker(line);
        if stripped.is_empty() {
            continue;
        }
        out.push(stripped.to_string());
        if out.len() >= MAX_FACTS {
            break;
        }
    }
    out
}

/// Parses a sonnet reply per the exact format `build_prompt` asks for.
/// Requires both a SUMMARY and a FACTS header to be present as their own
/// line somewhere in the output (in either order, tolerating any leading
/// chatter before the first one) -- missing either is treated as a
/// malformed reply (`Fallback`), NAMES is optional (a reply that never
/// mentions it -- or writes "none" -- yields zero mappings either way).
pub fn parse_summary_output(raw: &str) -> ParseOutcome {
    let lines: Vec<&str> = raw.lines().collect();
    let mut summary_lines: Vec<&str> = Vec::new();
    let mut names_lines: Vec<&str> = Vec::new();
    let mut facts_lines: Vec<&str> = Vec::new();
    let mut current: Option<&'static str> = None;
    let mut saw_summary = false;
    let mut saw_facts = false;

    for line in &lines {
        if let Some(h) = normalize_header(line) {
            current = Some(h);
            match h {
                "SUMMARY" => saw_summary = true,
                "FACTS" => saw_facts = true,
                _ => {}
            }
            continue;
        }
        match current {
            Some("SUMMARY") => summary_lines.push(line),
            Some("NAMES") => names_lines.push(line),
            Some("FACTS") => facts_lines.push(line),
            _ => {}
        }
    }

    if !saw_summary || !saw_facts {
        return ParseOutcome::Fallback(raw.trim().to_string());
    }

    let summary = join_trim(&summary_lines);
    let names_raw = join_trim(&names_lines);
    let facts_raw = join_trim(&facts_lines);

    ParseOutcome::Parsed(ParsedSummary { summary, names: parse_names(&names_raw), facts: parse_facts(&facts_raw) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meeting(mode: Mode, duration: Option<u64>) -> Meeting {
        Meeting {
            started_at: "2026-09-07T17:33:00Z".to_string(),
            ended_at: Some("2026-09-07T18:03:00Z".to_string()),
            mode,
            title: Some("Weekly Standup".to_string()),
            sample_rate: 48000,
            capture_binary: "pw-record".to_string(),
            pids: crate::meeting::Pids { mic: 1, system: None },
            tracks: vec![],
            duration_secs: duration,
        }
    }

    // --- cap_transcript ---

    #[test]
    fn cap_transcript_is_a_noop_under_the_cap() {
        let short = "hello world";
        assert_eq!(cap_transcript(short), short);
    }

    #[test]
    fn cap_transcript_keeps_the_tail_and_adds_a_note_when_over_cap() {
        // Comfortably larger than the cap so the leading note (a few
        // hundred bytes) is negligible against what got dropped -- a
        // transcript merely a handful of bytes over the cap can legitimately
        // come back slightly LONGER once the note is added; that's not what
        // this test is checking.
        let big = "x".repeat(TRANSCRIPT_CAP_BYTES * 10);
        let capped = cap_transcript(&big);
        assert!(capped.starts_with("[transcript truncated"));
        assert!(capped.ends_with(&"x".repeat(50)));
        assert!(capped.len() < big.len());
    }

    #[test]
    fn cap_transcript_never_splits_a_multibyte_char() {
        // A transcript made entirely of a 3-byte UTF-8 character (e) around
        // the cut boundary -- the result must still be valid UTF-8.
        let big: String = std::iter::repeat('e').take(TRANSCRIPT_CAP_BYTES + 10).collect();
        let capped = cap_transcript(&big);
        assert!(capped.is_char_boundary(0));
        assert!(std::str::from_utf8(capped.as_bytes()).is_ok());
    }

    // --- build_prompt ---

    #[test]
    fn build_prompt_includes_metadata_transcript_and_headers() {
        let m = sample_meeting(Mode::Dual, Some(1800));
        let p = build_prompt(&m, "hello there");
        assert!(p.contains("Weekly Standup"));
        assert!(p.contains("2026-09-07T17:33:00Z"));
        assert!(p.contains("30m00s"));
        assert!(p.contains("hello there"));
        assert!(p.contains("SUMMARY:"));
        assert!(p.contains("NAMES:"));
        assert!(p.contains("FACTS:"));
    }

    #[test]
    fn build_prompt_untitled_meeting_uses_fallback_title() {
        let mut m = sample_meeting(Mode::Solo, None);
        m.title = None;
        let p = build_prompt(&m, "text");
        assert!(p.contains("untitled meeting"));
        assert!(p.contains("unknown")); // duration
    }

    // --- parse_summary_output: well-formed ---

    #[test]
    fn parse_summary_output_well_formed_with_names_and_facts() {
        let raw = "SUMMARY:\n\
                    Discussed Q3 roadmap and agreed to ship the export feature by Friday.\n\n\
                    NAMES:\n\
                    Them = Alice Chen (inferred)\n\n\
                    FACTS:\n\
                    - On 2026-09-07, the team agreed to ship the export feature by Friday.\n\
                    - Alice Chen owns the export feature rollout.\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert!(p.summary.contains("Q3 roadmap"));
                assert_eq!(p.names, vec![NameMapping { label: "Them".to_string(), name: "Alice Chen".to_string() }]);
                assert_eq!(p.facts.len(), 2);
                assert!(p.facts[0].contains("2026-09-07"));
                assert!(p.facts[1].contains("Alice Chen owns"));
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_summary_output_tolerates_markdown_decorated_headers() {
        let raw = "## SUMMARY\nBrief chat.\n\n**NAMES:**\nnone\n\n### FACTS ###\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert_eq!(p.summary, "Brief chat.");
                assert!(p.names.is_empty());
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_summary_output_names_section_of_none_yields_no_mappings() {
        let raw = "SUMMARY:\nShort call.\n\nNAMES:\nnone\n\nFACTS:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert!(p.names.is_empty());
                assert!(p.facts.is_empty());
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_summary_output_missing_names_section_entirely_is_still_parsed() {
        // NAMES is optional -- a model that skips it (or this fixture
        // simulating that) must not fall back, only SUMMARY/FACTS are
        // required.
        let raw = "SUMMARY:\nShort call.\n\nFACTS:\n- Met on 2026-09-07.\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert!(p.names.is_empty());
                assert_eq!(p.facts, vec!["Met on 2026-09-07.".to_string()]);
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    // --- parse_summary_output: names inference marking ---

    #[test]
    fn parse_names_drops_a_mapping_missing_the_inferred_marker() {
        // No "(inferred)" -- must never be accepted as a real mapping,
        // exactly the "never invent" rule this format exists to enforce.
        let raw = "SUMMARY:\ns\n\nNAMES:\nThem = Bob\n\nFACTS:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => assert!(p.names.is_empty()),
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_names_accepts_both_you_and_them_and_strips_the_marker() {
        let raw = "SUMMARY:\ns\n\nNAMES:\nYou = Ivan (inferred)\nThem = Priya Patel (inferred)\n\nFACTS:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert_eq!(
                    p.names,
                    vec![
                        NameMapping { label: "You".to_string(), name: "Ivan".to_string() },
                        NameMapping { label: "Them".to_string(), name: "Priya Patel".to_string() },
                    ]
                );
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_names_is_case_insensitive_on_label_and_marker() {
        let raw = "SUMMARY:\ns\n\nNAMES:\nthem = Sam (INFERRED)\n\nFACTS:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => {
                assert_eq!(p.names, vec![NameMapping { label: "Them".to_string(), name: "Sam".to_string() }]);
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    #[test]
    fn parse_names_ignores_a_line_with_an_unrecognized_label() {
        let raw = "SUMMARY:\ns\n\nNAMES:\nSomeone = Bob (inferred)\n\nFACTS:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Parsed(p) => assert!(p.names.is_empty()),
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    // --- parse_summary_output: malformed ---

    #[test]
    fn parse_summary_output_falls_back_on_missing_headers_entirely() {
        let raw = "Just some prose the model produced with no structure at all.";
        assert_eq!(parse_summary_output(raw), ParseOutcome::Fallback(raw.to_string()));
    }

    #[test]
    fn parse_summary_output_falls_back_when_facts_header_is_missing() {
        let raw = "SUMMARY:\nA fine summary.\n\nNAMES:\nnone\n";
        match parse_summary_output(raw) {
            ParseOutcome::Fallback(text) => assert!(text.contains("A fine summary.")),
            other => panic!("expected Fallback, got {:?}", other),
        }
    }

    #[test]
    fn parse_summary_output_falls_back_when_summary_header_is_missing() {
        let raw = "NAMES:\nnone\n\nFACTS:\n- a fact\n";
        assert!(matches!(parse_summary_output(raw), ParseOutcome::Fallback(_)));
    }

    #[test]
    fn parse_summary_output_falls_back_on_empty_output() {
        assert_eq!(parse_summary_output(""), ParseOutcome::Fallback(String::new()));
    }

    // --- facts cap ---

    #[test]
    fn parse_summary_output_caps_facts_at_eight() {
        let mut raw = String::from("SUMMARY:\ns\n\nFACTS:\n");
        for i in 1..=12 {
            raw.push_str(&format!("- fact number {}\n", i));
        }
        match parse_summary_output(&raw) {
            ParseOutcome::Parsed(p) => {
                assert_eq!(p.facts.len(), 8);
                assert_eq!(p.facts[0], "fact number 1");
                assert_eq!(p.facts[7], "fact number 8");
            }
            other => panic!("expected Parsed, got {:?}", other),
        }
    }

    // --- fake LLM ---

    pub struct FakeClaudeLlm {
        pub reply: String,
    }

    impl ClaudeLlm for FakeClaudeLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn fake_llm_round_trips_through_call() {
        let llm = FakeClaudeLlm { reply: "SUMMARY:\ns\n\nFACTS:\nnone\n".to_string() };
        let out = llm.call(CLAUDE_MODEL, "prompt", CLAUDE_TIMEOUT).unwrap();
        assert!(matches!(parse_summary_output(&out), ParseOutcome::Parsed(_)));
    }
}
