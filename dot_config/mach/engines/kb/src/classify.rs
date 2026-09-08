
//! Save-time supersession classifier (Mem0-classifier / Graphiti-tombstone
//! style): when `mach kb add` finds an existing memory close enough to the
//! new fact, this asks a small non-interactive `claude -p` call for a
//! one-word verdict — `ADD`, `UPDATE <id>`, `SUPERSEDE <id>`, or `NOOP` —
//! rather than silently duplicating or silently overwriting.
//!
//! Behind the `Classifier` trait so `store::apply_verdict`'s DB-mutation
//! logic can be tested against a fixed `Verdict` without spawning a real
//! process; `ProcessClassifier` is the real implementation used at runtime.
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Add,
    Update(i64),
    Supersede(i64),
    Noop,
}

pub trait Classifier {
    /// `new_fact` is the content being added; `similar` is the top-N
    /// existing memories it was found close to, as `(id, content)` pairs.
    fn classify(&self, new_fact: &str, similar: &[(i64, String)]) -> Verdict;
}

/// Shells out to `claude -p --model haiku`, mirroring the exact invocation
/// pattern `kb-capture.sh` already uses successfully for its own digest
/// call (same flags, same `MACH_KB_DIGEST=1` recursion guard — this call is
/// itself a non-interactive session, so it must not let its own
/// `SessionEnd` fire another digest/classifier call).
pub struct ProcessClassifier {
    claude_bin: String,
}

impl ProcessClassifier {
    /// Uses `CLAUDE_BIN` if set (same env var `kb-capture.sh` honors),
    /// otherwise plain `claude` from `PATH`.
    pub fn new() -> Self {
        ProcessClassifier {
            claude_bin: std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
        }
    }

    /// For tests: pin the binary to something that will fail to spawn (or
    /// a stub), without mutating process-wide env vars other tests share.
    #[cfg(test)]
    pub fn with_bin(bin: &str) -> Self {
        ProcessClassifier { claude_bin: bin.to_string() }
    }
}

impl Default for ProcessClassifier {
    fn default() -> Self {
        Self::new()
    }
}

fn build_prompt(new_fact: &str, similar: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are a memory-classifier for a personal knowledge bank. A new fact is about to be \
         saved. Compare it against similar existing memories and decide the single correct \
         action.\n\n",
    );
    s.push_str("New fact:\n");
    s.push_str(new_fact);
    s.push_str("\n\nSimilar existing memories:\n");
    for (id, content) in similar {
        s.push_str(&format!("#{}: {}\n", id, content));
    }
    s.push_str(
        "\nReply with exactly one line and no other text: ADD, UPDATE <id>, SUPERSEDE <id>, or NOOP.\n\
         UPDATE <id> — the new fact refines or corrects memory <id> about the same thing (keep both, as history).\n\
         SUPERSEDE <id> — the new fact replaces memory <id>, which is now obsolete or contradicted.\n\
         NOOP — the new fact is a duplicate that adds nothing.\n\
         ADD — the new fact is genuinely new information.\n",
    );
    s
}

/// Parses the classifier's reply into a `Verdict`. Scans line by line for
/// the first recognizable one of the four forms (tolerating stray
/// whitespace, a leading `#` on the id, and surrounding chatter); anything
/// that never matches — empty output, prose, a malformed id — falls back
/// to `Add`, matching the "never lose a fact to a flaky LLM" rule.
pub fn parse_verdict(output: &str) -> Verdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if upper == "ADD" {
            return Verdict::Add;
        }
        if upper == "NOOP" {
            return Verdict::Noop;
        }
        if let Some(rest) = upper.strip_prefix("UPDATE") {
            if let Some(id) = extract_id(rest) {
                return Verdict::Update(id);
            }
        }
        if let Some(rest) = upper.strip_prefix("SUPERSEDE") {
            if let Some(id) = extract_id(rest) {
                return Verdict::Supersede(id);
            }
        }
    }
    Verdict::Add
}

fn extract_id(rest: &str) -> Option<i64> {
    rest.trim().trim_start_matches('#').split_whitespace().next()?.parse().ok()
}

/// Runs a non-interactive `claude -p --model <model>` to completion (or up
/// to `timeout`), feeding `prompt` on stdin and returning stdout. Any
/// failure — spawn error, timeout, non-zero exit — is an `Err`. Shared by
/// `classify` (model "haiku", `TIMEOUT`) and `reflect` (model "haiku" or
/// "sonnet" per stage, its own timeouts) — same flags and the same
/// `MACH_KB_DIGEST=1` recursion guard `kb-capture.sh` established, since
/// every one of these calls is itself a non-interactive session whose own
/// `SessionEnd` must not fire another digest/classifier/reflect call.
/// Public (not `pub(crate)`) so `engines/meet`'s phase-B summarizer can
/// reuse the exact same invocation — same flags, same recursion guard —
/// for its own one-shot sonnet call rather than duplicating this function
/// across crates.
pub fn run_claude(claude_bin: &str, model: &str, timeout: Duration, prompt: &str) -> Result<String, String> {
    let mut cmd = Command::new(claude_bin);
    cmd.arg("-p")
        .arg("--model")
        .arg(model)
        .arg("--permission-prompts")
        .arg("none")
        .arg("--disallowedTools")
        .arg("Bash Edit Write NotebookEdit WebFetch WebSearch Agent")
        .env("MACH_KB_DIGEST", "1");
    run_with_stdin(cmd, timeout, prompt)
}

/// Spawns `cmd` with `prompt` on stdin and polls it to completion (or up to
/// `timeout`, killing it), returning stdout on a zero exit. The process
/// mechanics behind `run_claude`, shared with `improve::ProcessImproveLlm`,
/// whose agentic `claude -p` needs a different flag set (an allowlist of
/// file-editing tools instead of a denylist) but the same stdin/timeout
/// discipline.
pub fn run_with_stdin(mut cmd: Command, timeout: Duration, prompt: &str) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to spawn '{}': {}", program, e))?;

    if let Some(mut stdin) = child.stdin.take() {
        // Best-effort: if the child has already exited (or its stdin pipe
        // is otherwise gone), a write error here isn't fatal — the exit
        // status check below is what actually decides success/failure.
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
                return if status.success() {
                    Ok(out)
                } else {
                    Err(format!("'{}' exited with {:?}", program, status.code()))
                };
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("'{}' timed out after {:?}", program, timeout));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("error waiting on '{}': {}", program, e)),
        }
    }
}

impl Classifier for ProcessClassifier {
    fn classify(&self, new_fact: &str, similar: &[(i64, String)]) -> Verdict {
        let prompt = build_prompt(new_fact, similar);
        match run_claude(&self.claude_bin, "haiku", TIMEOUT, &prompt) {
            Ok(output) => parse_verdict(&output),
            Err(_) => Verdict::Add,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add() {
        assert_eq!(parse_verdict("ADD"), Verdict::Add);
        assert_eq!(parse_verdict("  add  \n"), Verdict::Add);
    }

    #[test]
    fn parses_noop() {
        assert_eq!(parse_verdict("NOOP"), Verdict::Noop);
        assert_eq!(parse_verdict("noop"), Verdict::Noop);
    }

    #[test]
    fn parses_update_with_id() {
        assert_eq!(parse_verdict("UPDATE 42"), Verdict::Update(42));
        assert_eq!(parse_verdict("update #42"), Verdict::Update(42));
    }

    #[test]
    fn parses_supersede_with_id() {
        assert_eq!(parse_verdict("SUPERSEDE 7"), Verdict::Supersede(7));
        assert_eq!(parse_verdict("Supersede #7\n"), Verdict::Supersede(7));
    }

    #[test]
    fn scans_past_leading_chatter_to_find_the_verdict_line() {
        let out = "Sure, here is my answer:\nUPDATE 5\n";
        assert_eq!(parse_verdict(out), Verdict::Update(5));
    }

    #[test]
    fn malformed_output_falls_back_to_add() {
        assert_eq!(parse_verdict(""), Verdict::Add);
        assert_eq!(parse_verdict("I think this is new information."), Verdict::Add);
        assert_eq!(parse_verdict("UPDATE"), Verdict::Add); // missing id
        assert_eq!(parse_verdict("UPDATE banana"), Verdict::Add); // non-numeric id
    }

    #[test]
    fn classifier_process_failure_falls_back_to_add() {
        // A binary that can't possibly exist — spawn() itself must fail,
        // and that failure must never propagate past `classify` as an
        // error; it degrades to a plain Add.
        let classifier = ProcessClassifier::with_bin("/nonexistent/definitely-not-a-binary-xyz");
        let verdict = classifier.classify("some fact", &[(1, "similar fact".to_string())]);
        assert_eq!(verdict, Verdict::Add);
    }

    #[test]
    fn build_prompt_includes_fact_and_similar_ids() {
        let prompt = build_prompt("new fact text", &[(3, "old fact text".to_string())]);
        assert!(prompt.contains("new fact text"));
        assert!(prompt.contains("#3: old fact text"));
        assert!(prompt.contains("NOOP"));
    }
}
