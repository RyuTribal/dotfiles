
//! `mach kb reflect` — the reflection subsystem: a periodic pass that reads
//! recently-added memories, asks a small model what higher-level questions
//! they might answer — about the user, a project or system, an engineering
//! practice or lesson, or a recurring pattern across work, not the user
//! alone — then asks a stronger model to state a durable insight for each
//! question when at least two independent memories back it up. A separate
//! re-verification pass samples existing insights and flags (never deletes)
//! ones whose evidence no longer holds.
//!
//! Behind the `ReflectLlm` trait so the pure parsing/prompt-building logic
//! here is unit-testable without spawning a real `claude` process — mirrors
//! `classify::Classifier`. `ProcessReflectLlm` is the real implementation,
//! reusing `classify::run_claude` (same invocation flags, same
//! `MACH_KB_DIGEST=1` recursion guard).
use std::time::Duration;

use crate::classify::run_claude;

/// Haiku calls (stage 1 questions, the re-verification contradiction check)
/// are short one-shot classifications — same budget `classify` gives its
/// single haiku call.
pub const TIMEOUT_HAIKU: Duration = Duration::from_secs(30);
/// Stage 2 (sonnet) is doing real reasoning over up to 8 evidence rows plus
/// existing insights, so it gets more headroom.
pub const TIMEOUT_SONNET: Duration = Duration::from_secs(60);

pub trait ReflectLlm {
    /// Runs `claude -p --model <model>` with `prompt` on stdin, returning
    /// stdout on success or an error string on any failure (spawn, timeout,
    /// non-zero exit).
    fn call(&self, model: &str, prompt: &str, timeout: Duration) -> Result<String, String>;
}

pub struct ProcessReflectLlm {
    claude_bin: String,
}

impl ProcessReflectLlm {
    /// Uses `CLAUDE_BIN` if set (same env var `kb-capture.sh`/`classify`
    /// honor), otherwise plain `claude` from `PATH`.
    pub fn new() -> Self {
        ProcessReflectLlm { claude_bin: std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()) }
    }
}

impl Default for ProcessReflectLlm {
    fn default() -> Self {
        Self::new()
    }
}

impl ReflectLlm for ProcessReflectLlm {
    fn call(&self, model: &str, prompt: &str, timeout: Duration) -> Result<String, String> {
        run_claude(&self.claude_bin, model, timeout, prompt)
    }
}

// --- stage 1: salient questions ---

/// Builds the stage-1 prompt: the working set as `id: content` lines,
/// asking for 2-3 higher-level questions these statements could answer —
/// about the user, a project or system, an engineering practice, or a
/// recurring pattern, not the user alone.
pub fn build_questions_prompt(working_set: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are analyzing a personal knowledge bank to find deeper patterns. \
         Here are recent statements from a personal knowledge bank (id: content):\n\n",
    );
    for (id, content) in working_set {
        s.push_str(&format!("{}: {}\n", id, content));
    }
    s.push_str(
        "\nWhat are the 2-3 most salient higher-level questions these statements \
         could answer -- about the user's preferences, their projects and systems, \
         engineering practices and lessons, or recurring patterns across work? \
         One per line, nothing else.\n",
    );
    s
}

/// Parses stage-1 output into up to 3 non-empty questions, tolerating
/// common leading list markers (`1.`, `-`, `*`) a model might add despite
/// being told not to.
pub fn parse_questions(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(strip_list_marker)
        .filter(|l| !l.is_empty())
        .take(3)
        .map(String::from)
        .collect()
}

fn strip_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start_matches(['-', '*', '•']).trim_start();
    // "1. " / "1) " style numbering
    if let Some(rest) = trimmed.split_once('.').or_else(|| trimmed.split_once(')')) {
        if !rest.0.is_empty() && rest.0.chars().all(|c| c.is_ascii_digit()) {
            return rest.1.trim_start();
        }
    }
    trimmed
}

// --- stage 2: durable insight per question ---

/// Builds the stage-2 prompt: evidence rows as `[id] content`, existing
/// non-duplicate insights as `[i<id>] text`, the question, and the exact
/// output-format instructions.
pub fn build_insight_prompt(question: &str, evidence: &[(i64, String)], existing_insights: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str("Evidence (knowledge-bank memories):\n");
    for (id, content) in evidence {
        s.push_str(&format!("[{}] {}\n", id, content));
    }
    if !existing_insights.is_empty() {
        s.push_str("\nExisting insights already recorded (build on these, do not duplicate):\n");
        for (id, text) in existing_insights {
            s.push_str(&format!("[i{}] {}\n", id, text));
        }
    }
    s.push_str(&format!(
        "\nQuestion: {}\n\n\
         State ONE durable higher-level insight the evidence supports -- it may be \
         about the user, a project, a technical practice, or a recurring pattern -- \
         ONLY IF at least 2 independent evidence rows support it. Format exactly: `<insight text> (because of: <id>, <id>[, ...])`. \
         If evidence is insufficient or the insight would duplicate an existing [i*] insight, \
         output exactly NONE.\n",
        question
    ));
    s
}

/// Outcome of parsing a stage-2 reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage2Result {
    /// A well-formed insight with `>= 2` distinct raw-memory citations.
    Insight { text: String, memory_ids: Vec<i64> },
    /// The model explicitly declined (output was `NONE`).
    None,
    /// Anything else: empty output, no recognizable citation line,
    /// malformed citation tokens, or fewer than 2 raw-memory ids cited.
    Rejected,
}

/// Parses a stage-2 reply per the exact format instructed in
/// `build_insight_prompt`. Scans line by line (tolerating leading chatter,
/// like `classify::parse_verdict` does) for either a bare `NONE` or a line
/// ending in `(because of: <id>[, <id>...])`.
///
/// Citation tokens come in two forms: a raw memory id (`12`, `#12`) which
/// counts toward the `>= 2` evidence floor, or an insight reference (`i7`,
/// `I7`) which is accepted as a citation but never counts toward that
/// floor. Any token that is neither form makes the whole line malformed —
/// rejected outright rather than silently dropping the bad token, since a
/// citation list is only as trustworthy as its worst entry.
pub fn parse_stage2(output: &str) -> Stage2Result {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("none") {
            return Stage2Result::None;
        }
        if !line.ends_with(')') {
            continue;
        }
        let marker = "(because of:";
        let open = match find_ignore_case(line, marker) {
            Some(i) => i,
            None => continue,
        };
        let text = line[..open].trim().trim_matches('`').trim();
        if text.is_empty() {
            continue;
        }
        let inner = &line[open + marker.len()..line.len() - 1];
        let tokens: Vec<&str> = inner.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
        if tokens.is_empty() {
            return Stage2Result::Rejected;
        }

        let mut memory_ids: Vec<i64> = Vec::new();
        for tok in &tokens {
            let stripped = tok.trim_start_matches('#');
            if let Some(rest) = stripped.strip_prefix('i').or_else(|| stripped.strip_prefix('I')) {
                if rest.parse::<i64>().is_err() {
                    return Stage2Result::Rejected;
                }
                // insight reference — valid citation, doesn't count toward the floor
                continue;
            }
            match stripped.parse::<i64>() {
                Ok(id) => memory_ids.push(id),
                Err(_) => return Stage2Result::Rejected,
            }
        }
        memory_ids.sort_unstable();
        memory_ids.dedup();
        if memory_ids.len() < 2 {
            return Stage2Result::Rejected;
        }
        return Stage2Result::Insight { text: text.to_string(), memory_ids };
    }
    Stage2Result::Rejected
}

fn find_ignore_case(haystack: &str, needle: &str) -> Option<usize> {
    let hay_lower = haystack.to_lowercase();
    let needle_lower = needle.to_lowercase();
    hay_lower.rfind(&needle_lower)
}

// --- re-verification: contradiction check ---

/// Builds the one-haiku-call contradiction check: the insight's claim text
/// plus up to 5 candidate active memories (fresh top-5 similarity search
/// over the insight text, not necessarily its original citations).
pub fn build_contradiction_prompt(claim: &str, candidates: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str("Claim under review:\n");
    s.push_str(claim);
    s.push_str("\n\nCandidate memories:\n");
    for (id, content) in candidates {
        s.push_str(&format!("[{}] {}\n", id, content));
    }
    s.push_str(
        "\nDoes any of these contradict the claim? Reply with exactly one line and nothing \
         else: \"YES <id>\" naming the first one that contradicts, or \"NO\".\n",
    );
    s
}

/// Parses the contradiction check's reply. `Some(id)` on a well-formed
/// "YES <id>"; `None` for "NO" or anything unparsable — an ambiguous or
/// malformed reply defaults to "no contradiction found," matching the
/// store-wide rule of never letting a flaky LLM call cost real data (here:
/// spuriously flagging a sound insight).
pub fn parse_contradiction(output: &str) -> Option<i64> {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if upper == "NO" {
            return None;
        }
        if let Some(rest) = upper.strip_prefix("YES") {
            if let Some(id) = rest.trim().trim_start_matches('#').split_whitespace().next().and_then(|t| t.parse().ok())
            {
                return Some(id);
            }
        }
    }
    None
}

/// Confidence for a freshly-accepted insight: `0.5` base, `+0.1` per
/// evidence row beyond the 2-row floor, capped at `0.9`.
pub fn compute_confidence(evidence_count: usize) -> f64 {
    let extra = evidence_count.saturating_sub(2) as f64;
    (0.5 + 0.1 * extra).min(0.9)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- stage 1 ---

    #[test]
    fn parse_questions_takes_up_to_three_nonempty_lines() {
        let out = "What does the user do for work?\nWhat are their hobbies?\nWhat tools do they prefer?\nA fourth question?\n";
        let qs = parse_questions(out);
        assert_eq!(qs.len(), 3);
        assert_eq!(qs[0], "What does the user do for work?");
        assert_eq!(qs[2], "What tools do they prefer?");
    }

    #[test]
    fn parse_questions_strips_common_list_markers() {
        let out = "1. What does the user do?\n- What do they like?\n* A third one?\n";
        let qs = parse_questions(out);
        assert_eq!(qs, vec!["What does the user do?", "What do they like?", "A third one?"]);
    }

    #[test]
    fn parse_questions_ignores_blank_lines() {
        let out = "\n\nOnly one real question?\n\n";
        let qs = parse_questions(out);
        assert_eq!(qs, vec!["Only one real question?"]);
    }

    // --- stage 2 parser: the core of the spec ---

    #[test]
    fn parse_stage2_accepts_valid_two_citation_insight() {
        let out = "The user is a backend-leaning Rust developer (because of: 12, 45)";
        match parse_stage2(out) {
            Stage2Result::Insight { text, memory_ids } => {
                assert_eq!(text, "The user is a backend-leaning Rust developer");
                assert_eq!(memory_ids, vec![12, 45]);
            }
            other => panic!("expected Insight, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_accepts_more_than_two_citations() {
        let out = "The user works late at night (because of: 1, 2, 3, 4)";
        match parse_stage2(out) {
            Stage2Result::Insight { memory_ids, .. } => assert_eq!(memory_ids, vec![1, 2, 3, 4]),
            other => panic!("expected Insight, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_tolerates_hash_prefixed_ids_and_backticks() {
        let out = "`The user likes tea` (because of: #3, #9)";
        match parse_stage2(out) {
            Stage2Result::Insight { text, memory_ids } => {
                assert_eq!(text, "The user likes tea");
                assert_eq!(memory_ids, vec![3, 9]);
            }
            other => panic!("expected Insight, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_rejects_none() {
        assert_eq!(parse_stage2("NONE"), Stage2Result::None);
        assert_eq!(parse_stage2("  none  \n"), Stage2Result::None);
    }

    #[test]
    fn parse_stage2_rejects_sub_floor_single_citation() {
        assert_eq!(parse_stage2("The user likes cats (because of: 12)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_rejects_duplicate_citation_collapsing_below_floor() {
        // "12, 12" is syntactically two tokens but only one distinct memory
        // — not independent evidence, so it must not clear the floor.
        assert_eq!(parse_stage2("The user likes cats (because of: 12, 12)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_insight_only_citations_do_not_count_toward_floor() {
        // Two citations, but both are insight references (i*) — zero raw
        // memory ids, so this must be rejected even though the token count
        // looks sufficient.
        assert_eq!(parse_stage2("The user is consistent (because of: i3, i9)"), Stage2Result::Rejected);
        // One insight ref plus only one raw id is still sub-floor.
        assert_eq!(parse_stage2("The user is consistent (because of: i3, 9)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_insight_ref_plus_two_raw_ids_passes() {
        match parse_stage2("The user is consistent (because of: i3, 9, 10)") {
            Stage2Result::Insight { memory_ids, .. } => assert_eq!(memory_ids, vec![9, 10]),
            other => panic!("expected Insight, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_rejects_malformed_citation_token() {
        assert_eq!(parse_stage2("The user likes cats (because of: 12, banana)"), Stage2Result::Rejected);
        assert_eq!(parse_stage2("The user likes cats (because of: j12, 45)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_rejects_empty_or_prose_output() {
        assert_eq!(parse_stage2(""), Stage2Result::Rejected);
        assert_eq!(parse_stage2("I'm not sure about this one."), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_rejects_missing_citation_list() {
        assert_eq!(parse_stage2("The user likes cats."), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nThe user is thorough (because of: 5, 6)";
        match parse_stage2(out) {
            Stage2Result::Insight { memory_ids, .. } => assert_eq!(memory_ids, vec![5, 6]),
            other => panic!("expected Insight, got {:?}", other),
        }
    }

    // --- contradiction check ---

    #[test]
    fn parse_contradiction_yes_with_id() {
        assert_eq!(parse_contradiction("YES 7"), Some(7));
        assert_eq!(parse_contradiction("yes #7"), Some(7));
    }

    #[test]
    fn parse_contradiction_no() {
        assert_eq!(parse_contradiction("NO"), None);
        assert_eq!(parse_contradiction("no\n"), None);
    }

    #[test]
    fn parse_contradiction_malformed_defaults_to_no_contradiction() {
        assert_eq!(parse_contradiction(""), None);
        assert_eq!(parse_contradiction("maybe?"), None);
        assert_eq!(parse_contradiction("YES"), None); // missing id
    }

    // --- confidence ---

    #[test]
    fn compute_confidence_floor_and_growth_and_cap() {
        assert_eq!(compute_confidence(2), 0.5);
        assert!((compute_confidence(3) - 0.6).abs() < 1e-9);
        assert!((compute_confidence(4) - 0.7).abs() < 1e-9);
        assert_eq!(compute_confidence(20), 0.9, "must cap at 0.9");
    }

    // --- prompt builders (smoke tests: content present, not exact wording) ---

    #[test]
    fn build_questions_prompt_includes_ids_and_content() {
        let p = build_questions_prompt(&[(1, "likes rust".to_string()), (2, "works late".to_string())]);
        assert!(p.contains("1: likes rust"));
        assert!(p.contains("2: works late"));
    }

    #[test]
    fn build_insight_prompt_includes_evidence_and_existing_insights() {
        let p = build_insight_prompt(
            "what kind of developer is the user?",
            &[(1, "uses rust".to_string()), (2, "writes tests first".to_string())],
            &[(9, "the user values type safety".to_string())],
        );
        assert!(p.contains("[1] uses rust"));
        assert!(p.contains("[2] writes tests first"));
        assert!(p.contains("[i9] the user values type safety"));
        assert!(p.contains("what kind of developer is the user?"));
        assert!(p.contains("NONE"));
    }

    #[test]
    fn build_contradiction_prompt_includes_claim_and_candidates() {
        let p = build_contradiction_prompt("the user prefers dark mode", &[(3, "switched to light theme".to_string())]);
        assert!(p.contains("the user prefers dark mode"));
        assert!(p.contains("[3] switched to light theme"));
    }

    // --- fake LLM for orchestration-level testing ---

    pub struct FakeReflectLlm {
        pub questions_reply: String,
        pub insight_reply: String,
        pub contradiction_reply: String,
    }

    impl ReflectLlm for FakeReflectLlm {
        fn call(&self, model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            match model {
                "sonnet" => Ok(self.insight_reply.clone()),
                _ => {
                    // Both stage-1 and the contradiction check use haiku;
                    // this fake is only ever exercised one role at a time
                    // in these unit tests, so returning whichever is
                    // non-empty keeps call sites simple.
                    if !self.questions_reply.is_empty() {
                        Ok(self.questions_reply.clone())
                    } else {
                        Ok(self.contradiction_reply.clone())
                    }
                }
            }
        }
    }

    #[test]
    fn fake_llm_round_trips_through_call() {
        let llm = FakeReflectLlm {
            questions_reply: "What does the user do?".to_string(),
            insight_reply: "NONE".to_string(),
            contradiction_reply: "NO".to_string(),
        };
        assert_eq!(llm.call("haiku", "prompt", TIMEOUT_HAIKU).unwrap(), "What does the user do?");
        assert_eq!(llm.call("sonnet", "prompt", TIMEOUT_SONNET).unwrap(), "NONE");
    }
}
