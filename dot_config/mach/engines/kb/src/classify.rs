
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
         UPDATE <id> — the new fact refines memory <id> about the same thing (keep both, as history).\n\
         SUPERSEDE <id> — the new fact replaces memory <id>, which is now obsolete or contradicted.\n\
         NOOP — the new fact is a duplicate that adds nothing.\n\
         ADD — the new fact is genuinely new information.\n\
         \n\
         SUPERSEDE only when the new fact gives a different value for the same attribute of the \
         same subject, so the old statement is now false.\n\
         Two facts about the same project or person but different attributes (for example a \
         colour decision and a font decision) are ADD, not SUPERSEDE.\n\
         A fact anchored to a date or period (\"on 2026-09-07\", \"as of 2026-09-09\", \"shipped \
         2026-08-31\") is history: never SUPERSEDE it; use ADD.\n\
         When unsure, ADD.\n",
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
        // No built-in tools at all. Every caller passes its material in
        // the prompt; none needs to touch the filesystem. `--disallowedTools`
        // alone left Read/Grep/Glob enabled, so a prompt-injected model
        // (these prompts embed memories, transcripts and code) could read
        // any file the user can, secrets included.
        .arg("--tools")
        .arg("")
        // Kept as defence in depth (harmless next to `--tools ""`).
        .arg("--disallowedTools")
        .arg("Bash Edit Write NotebookEdit WebFetch WebSearch Agent")
        // No MCP servers. Every tool is already disallowed above, so a
        // spawned call can never reach one, yet the CLI still connects each
        // configured server on startup. Measured over interleaved pairs this
        // buys no wall time (server connect overlaps model latency) but
        // halves client CPU per call, ~3.1s to ~1.5s user. Kept for the
        // isolation as much as the CPU: a chore subprocess has no business
        // holding connections to Asana, Blender or a browser.
        .arg("--strict-mcp-config")
        .arg("--mcp-config")
        .arg("{\"mcpServers\":{}}")
        // Load no user/project/local settings.json at all. Without this,
        // the CLI still loads the user's own Claude Code settings for a
        // spawned `-p` call -- including hooks -- and a hook there (a
        // "caveman" terse-speech style hook, in this user's case) rewrites
        // the model's replies into telegraphic fragments before they ever
        // reach us. That corrupts every stored answer (memories, note
        // descriptions, digests): the classifier/reflect/note text this
        // function returns gets saved verbatim. Hooks are already skipped
        // via `MACH_KB_DIGEST=1` below as a second guard, but that only
        // covers *this repo's own* hooks -- it does nothing about other
        // style/behavior hooks the user has configured, which is exactly
        // what bit us. OAuth login still works with no settings loaded
        // (verified with real calls), so this costs nothing but the
        // inherited hooks/permissions/env the settings files would add.
        .arg("--setting-sources=")
        // Read by the kb hooks (kb-recall, kb-model, kb-capture,
        // kb-checkpoint, kb-decision, kb-pretool-recall), which all exit
        // early on it: a chore subprocess must not be handed recalled
        // memories, and must not be ingested as if it were a user session.
        .env("MACH_KB_DIGEST", "1");
    detach_from_session_proxy(&mut cmd);
    run_with_stdin(cmd, timeout, prompt)
}

/// Drops `ANTHROPIC_BASE_URL` from a spawned `claude`'s environment so it
/// talks to the API directly, the same way the systemd-run jobs do.
/// Inside an interactive Claude Code session that variable points at a
/// session-local proxy (caveman-proxy on 127.0.0.1:8787) which exits after
/// 30 minutes without interactive activity -- traffic from these chore
/// calls does not keep it alive -- and every call made while it is down
/// fails with "Connection error" until the CLI's retries run out. A manual
/// `mach kb index` started from a session then stalls for as long as the
/// session is idle.
pub fn detach_from_session_proxy(cmd: &mut Command) {
    cmd.env_remove("ANTHROPIC_BASE_URL");
}

/// Spawns `cmd` with `prompt` on stdin and polls it to completion (or up to
/// `timeout`, killing it), returning stdout on a zero exit. The process
/// mechanics behind `run_claude`, shared with `improve::ProcessImproveLlm`,
/// whose agentic `claude -p` needs a different flag set (an allowlist of
/// file-editing tools instead of a denylist) but the same stdin/timeout
/// discipline.
/// The last `n` non-blank lines of `text`, joined with " | ". Keeps a
/// failure message to one line while preserving the part of a stderr dump
/// that actually says what went wrong (which is the end of it).
pub fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join(" | ")
}

pub fn run_with_stdin(mut cmd: Command, timeout: Duration, prompt: &str) -> Result<String, String> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Piped, not null: a failed unattended run used to leave only
        // "exited with Some(1)" behind, which names the failure without
        // saying anything about it.
        .stderr(Stdio::piped())
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
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                return if status.success() {
                    Ok(out)
                } else {
                    // Include stderr: "exited with Some(1)" names the
                    // failure without saying anything about it, and that is
                    // exactly what an unattended timer run leaves behind to
                    // debug from.
                    // The claude CLI prints API errors (rate limits, auth)
                    // to stdout, so fall back to it when stderr is empty.
                    let tail = last_lines(&err, 3);
                    let tail = if tail.is_empty() { last_lines(&out, 3) } else { tail };
                    if tail.is_empty() {
                        Err(format!("'{}' exited with {:?} (no output)", program, status.code()))
                    } else {
                        Err(format!("'{}' exited with {:?}: {}", program, status.code(), tail))
                    }
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
    fn spawned_claude_never_inherits_the_session_proxy_url() {
        let mut cmd = Command::new("claude");
        cmd.env("ANTHROPIC_BASE_URL", "http://127.0.0.1:8787/w/claude");
        detach_from_session_proxy(&mut cmd);
        let base_url = cmd.get_envs().find(|(k, _)| *k == "ANTHROPIC_BASE_URL").map(|(_, v)| v);
        assert_eq!(base_url, Some(None), "must be explicitly removed, not just left unset");
    }

    /// Security (final-review item 6): every `run_claude` subcall must run
    /// with NO built-in tools (`--tools ""`) -- Read/Grep/Glob would
    /// otherwise let a prompt-injected model read any file, secrets
    /// included. Captured from the real spawned argv via a stand-in
    /// `claude` script (no LLM involved).
    #[test]
    fn run_claude_spawns_with_no_builtin_tools() {
        let dir = std::env::temp_dir().join(format!("kb-argv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let argv_file = dir.join("argv");
        let script = dir.join("fake-claude");
        std::fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat >/dev/null\necho ok\n", argv_file.display())).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let out = run_claude(script.to_str().unwrap(), "haiku", Duration::from_secs(10), "prompt").unwrap();
        assert_eq!(out.trim(), "ok");
        let argv: Vec<String> = std::fs::read_to_string(&argv_file).unwrap().lines().map(str::to_string).collect();
        let i = argv.iter().position(|a| a == "--tools").expect("--tools flag present");
        assert_eq!(argv[i + 1], "", "--tools must be given the empty list");
        assert!(argv.iter().any(|a| a == "--disallowedTools"), "the denylist stays as defence in depth");
        assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
        assert!(
            argv.iter().any(|a| a == "--setting-sources="),
            "must load no user/project/local settings -- otherwise the user's own hooks (e.g. a \
             terse-speech style hook) reshape stored text"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    #[test]
    fn build_prompt_contains_different_attribute_rule() {
        let prompt = build_prompt("new fact text", &[(3, "old fact text".to_string())]);
        assert!(prompt.contains(
            "Two facts about the same project or person but different attributes (for example a \
             colour decision and a font decision) are ADD, not SUPERSEDE."
        ));
    }

    #[test]
    fn build_prompt_contains_dated_fact_rule() {
        let prompt = build_prompt("new fact text", &[(3, "old fact text".to_string())]);
        assert!(prompt.contains(
            "A fact anchored to a date or period (\"on 2026-09-07\", \"as of 2026-09-09\", \
             \"shipped 2026-08-31\") is history: never SUPERSEDE it; use ADD."
        ));
    }

    #[test]
    fn build_prompt_contains_when_unsure_rule() {
        let prompt = build_prompt("new fact text", &[(3, "old fact text".to_string())]);
        assert!(prompt.contains("When unsure, ADD."));
    }
}

#[cfg(test)]
mod stderr_tests {
    use super::last_lines;

    #[test]
    fn last_lines_keeps_the_informative_tail() {
        assert_eq!(last_lines("a\nb\nc\nd", 2), "c | d");
        assert_eq!(last_lines("only", 3), "only");
        assert_eq!(last_lines("  \n\n", 3), "", "blank stderr yields nothing to report");
        assert_eq!(last_lines("x\n\n  y  \n", 5), "x | y");
    }
}
