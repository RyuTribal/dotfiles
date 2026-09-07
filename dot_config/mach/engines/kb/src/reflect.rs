
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
use std::collections::HashSet;
use std::time::Duration;

use crate::classify::run_claude;
use crate::store::cosine;

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
         If the evidence instead just re-confirms one of the existing [i*] insights above rather \
         than adding anything new, output exactly `REINFORCE i<id> (because of: <id>, <id>[, ...])`, \
         citing the evidence rows that re-confirm it. \
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
    /// The evidence re-confirms an existing insight (by id) rather than
    /// yielding a new one — `memory_ids` are the raw citations to append
    /// (at least one; whether any of them are actually *new* to that
    /// insight is checked at apply time, against its current `source_ids`,
    /// since the parser has no access to that context).
    Reinforce { insight_id: i64, memory_ids: Vec<i64> },
    /// The model explicitly declined (output was `NONE`).
    None,
    /// Anything else: empty output, no recognizable citation line,
    /// malformed citation tokens, or fewer than 2 raw-memory ids cited
    /// (fewer than 1 for `REINFORCE`).
    Rejected,
}

/// Parses the citation list inside a `(because of: ...)` block into
/// `(insight_refs, raw_memory_ids)`. A token is either a raw memory id
/// (`12`, `#12`, `[12]` — a model echoing the `[<id>]` form its own
/// evidence lines were shown in is tolerated the same way `#` and
/// backticks already are) or an insight reference (`i7`, `I7`, `[i7]`);
/// any other token makes the whole list malformed (`Err`) rather than
/// silently dropped — shared by `parse_stage2`'s Insight/Reinforce arms
/// and `parse_theme`, so every citation list in the reflection subsystem
/// is tokenized the same way.
fn parse_citations(inner: &str) -> Result<(Vec<i64>, Vec<i64>), ()> {
    let tokens: Vec<&str> = inner.split(',').map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
    if tokens.is_empty() {
        return Err(());
    }
    let mut insight_ids: Vec<i64> = Vec::new();
    let mut memory_ids: Vec<i64> = Vec::new();
    for tok in &tokens {
        let stripped = tok.trim_matches(|c: char| c == '[' || c == ']').trim_start_matches('#');
        if let Some(rest) = stripped.strip_prefix('i').or_else(|| stripped.strip_prefix('I')) {
            match rest.parse::<i64>() {
                Ok(id) => insight_ids.push(id),
                Err(_) => return Err(()),
            }
            continue;
        }
        match stripped.parse::<i64>() {
            Ok(id) => memory_ids.push(id),
            Err(_) => return Err(()),
        }
    }
    insight_ids.sort_unstable();
    insight_ids.dedup();
    memory_ids.sort_unstable();
    memory_ids.dedup();
    Ok((insight_ids, memory_ids))
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
    const REINFORCE_KEYWORD_LEN: usize = "reinforce".len();
    let marker = "(because of:";

    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("none") {
            return Stage2Result::None;
        }

        // REINFORCE i<id> (because of: <id>[, ...]) — recurrence
        // reinforcement of an existing insight, checked before the plain
        // insight form since both share the trailing citation syntax.
        if line.len() > REINFORCE_KEYWORD_LEN && line[..REINFORCE_KEYWORD_LEN].eq_ignore_ascii_case("reinforce") {
            if !line.ends_with(')') {
                continue;
            }
            let open = match find_ignore_case(line, marker) {
                Some(i) if i >= REINFORCE_KEYWORD_LEN => i,
                _ => continue,
            };
            let head = line[REINFORCE_KEYWORD_LEN..open].trim();
            let head_stripped = head.trim_start_matches('#');
            let target_id = match head_stripped.strip_prefix('i').or_else(|| head_stripped.strip_prefix('I')) {
                Some(rest) => match rest.parse::<i64>() {
                    Ok(id) => id,
                    Err(_) => continue,
                },
                None => continue,
            };
            let inner = &line[open + marker.len()..line.len() - 1];
            let (_insight_refs, memory_ids) = match parse_citations(inner) {
                Ok(v) => v,
                Err(_) => return Stage2Result::Rejected,
            };
            if memory_ids.is_empty() {
                return Stage2Result::Rejected;
            }
            return Stage2Result::Reinforce { insight_id: target_id, memory_ids };
        }

        if !line.ends_with(')') {
            continue;
        }
        let open = match find_ignore_case(line, marker) {
            Some(i) => i,
            None => continue,
        };
        let text = line[..open].trim().trim_matches('`').trim();
        if text.is_empty() {
            continue;
        }
        let inner = &line[open + marker.len()..line.len() - 1];
        let (_insight_refs, memory_ids) = match parse_citations(inner) {
            Ok(v) => v,
            Err(_) => return Stage2Result::Rejected,
        };
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

// --- meta-reflection: level-2 themes ---

/// Builds the per-cluster meta-reflection prompt: the cluster's own
/// level-1 insights as `[i<id>] text`, then their top supporting raw
/// memory evidence as `[<id>] content`.
pub fn build_theme_prompt(insights: &[(i64, String)], evidence: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "These are durable insights already recorded about the user, a project, or a \
         recurring pattern, derived from a personal knowledge bank, that appear to share a \
         common theme:\n\n",
    );
    for (id, text) in insights {
        s.push_str(&format!("[i{}] {}\n", id, text));
    }
    s.push_str("\nTop supporting raw memory evidence for these insights:\n");
    for (id, content) in evidence {
        s.push_str(&format!("[{}] {}\n", id, content));
    }
    s.push_str(
        "\nState ONE unifying theme statement covering these insights -- naming the domain it \
         covers, e.g. \"Engineering discipline: ...\" or \"Collaboration style: ...\" -- ONLY IF \
         at least 2 of the insights above genuinely share it AND at least 1 raw memory row \
         directly supports it. Format exactly: \
         `<theme text> (because of: i<id>, i<id>[, ...], <raw id>[, ...])`. \
         If these insights don't share a genuine common theme, output exactly NONE.\n",
    );
    s
}

/// Outcome of parsing a meta-reflection reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeResult {
    /// A well-formed theme citing `>= 2` level-1 insights (all from the
    /// known/shown set) and `>= 1` raw memory id.
    Theme { text: String, insight_ids: Vec<i64>, memory_ids: Vec<i64> },
    /// The model explicitly declined (output was `NONE`).
    None,
    /// Anything else: empty/prose output, malformed citation tokens, fewer
    /// than 2 insight citations, zero raw-memory citations, or an insight
    /// citation naming an id outside `known_insight_ids` — most notably
    /// covering the case of a hallucinated id or a level-2 theme's id
    /// (a theme is built only from level-1 insights, so a genuine level-2
    /// id is never among `known_insight_ids` in the first place — a theme
    /// must never cite another theme).
    Rejected,
}

/// Parses a meta-reflection reply per the exact format instructed in
/// `build_theme_prompt`. `known_insight_ids` is the set of level-1 insight
/// ids actually shown to the model in this cluster's prompt (as `[i<id>]`
/// lines) — any `i<id>` citation outside that set rejects the whole reply.
pub fn parse_theme(output: &str, known_insight_ids: &HashSet<i64>) -> ThemeResult {
    let marker = "(because of:";
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.eq_ignore_ascii_case("none") {
            return ThemeResult::None;
        }
        if !line.ends_with(')') {
            continue;
        }
        let open = match find_ignore_case(line, marker) {
            Some(i) => i,
            None => continue,
        };
        let text = line[..open].trim().trim_matches('`').trim();
        if text.is_empty() {
            continue;
        }
        let inner = &line[open + marker.len()..line.len() - 1];
        let (insight_ids, memory_ids) = match parse_citations(inner) {
            Ok(v) => v,
            Err(_) => return ThemeResult::Rejected,
        };
        if insight_ids.iter().any(|id| !known_insight_ids.contains(id)) {
            return ThemeResult::Rejected;
        }
        if insight_ids.len() < 2 || memory_ids.is_empty() {
            return ThemeResult::Rejected;
        }
        return ThemeResult::Theme { text: text.to_string(), insight_ids, memory_ids };
    }
    ThemeResult::Rejected
}

/// Minimum cosine similarity for two level-1 insights to land in the same
/// cluster during meta-reflection.
pub const META_CLUSTER_MIN_SIM: f32 = 0.5;
/// A cluster smaller than this stays unthemed rather than becoming a
/// trivial one-insight "theme."
pub const META_MIN_CLUSTER_SIZE: usize = 2;
/// The meta-reflection trigger's level-1 evidence floor (see
/// `should_run_meta_pass`).
pub const META_TRIGGER_MIN_LEVEL1: i64 = 6;
/// How many new level-1 insights must accumulate since the newest theme
/// before another meta pass is worth running (see `should_run_meta_pass`).
pub const META_TRIGGER_GROWTH: i64 = 3;

/// Greedy clustering by embedding similarity, parameterized by threshold
/// and minimum cluster size: `items` must be pre-sorted oldest-first
/// (ascending id). The seed of each cluster is the oldest not-yet-assigned
/// item; every other not-yet-assigned item with cosine similarity `>=
/// min_sim` to that seed joins its cluster. A cluster smaller than
/// `min_size` is dropped (its seed stays unclustered for a later pass, once
/// more items accumulate around it). Returns each surviving cluster as a
/// `Vec<i64>` of ids. Shared by meta-reflection's theme clustering
/// (`cluster_insights_by_similarity`) and the dormancy pass's consolidation
/// clustering, each with its own threshold/floor.
pub fn cluster_by_similarity(items: &[(i64, Vec<f32>)], min_sim: f32, min_size: usize) -> Vec<Vec<i64>> {
    let mut used = vec![false; items.len()];
    let mut clusters = Vec::new();
    for seed_idx in 0..items.len() {
        if used[seed_idx] {
            continue;
        }
        let mut cluster = vec![seed_idx];
        used[seed_idx] = true;
        for j in (seed_idx + 1)..items.len() {
            if used[j] {
                continue;
            }
            if cosine(&items[seed_idx].1, &items[j].1) >= min_sim {
                cluster.push(j);
                used[j] = true;
            }
        }
        if cluster.len() >= min_size {
            clusters.push(cluster.iter().map(|&i| items[i].0).collect());
        }
        // else: a singleton (or under-floor group) stays out of any
        // cluster — its `used` flag is still set so it isn't re-tested as
        // a neighbor of a later seed, but it also never appears in any
        // cluster this pass produces.
    }
    clusters
}

/// Meta-reflection's own clustering call: the fixed `META_CLUSTER_MIN_SIM`
/// / `META_MIN_CLUSTER_SIZE` thresholds, over active level-1 insights.
pub fn cluster_insights_by_similarity(items: &[(i64, Vec<f32>)]) -> Vec<Vec<i64>> {
    cluster_by_similarity(items, META_CLUSTER_MIN_SIM, META_MIN_CLUSTER_SIZE)
}

/// Minimum cosine similarity for two newly-dormant memories to land in the
/// same consolidation cluster (the dormancy pass in `mach kb reflect`).
pub const DORMANCY_CONSOLIDATION_MIN_SIM: f32 = 0.55;
/// A consolidation cluster smaller than this is left alone — each row stays
/// its own dormant memory rather than becoming a trivial one-row "summary".
pub const DORMANCY_CONSOLIDATION_MIN_CLUSTER: usize = 3;

/// Builds the per-cluster consolidation prompt: a cluster of related,
/// newly-dormant, low-importance memories, asked to be folded into one
/// compact durable fact (or declined outright).
pub fn build_consolidation_prompt(memories: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "These related, low-importance memories from a personal knowledge bank are about to go \
         dormant (archived, no longer surfaced in search):\n\n",
    );
    for (id, content) in memories {
        s.push_str(&format!("[{}] {}\n", id, content));
    }
    s.push_str(
        "\nSummarize these into ONE compact fact that preserves anything durable still worth \
         keeping. Output the fact alone and nothing else, or output exactly NONE if nothing here \
         is worth preserving.\n",
    );
    s
}

/// Parses a consolidation reply: `None` for an explicit `NONE` (whole-output
/// match, tolerating surrounding whitespace) or empty output; otherwise the
/// trimmed fact text. Unlike the other stage parsers here there's no
/// citation marker to scan for — the model is asked for prose alone — so
/// this simply trims rather than hunting line by line.
pub fn parse_consolidation(output: &str) -> Option<String> {
    let text = output.trim().trim_matches('`').trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(text.to_string())
    }
}

/// Whether `mach kb reflect`'s meta-reflection (theme) pass should run this
/// time: `>= META_TRIGGER_MIN_LEVEL1` active level-1 insights exist, AND
/// either no level-2 theme exists yet or at least `META_TRIGGER_GROWTH` new
/// level-1 insights have accumulated since the newest theme was created.
pub fn should_run_meta_pass(level1_active_count: i64, has_active_theme: bool, level1_since_newest_theme: i64) -> bool {
    if level1_active_count < META_TRIGGER_MIN_LEVEL1 {
        return false;
    }
    if !has_active_theme {
        return true;
    }
    level1_since_newest_theme >= META_TRIGGER_GROWTH
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
    fn parse_stage2_tolerates_bracket_wrapped_ids() {
        // A model sometimes echoes a raw id back in the `[<id>]` form its
        // own evidence lines were shown in — must be tolerated exactly
        // like the `#` prefix already is.
        let out = "The user likes tea (because of: [3], [9])";
        match parse_stage2(out) {
            Stage2Result::Insight { memory_ids, .. } => assert_eq!(memory_ids, vec![3, 9]),
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

    // --- stage 2 parser: REINFORCE ---

    #[test]
    fn parse_stage2_accepts_reinforce_with_one_citation() {
        match parse_stage2("REINFORCE i7 (because of: 12)") {
            Stage2Result::Reinforce { insight_id, memory_ids } => {
                assert_eq!(insight_id, 7);
                assert_eq!(memory_ids, vec![12]);
            }
            other => panic!("expected Reinforce, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_reinforce_accepts_multiple_citations_deduped() {
        match parse_stage2("REINFORCE i3 (because of: 12, 12, 45)") {
            Stage2Result::Reinforce { insight_id, memory_ids } => {
                assert_eq!(insight_id, 3);
                assert_eq!(memory_ids, vec![12, 45]);
            }
            other => panic!("expected Reinforce, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_reinforce_is_case_insensitive_and_tolerates_hash_prefix() {
        match parse_stage2("reinforce #i9 (because of: #4, #5)") {
            Stage2Result::Reinforce { insight_id, memory_ids } => {
                assert_eq!(insight_id, 9);
                assert_eq!(memory_ids, vec![4, 5]);
            }
            other => panic!("expected Reinforce, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_reinforce_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nREINFORCE i2 (because of: 8, 9)";
        match parse_stage2(out) {
            Stage2Result::Reinforce { insight_id, memory_ids } => {
                assert_eq!(insight_id, 2);
                assert_eq!(memory_ids, vec![8, 9]);
            }
            other => panic!("expected Reinforce, got {:?}", other),
        }
    }

    #[test]
    fn parse_stage2_rejects_reinforce_with_no_citations() {
        // "(because of:)" carries no tokens at all — malformed regardless
        // of the >= 1 floor.
        assert_eq!(parse_stage2("REINFORCE i7 (because of:)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_rejects_reinforce_citing_only_insight_refs() {
        // Zero raw-memory ids — an insight-only citation can never satisfy
        // "at least 1 new raw-memory id."
        assert_eq!(parse_stage2("REINFORCE i7 (because of: i3, i9)"), Stage2Result::Rejected);
    }

    #[test]
    fn parse_stage2_rejects_reinforce_missing_target_id() {
        assert_eq!(parse_stage2("REINFORCE (because of: 1, 2)"), Stage2Result::Rejected);
        assert_eq!(parse_stage2("REINFORCE 7 (because of: 1, 2)"), Stage2Result::Rejected, "missing the i-prefix on the target");
    }

    #[test]
    fn parse_stage2_rejects_reinforce_malformed_citation_token() {
        assert_eq!(parse_stage2("REINFORCE i7 (because of: 12, banana)"), Stage2Result::Rejected);
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
        assert!(p.contains("REINFORCE"));
    }

    #[test]
    fn build_contradiction_prompt_includes_claim_and_candidates() {
        let p = build_contradiction_prompt("the user prefers dark mode", &[(3, "switched to light theme".to_string())]);
        assert!(p.contains("the user prefers dark mode"));
        assert!(p.contains("[3] switched to light theme"));
    }

    // --- meta-reflection: theme prompt/parser ---

    #[test]
    fn build_theme_prompt_includes_insights_and_evidence() {
        let p = build_theme_prompt(
            &[(1, "the user writes tests first".to_string()), (2, "the user reviews their own diffs".to_string())],
            &[(10, "ran cargo test before every commit".to_string())],
        );
        assert!(p.contains("[i1] the user writes tests first"));
        assert!(p.contains("[i2] the user reviews their own diffs"));
        assert!(p.contains("[10] ran cargo test before every commit"));
        assert!(p.contains("NONE"));
    }

    #[test]
    fn parse_theme_accepts_valid_theme_citing_two_known_insights_and_a_raw_id() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        match parse_theme("Engineering discipline: the user tests before shipping (because of: i1, i2, 10)", &known) {
            ThemeResult::Theme { text, insight_ids, memory_ids } => {
                assert_eq!(text, "Engineering discipline: the user tests before shipping");
                assert_eq!(insight_ids, vec![1, 2]);
                assert_eq!(memory_ids, vec![10]);
            }
            other => panic!("expected Theme, got {:?}", other),
        }
    }

    #[test]
    fn parse_theme_rejects_none() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(parse_theme("NONE", &known), ThemeResult::None);
    }

    #[test]
    fn parse_theme_rejects_fewer_than_two_insight_citations() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(
            parse_theme("A theme (because of: i1, 10)", &known),
            ThemeResult::Rejected,
            "only one insight cited"
        );
    }

    #[test]
    fn parse_theme_rejects_zero_raw_memory_citations() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(
            parse_theme("A theme (because of: i1, i2)", &known),
            ThemeResult::Rejected,
            "no raw memory evidence at all"
        );
    }

    #[test]
    fn parse_theme_rejects_citation_outside_known_ids_theme_cites_theme() {
        // known_insight_ids only ever holds level-1 insight ids actually
        // shown in this cluster's prompt — a level-2 theme's id is never
        // among them, so a citation naming one (or any hallucinated id)
        // must reject the whole reply, never silently drop just that
        // token: a theme must never cite another theme.
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(
            parse_theme("A theme (because of: i1, i99, 10)", &known),
            ThemeResult::Rejected,
            "i99 is outside the known level-1 set — e.g. a theme's own id"
        );
    }

    #[test]
    fn parse_theme_tolerates_bracket_wrapped_citations() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        match parse_theme("A real theme (because of: [i1], [i2], [64])", &known) {
            ThemeResult::Theme { insight_ids, memory_ids, .. } => {
                assert_eq!(insight_ids, vec![1, 2]);
                assert_eq!(memory_ids, vec![64]);
            }
            other => panic!("expected Theme, got {:?}", other),
        }
    }

    #[test]
    fn parse_theme_rejects_malformed_citation_token() {
        let known: HashSet<i64> = [1, 2].into_iter().collect();
        assert_eq!(parse_theme("A theme (because of: i1, i2, banana)", &known), ThemeResult::Rejected);
    }

    // --- meta-reflection: clustering ---

    fn unit_vec(dims: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        v[hot] = 1.0;
        v
    }

    #[test]
    fn cluster_insights_by_similarity_groups_near_duplicates_and_drops_singletons() {
        // ids 1,2 share a direction (sim 1.0), id 3 is orthogonal and alone
        // -> forms its own cluster of size 1, which must be dropped.
        let items = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 1))];
        let clusters = cluster_insights_by_similarity(&items);
        assert_eq!(clusters, vec![vec![1, 2]]);
    }

    #[test]
    fn cluster_insights_by_similarity_seeds_from_oldest_first() {
        // items must already be sorted oldest-first by the caller; the
        // seed of the first cluster is always items[0].
        let items = vec![(5, unit_vec(4, 0)), (7, unit_vec(4, 0)), (9, unit_vec(4, 0))];
        let clusters = cluster_insights_by_similarity(&items);
        assert_eq!(clusters, vec![vec![5, 7, 9]]);
    }

    #[test]
    fn cluster_insights_by_similarity_forms_multiple_disjoint_clusters() {
        let items =
            vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 1)), (4, unit_vec(4, 1))];
        let clusters = cluster_insights_by_similarity(&items);
        assert_eq!(clusters, vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn cluster_insights_by_similarity_empty_input_yields_no_clusters() {
        assert!(cluster_insights_by_similarity(&[]).is_empty());
    }

    // --- dormancy consolidation: clustering threshold/floor, prompt, parser ---

    #[test]
    fn cluster_by_similarity_respects_its_own_threshold_and_floor() {
        // Same shape as the meta-reflection clustering tests, but proving
        // the generic function actually honors whatever threshold/floor
        // it's called with rather than the hardcoded META_* constants.
        let items = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 0))];
        // Floor of 3 is met exactly.
        assert_eq!(cluster_by_similarity(&items, 0.55, 3), vec![vec![1, 2, 3]]);
        // Raise the floor to 4 — the same three items now fall short.
        assert!(cluster_by_similarity(&items, 0.55, 4).is_empty());
    }

    #[test]
    fn cluster_by_similarity_orthogonal_items_never_join_a_cluster() {
        let items = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 1))];
        let clusters = cluster_by_similarity(&items, 0.55, 3);
        assert!(clusters.is_empty(), "only 2 of the 3 share a direction — under the floor of 3");
    }

    #[test]
    fn build_consolidation_prompt_includes_ids_and_content() {
        let p = build_consolidation_prompt(&[(1, "likes tea".to_string()), (2, "likes coffee".to_string())]);
        assert!(p.contains("[1] likes tea"));
        assert!(p.contains("[2] likes coffee"));
        assert!(p.contains("NONE"));
    }

    #[test]
    fn parse_consolidation_returns_trimmed_fact() {
        assert_eq!(parse_consolidation("  The user has tried several beverages.  \n"), Some("The user has tried several beverages.".to_string()));
        assert_eq!(parse_consolidation("`a backtick-wrapped fact`"), Some("a backtick-wrapped fact".to_string()));
    }

    #[test]
    fn parse_consolidation_none_and_empty_both_decline() {
        assert_eq!(parse_consolidation("NONE"), None);
        assert_eq!(parse_consolidation("  none  \n"), None);
        assert_eq!(parse_consolidation(""), None);
        assert_eq!(parse_consolidation("   \n  "), None);
    }

    // --- meta-reflection: trigger conditions ---

    #[test]
    fn should_run_meta_pass_false_below_level1_floor() {
        assert!(!should_run_meta_pass(5, false, 0));
        assert!(!should_run_meta_pass(0, false, 0));
    }

    #[test]
    fn should_run_meta_pass_true_at_floor_with_no_existing_theme() {
        assert!(should_run_meta_pass(6, false, 0));
        assert!(should_run_meta_pass(20, false, 0));
    }

    #[test]
    fn should_run_meta_pass_false_when_theme_exists_and_growth_insufficient() {
        assert!(!should_run_meta_pass(6, true, 0));
        assert!(!should_run_meta_pass(9, true, 2));
    }

    #[test]
    fn should_run_meta_pass_true_when_theme_exists_but_growth_meets_floor() {
        assert!(should_run_meta_pass(6, true, 3));
        assert!(should_run_meta_pass(9, true, 5));
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
