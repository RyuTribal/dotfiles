
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
use std::io::{self, IsTerminal, Read};
use std::process::Command;
use std::time::Duration;

use crate::classify::run_claude;
use crate::embed::{Embedder, OllamaEmbedder};
use crate::store::{self, KbError};

const TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_IMPORTANCE: i64 = 6;

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

/// The result of classifying one note: one topic for the whole note, a
/// short referable title, and 1-N self-contained durable facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub topic: String,
    pub title: String,
    pub facts: Vec<String>,
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
    let mut topic: Option<String> = None;
    let mut title: Option<String> = None;
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
        if let Some(rest) = strip_prefix_ci(line, "FACT:") {
            let fact = rest.trim();
            if !fact.is_empty() {
                facts.push(fact.to_string());
            }
            continue;
        }
    }

    match (topic, title) {
        (Some(topic), Some(title)) if !facts.is_empty() => Classification { topic, title, facts },
        _ => fallback_classification(note),
    }
}

/// The "never lose the note" fallback: stores the raw note verbatim as one
/// fact, topic "notes", title built from its first few words.
fn fallback_classification(note: &str) -> Classification {
    let trimmed = note.trim();
    let title: String = trimmed.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
    Classification {
        topic: "notes".to_string(),
        title: if title.is_empty() { "untitled note".to_string() } else { title },
        facts: vec![trimmed.to_string()],
    }
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
    println!("       note \"<text>\" [-i N]       same, via the `note` symlink");
    println!();
    println!("  -i, --importance N   1-10, default 6");
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

/// Runs `mach note ...` (also reachable as `note ...` via the argv0
/// dispatch in `mach`'s main). Content comes from a positional argument, or
/// — bare `mach note` — from stdin if piped, else `$EDITOR`.
pub fn run(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut content_arg: Option<String> = None;
    let mut importance: i64 = DEFAULT_IMPORTANCE;

    while let Some(a) = args.next() {
        match a.as_str() {
            "-i" | "--importance" => {
                importance = args.next().and_then(|v| v.parse::<i64>().ok()).unwrap_or(DEFAULT_IMPORTANCE).clamp(1, 10);
            }
            "-h" | "--help" => {
                print_help();
                return Ok(());
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

    let raw = match content_arg {
        Some(c) => c,
        None if !io::stdin().is_terminal() => {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf)?;
            buf
        }
        None => read_from_editor()?,
    };

    let note = raw.trim().to_string();
    if note.is_empty() {
        eprintln!("mach note: nothing to save — empty note");
        std::process::exit(1);
    }

    let conn = store::open().map_err(to_io)?;
    let existing_topics = store::distinct_projects(&conn).map_err(to_io)?;
    let today = store::now_rfc3339();
    let today = today.get(0..10).unwrap_or(&today);

    let llm = ProcessNoteLlm::new();
    // A failed classifier call (spawn error, timeout, non-zero exit) yields
    // empty output here, which `parse_classification` treats exactly like a
    // malformed reply — same "never lose the note" fallback path either way.
    let output = llm.call(&build_prompt(&note, &existing_topics, today)).unwrap_or_default();
    let classification = parse_classification(&output, &note);

    let slug = slugify(&classification.title);
    let source = format!("note:{}", slug);

    let embedder = OllamaEmbedder::new();
    let mut any_missing_embedding = false;
    for fact in &classification.facts {
        let embedding = match embedder.embed(fact) {
            Ok(v) => Some(v),
            Err(_) => {
                any_missing_embedding = true;
                None
            }
        };
        // Direct store call, not `mach kb add`'s subprocess/classifier
        // path: a note is fresh, deliberate input (reviewed, not
        // unreviewed) and the save-time supersession classifier is
        // deliberately skipped — that's `mach kb reflect`'s job later.
        store::insert(&conn, fact, Some(&source), Some(&classification.topic), true, embedding.as_deref(), importance)
            .map_err(to_io)?;
    }

    if any_missing_embedding {
        println!(
            "warning: could not reach ollama for embeddings — stored without them; \
             recall will find these by keyword only until re-embedded"
        );
    }

    let n = classification.facts.len();
    println!("filed {} {} under \"{}\"", n, if n == 1 { "memory" } else { "memories" }, classification.topic);
    println!("  refer to it as: \"{}\"", classification.title);
    for fact in &classification.facts {
        println!("  - {}", fact);
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
}
