
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
use std::collections::{HashMap, HashSet};
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

// --- opportunistic scheduling: offline safety ---

/// Cheap, no-`claude`-spawn reachability probe: a plain TCP connect (with
/// DNS resolution) to `api.anthropic.com:443`, 2s timeout per resolved
/// address. Not a guarantee a real `claude -p` call will succeed (a
/// captive portal or a misbehaving proxy can still pass this and fail the
/// real call), but cheap enough to run on every opportunistic timer
/// wakeup, and precise enough to skip a laptop that's asleep, suspended,
/// or off wifi before ever spawning `claude` and eating one of its
/// 30s-60s timeouts. Local `ollama` embedding calls need no such guard —
/// only a `claude` call ever leaves the machine. Not unit-tested (real
/// DNS/network, like `ProcessReflectLlm`'s own process-spawning path) —
/// exercised by a live run instead.
pub fn claude_reachable() -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    match "api.anthropic.com:443".to_socket_addrs() {
        Ok(addrs) => addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok()),
        Err(_) => false,
    }
}

/// Whether `mach kb reflect` should advance its watermark (`last_run_at` +
/// `last_memory_id`) this run: never when there was nothing new to
/// examine in the first place (unchanged, long-standing rule — a
/// `--meta`-only invocation must never clobber it back to NULL), and never
/// when any `claude` call failed partway through this run (offline mid-run,
/// a spawn error, a timeout) — a partial run leaves some of this run's new
/// material, or a dedupe/verification candidate, unexamined, and the
/// watermark must not claim otherwise. Both fields move together or not at
/// all: an offline or degraded run must be fully retry-able next time, not
/// have some of its queues silently orphaned behind an advanced watermark.
pub fn should_advance_watermark(has_new: bool, llm_failed: bool) -> bool {
    has_new && !llm_failed
}

// --- nightly dedupe pass ---

/// Minimum cosine similarity for two active memories to be considered a
/// candidate near-duplicate pair by the nightly dedupe pass.
pub const DEDUPE_MIN_SIM: f32 = 0.85;
/// At most this many candidate pairs are sent to the dedupe judge per
/// `mach kb reflect` run — keeps a single run's LLM cost bounded even if a
/// large backlog of near-duplicates has accumulated (fast capture paths
/// like `mach note` and digests deliberately skip save-time dedupe
/// classification, so it can).
pub const DEDUPE_MAX_PAIRS_PER_RUN: usize = 10;

/// Finds this run's dedupe candidate pairs: `(a, b)` with `a < b`, cosine
/// similarity `>= min_sim`, at least one of the pair in `new_ids` (created
/// since the last reflect run — mirrors the reflection working set's own
/// "new-vs-all" shape rather than rescanning the whole store nightly), and
/// not already recorded in `seen`. Capped at `cap`, oldest-first among
/// qualifying pairs (sorted by the smaller id in each pair, which is
/// already how each tuple is normalized) so a backlog drains in a stable
/// order across runs instead of being reshuffled by whatever happens to
/// embed together this time.
///
/// `pool` must hold every active memory with a stored embedding (id order
/// doesn't matter — this does its own O(n^2) scan, acceptable at the
/// reflection working set's scale); `new_ids` and `seen` are lookup sets
/// the caller builds once per run.
pub fn dedupe_candidate_pairs(
    pool: &[(i64, Vec<f32>)],
    new_ids: &HashSet<i64>,
    seen: &HashSet<(i64, i64)>,
    min_sim: f32,
    cap: usize,
) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let (id_a, id_b) = if pool[i].0 < pool[j].0 { (pool[i].0, pool[j].0) } else { (pool[j].0, pool[i].0) };
            if !new_ids.contains(&id_a) && !new_ids.contains(&id_b) {
                continue;
            }
            if seen.contains(&(id_a, id_b)) {
                continue;
            }
            if cosine(&pool[i].1, &pool[j].1) >= min_sim {
                out.push((id_a, id_b));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out.truncate(cap);
    out
}

/// Outcome of parsing the nightly dedupe judge's reply for one candidate
/// pair.
///
/// `DUPLICATE <id>` and `SUPERSEDES <id>` used to be two separate variants
/// here, both naming the winner id — but `run_dedupe_pass` (in `cli.rs`)
/// has always applied them identically (merge the loser's stats into the
/// winner, then tombstone the loser), so the distinction was cosmetic and
/// bought nothing except a second copy of the exact "which one is the
/// winner" grammar the contradiction judge's own `SUPERSEDES <id>` was
/// found to invert (see `build_contradiction_pass_prompt`'s doc comment).
/// Collapsed into one `KEEP <id>` token: same apply-time behavior, and one
/// fewer grammar for a model to misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupeVerdict {
    /// Both memories describe the same fact, or one updates/corrects the
    /// other — either way, `keep_id` (one of the pair) names the copy to
    /// keep; the other is the loser.
    Keep { keep_id: i64 },
    /// The judge explicitly declined: similar topic, different facts —
    /// leave both alone. Recorded in `dedupe_seen` so this exact pair is
    /// never re-asked.
    Distinct,
    /// Empty/prose output, an unrecognized line, a missing id, or a cited
    /// id that isn't one of the two pair members. Never destructive (never
    /// merges) — but deliberately distinct from `Distinct`: this pair is
    /// NOT recorded as seen, so a transient bad reply (or, upstream, a
    /// `claude` call that failed outright) gets a fresh chance on a later
    /// run instead of being silenced forever.
    Malformed,
}

/// Parses the nightly dedupe judge's reply for the pair `(id_a, id_b)`.
/// Scans line by line (tolerating leading chatter and a `#` prefix on the
/// id, like `classify::parse_verdict`) for `KEEP <id>` or a bare
/// `DISTINCT`. Strict about the cited id: it must be one of the two pair
/// members, or the whole reply is `Malformed` rather than silently
/// accepting a hallucinated third id.
pub fn parse_dedupe_verdict(output: &str, id_a: i64, id_b: i64) -> DedupeVerdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if upper == "DISTINCT" {
            return DedupeVerdict::Distinct;
        }
        if let Some(rest) = upper.strip_prefix("KEEP") {
            if let Some(id) = extract_dedupe_id(rest) {
                return if id == id_a || id == id_b {
                    DedupeVerdict::Keep { keep_id: id }
                } else {
                    DedupeVerdict::Malformed
                };
            }
        }
    }
    DedupeVerdict::Malformed
}

fn extract_dedupe_id(rest: &str) -> Option<i64> {
    rest.trim().trim_start_matches('#').split_whitespace().next()?.parse().ok()
}

/// Builds the nightly dedupe pass's one-haiku-call prompt (same invocation
/// pattern as `classify::build_prompt`): both memories' full content and
/// source, asking for a verdict among `KEEP <id>` (same fact, or one
/// updates/corrects the other — either way, keep this one) or `DISTINCT`
/// (similar topic, different facts — leave both). `a`/`b` are `(id,
/// content, source)`.
pub fn build_dedupe_prompt(a: (i64, &str, Option<&str>), b: (i64, &str, Option<&str>)) -> String {
    let mut s = String::new();
    s.push_str(
        "You are checking a personal knowledge bank for near-duplicate memories. Two memories \
         embedded close together are shown below -- decide the single correct relationship \
         between them.\n\n",
    );
    s.push_str(&format!("Memory #{} (source: {}):\n{}\n\n", a.0, a.2.unwrap_or("unknown"), a.1));
    s.push_str(&format!("Memory #{} (source: {}):\n{}\n\n", b.0, b.2.unwrap_or("unknown"), b.1));
    s.push_str(
        "Reply with exactly one line and no other text: KEEP <id> or DISTINCT.\n\
         KEEP <id> -- these describe the same fact, or one updates or corrects the other; <id> \
         names the copy to keep (the richer, newer, or now-correct one) -- the other will be removed.\n\
         DISTINCT -- similar topic but different facts; keep both.\n",
    );
    s
}

// --- contradiction patrol ---

/// Minimum cosine similarity for two active memories to be considered a
/// contradiction-patrol candidate pair: below this, two memories are
/// similar only in a coincidental, surface way and aren't worth a judge
/// call.
pub const CONTRADICTION_MIN_SIM: f32 = 0.60;
/// Similarity at or above this belongs to the nightly dedupe pass instead
/// (`DEDUPE_MIN_SIM`) — the contradiction band is deliberately the strip
/// just below it: semantic siblings (same rough topic) that are textually
/// different enough that dedupe's own classifier never sees them as a
/// candidate pair, yet close enough that one may be a stale version of the
/// other (e.g. "Moses is the user's boss" vs. "the user's boss Ivar
/// approved the RHI redesign" — around 0.6-0.8 similarity, nowhere near
/// dedupe's 0.85 floor, so the stale fact would otherwise just sit there
/// accumulating reinforcement forever).
pub const CONTRADICTION_MAX_SIM: f32 = 0.85;
/// At most this many candidate pairs are sent to the contradiction judge
/// per `mach kb reflect` run — same bounded-cost rationale as
/// `DEDUPE_MAX_PAIRS_PER_RUN`.
pub const CONTRADICTION_MAX_PAIRS_PER_RUN: usize = 10;

/// Finds this run's contradiction-patrol candidate pairs: `(a, b)` with `a <
/// b`, cosine similarity in `[min_sim, max_sim)` — semantic siblings,
/// textually different, sitting just below the dedupe band rather than
/// overlapping it — at least one side in `new_ids` (created since the last
/// reflect run), and not already recorded in `seen` (the caller's own union
/// of `dedupe_seen` and `contradiction_seen`: a pair either judge has
/// already ruled on needs no second opinion). Capped at `cap`,
/// **newest-first** (by the larger id in each pair, which is already how
/// each tuple is normalized) — the opposite order from
/// `dedupe_candidate_pairs`'s oldest-first drain: a fresh contradiction (a
/// new fact quietly conflicting with an old one) is exactly the case this
/// pass exists to catch quickly, so it shouldn't wait behind a long backlog
/// of older near-duplicate pairs.
pub fn contradiction_candidate_pairs(
    pool: &[(i64, Vec<f32>)],
    new_ids: &HashSet<i64>,
    seen: &HashSet<(i64, i64)>,
    min_sim: f32,
    max_sim: f32,
    cap: usize,
) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for i in 0..pool.len() {
        for j in (i + 1)..pool.len() {
            let (id_a, id_b) = if pool[i].0 < pool[j].0 { (pool[i].0, pool[j].0) } else { (pool[j].0, pool[i].0) };
            if !new_ids.contains(&id_a) && !new_ids.contains(&id_b) {
                continue;
            }
            if seen.contains(&(id_a, id_b)) {
                continue;
            }
            let sim = cosine(&pool[i].1, &pool[j].1);
            if sim >= min_sim && sim < max_sim {
                out.push((id_a, id_b));
            }
        }
    }
    // Newest-first: id_b is always the larger of the normalized pair, so
    // sorting on it descending (tie-broken on id_a descending) is enough —
    // no need to compute a separate max().
    out.sort_by(|x, y| y.1.cmp(&x.1).then(y.0.cmp(&x.0)));
    out.dedup();
    out.truncate(cap);
    out
}

/// Outcome of parsing the contradiction judge's reply for one candidate
/// pair. Carries no id: unlike an earlier version of this verdict, which
/// asked the model to name a `SUPERSEDES <id>` winner, the winner is now
/// resolved entirely in Rust via `newer_wins` — see
/// `build_contradiction_pass_prompt`'s doc comment for why the model is
/// never asked to name one.
///
/// `Conflict` and `ConflictRetro` both mean the two memories describe the
/// same role/attribute/state and disagree — the split is *which one
/// describes the current state*, a judgment the model has to make, not
/// arithmetic on timestamps. Record time and event time are not the same
/// thing: someone can jot down a retrospective note (`"Moses was my boss
/// before Ivar"`) well after the fact, in which case the newer-*recorded*
/// memory is the one that's stale about *now*, even though it's the more
/// recent row. That's exactly the case `ConflictRetro` exists to catch —
/// see its own doc comment for how it's applied differently from a plain
/// `Conflict`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContradictionVerdict {
    /// The two facts conflict about the same thing — a role, attribute, or
    /// state that changed over time — AND the newer-recorded memory is the
    /// one describing the current state (the ordinary case: a change is
    /// learned of and written down close to when it happened). The caller
    /// resolves which one wins via `newer_wins` (newer record wins) and
    /// tombstones the other.
    Conflict,
    /// Same disagreement, but the newer-*recorded* memory is itself
    /// describing a *past* state retrospectively — the older-recorded
    /// memory is the one that actually holds true now. This is not "the
    /// retrospective note is wrong" (it may be a perfectly accurate piece
    /// of history) — it's simply not the *current* fact to surface, so the
    /// caller tombstones the newer-recorded memory under the older one
    /// (`newer_wins`'s output consumed with winner/loser swapped from the
    /// plain `Conflict` case) — it leaves active recall but stays
    /// queryable, same as any other tombstoned row.
    ConflictRetro,
    /// Both facts are compatible: different subjects, or explicitly both
    /// still true. Recorded in `contradiction_seen` so this exact pair is
    /// never re-asked.
    BothHold,
    /// The judge couldn't tell, or its reply was empty or unrecognized —
    /// never recorded either way, so this pair gets a fresh chance on a
    /// later run (same offline discipline the nightly dedupe pass already
    /// follows for its own `Malformed`).
    Unclear,
}

/// Given two `(id, recorded_at)` pairs, resolves which one was recorded more
/// recently, as `(later_id, earlier_id)`. A plain string compare is exact
/// for this store's fixed-width RFC3339 timestamps (same trick
/// `store::memory_last_modified` relies on); a tie favors `b`. Shared by
/// the prompt builder (for its informational note) and
/// `cli::apply_contradiction_verdict`, which consumes this same ordering
/// two different ways depending on the verdict: for a plain `Conflict` the
/// later-recorded id is the winner (as returned); for `ConflictRetro` the
/// caller swaps it — the later-recorded id is retrospective and loses,
/// the earlier-recorded id is the one that's actually current. Naming this
/// `(later_id, earlier_id)` rather than `(winner_id, loser_id)` reflects
/// that this function only ever resolves record order, never who "wins" —
/// that judgment now depends on which verdict the model returned.
pub fn newer_wins(a: (i64, &str), b: (i64, &str)) -> (i64, i64) {
    if b.1 >= a.1 {
        (b.0, a.0)
    } else {
        (a.0, b.0)
    }
}

/// Parses the contradiction judge's reply. Scans line by line (tolerating
/// leading chatter, like every other judge parser in this module) for
/// exactly one of four whole-line tokens: `CONFLICT`, `CONFLICT_RETRO`,
/// `BOTH_HOLD`, or `UNCLEAR`. Deliberately strict — no id is ever parsed
/// here (see `ContradictionVerdict`'s own doc comment for why), so a line
/// like `CONFLICT 12` matches none of the four tokens and is simply skipped
/// like any other unrecognized line; reaching the end of the reply without
/// a match is `Unclear`, matching the module-wide rule that a bad or
/// unparseable reply is never destructive.
pub fn parse_contradiction_verdict(output: &str) -> ContradictionVerdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        match line.to_uppercase().as_str() {
            "CONFLICT" => return ContradictionVerdict::Conflict,
            "CONFLICT_RETRO" => return ContradictionVerdict::ConflictRetro,
            "BOTH_HOLD" => return ContradictionVerdict::BothHold,
            "UNCLEAR" => return ContradictionVerdict::Unclear,
            _ => continue,
        }
    }
    ContradictionVerdict::Unclear
}

/// Builds the contradiction judge's one-haiku-call prompt: both memories'
/// content and recorded date, asking for a verdict among `CONFLICT`,
/// `CONFLICT_RETRO`, `BOTH_HOLD`, or `UNCLEAR`. `a`/`b` are `(id, content,
/// date)`.
///
/// The model is never asked to name a winner id. An earlier version of this
/// verdict asked for `SUPERSEDES <id>` — first leaving the model to work out
/// *which* id was newer from the raw dates (live testing against real haiku
/// replies showed it reliably favored whichever memory *reads* as the more
/// direct, plain statement of the role, even with the dates right there and
/// an explicit "always pick the later one" instruction — a wording bias that
/// silently overrode recency), then, after precomputing the winner in Rust
/// and spelling it out as `output SUPERSEDES {winner_id}` in the instruction
/// itself, STILL inverting it: asked three times to output `SUPERSEDES 134`
/// (the precomputed, spelled-out winner), haiku replied `SUPERSEDES 133`
/// (the loser) every time. The grammar itself is ambiguous — "SUPERSEDES
/// <id>" reads in ordinary English as "<id> is the one that gets
/// superseded," i.e. the loser, the opposite of what the parser expected —
/// and no amount of surrounding instruction reliably overrides that reading.
/// The fix removes the id from the model's task entirely.
///
/// What the model still has to decide, though, is which memory describes
/// the *current* state — that's a judgment call, not arithmetic: record
/// time and event time can diverge (a retrospective note written well after
/// the fact is a newer *row* about an *older* state). `CONFLICT_RETRO`
/// exists so the model can say so explicitly rather than the timestamp
/// alone silently (and sometimes wrongly) picking a winner. The raw
/// timestamp comparison (`newer_wins`) still lives entirely in Rust either
/// way — the model never states or is asked to state an id, only which of
/// the four fixed-shape verdicts applies.
pub fn build_contradiction_pass_prompt(a: (i64, &str, &str), b: (i64, &str, &str)) -> String {
    let mut s = String::new();
    s.push_str(
        "You are checking a personal knowledge bank for contradictions between two semantically \
         related but textually different memories -- decide the single correct relationship \
         between them.\n\n",
    );
    s.push_str(&format!("Memory #{} (recorded {}):\n{}\n\n", a.0, a.2, a.1));
    s.push_str(&format!("Memory #{} (recorded {}):\n{}\n\n", b.0, b.2, b.1));
    let (later_id, earlier_id) = newer_wins((a.0, a.2), (b.0, b.2));
    s.push_str(&format!(
        "Note: memory #{} was recorded more recently than memory #{}. This is only when each memory \
         was WRITTEN, not necessarily which one describes the more current state -- a memory can be a \
         retrospective note about the past, recorded well after the fact.\n\n",
        later_id, earlier_id
    ));
    s.push_str(
        "Reply with exactly one line and no other text: CONFLICT, CONFLICT_RETRO, BOTH_HOLD, or UNCLEAR.\n\
         CONFLICT -- both memories name who or what currently holds the same role, attribute, or state, \
         they disagree, AND the more recently recorded memory is describing the CURRENT state (the \
         ordinary case: a change happened and was written down close to when it happened). You don't \
         need to say which one wins; the more recently recorded memory will automatically be treated as \
         current.\n\
         CONFLICT_RETRO -- same disagreement, but the more recently recorded memory is itself describing \
         a PAST state retrospectively (reminiscing, or giving historical context), while the OLDER-recorded \
         memory is the one that actually describes what's true now. Use this when the newer note reads as \
         being about the past, not as an update to the present.\n\
         BOTH_HOLD -- these are genuinely different subjects, or both are still true at once; keep both.\n\
         UNCLEAR -- you cannot tell from what's shown.\n",
    );
    s
}

// --- curation pass: organic promotion of the unreviewed queue ---
//
// Memory is organic: `search_ranked` already surfaces unreviewed rows by
// default (at a small confidence penalty — see
// `store::UNREVIEWED_SEARCH_PENALTY`), so `mach kb review` is no longer a
// gate anything has to pass through to be useful. This pass is what closes
// the loop — it judges the auto-captured queue itself, on the same footing
// `mach kb review`'s human "keep" already uses (`reviewed = 1`), so the
// penalty goes away as evidence accumulates, not on a fixed clock. `mach kb
// review` remains available as an optional, immediate override (it can
// still keep/edit/delete a row directly) — this pass is what makes it
// optional rather than required.
//
// Runs in `mach kb reflect` after the contradiction patrol and before the
// dormancy pass (see `cmd_reflect`'s step-ordering comments in `cli.rs`):
// a row this pass just DEMOTEd can never be swept into dormancy in the very
// same run regardless of ordering, because dormancy's own age floor
// (`store::DORMANCY_MIN_AGE_DAYS`, 90 days) is well above this pass's
// 3-day settling period — a row young enough to still be a curation
// candidate is never old enough to qualify for dormancy yet.

/// Settling period before an unreviewed row is judged: gives session
/// engagement (touch, via `mach kb ingest-sessions`) a chance to accumulate
/// before a model has to guess from static content alone. A row younger
/// than this is simply left for a later run.
pub const CURATION_MIN_AGE_DAYS: f64 = 3.0;
/// At most this many candidate rows are judged per `mach kb reflect` run —
/// same bounded-cost rationale as `DEDUPE_MAX_PAIRS_PER_RUN`.
pub const CURATION_MAX_PER_RUN: usize = 12;
/// An unreviewed row touched at least this many times (by engagement-gated
/// session reinforcement — see `store::touch`/`ingest.rs`) auto-promotes
/// with no LLM call at all: having actually been used twice is stronger
/// evidence than a model's guess from content alone.
pub const CURATION_ENGAGEMENT_FAST_PATH: i64 = 2;
/// Importance a DEMOTEd row is pinned to (well under
/// `store::DORMANCY_MAX_IMPORTANCE`, so ordinary dormancy criteria still
/// pick it up in due course). Never deletes — organic decay finishes the
/// job later.
pub const CURATION_DEMOTE_IMPORTANCE: i64 = 2;

/// Outcome of parsing the curation judge's reply for one candidate row.
/// Unlike the dedupe/contradiction verdicts, there is no `Malformed`
/// variant here: an empty reply, an unrecognized line, or a judge call that
/// failed outright are all treated identically to `Leave` by the caller
/// (`run_curation_pass` in `cli.rs`) — nothing is recorded either way, so
/// the row simply re-enters the candidate pool on a later run rather than
/// being silenced. That's a deliberate difference from dedupe/contradiction:
/// those record a genuine `Distinct`/`BOTH_HOLD` verdict as "seen" so the
/// same pair is never re-asked, but a curation candidate has no paired
/// judge to avoid re-asking — leaving it exactly as it is IS the correct
/// outcome for "can't tell yet," so there's nothing to distinguish a
/// deliberate `LEAVE` from a malformed reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurationVerdict {
    /// Durable, coherent with what's already known, plausibly useful weeks
    /// or months from now — promote it (`reviewed = 1`; the search penalty
    /// goes away).
    Promote,
    /// Can't yet tell — leave it exactly as it is; it re-enters the pool on
    /// a later run once more evidence (age, engagement) has accumulated.
    Leave,
    /// Transient, noise, or incoherent junk — demote it
    /// (`CURATION_DEMOTE_IMPORTANCE` + halved stability). Never deleted;
    /// organic decay (dormancy) finishes the job.
    Demote,
}

/// Parses the curation judge's reply. Scans line by line (tolerating
/// leading chatter, like the other reflect-pass parsers) for the first
/// recognized token; anything else — empty output, an unrecognized line —
/// defaults to `Leave`, which is exactly the right behavior for "can't
/// tell" (see `CurationVerdict`'s own doc comment for why there's no
/// separate `Malformed` case here).
pub fn parse_curation_verdict(output: &str) -> CurationVerdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if upper.starts_with("PROMOTE") {
            return CurationVerdict::Promote;
        }
        if upper.starts_with("LEAVE") {
            return CurationVerdict::Leave;
        }
        if upper.starts_with("DEMOTE") {
            return CurationVerdict::Demote;
        }
    }
    CurationVerdict::Leave
}

/// Builds the curation pass's one-haiku-call prompt: the candidate's fact,
/// source class, age, engagement evidence (`access_count` — a session
/// having actually touched it is stronger signal than any of the rest), and
/// its top-3 semantic neighbors (a coherence check against what the store
/// already believes) — asks for a single PROMOTE/LEAVE/DEMOTE verdict.
/// `neighbors` are `(id, content)`.
pub fn build_curation_prompt(
    content: &str,
    source: Option<&str>,
    age_days: f64,
    access_count: i64,
    neighbors: &[(i64, String)],
) -> String {
    let mut s = String::new();
    s.push_str(
        "You are curating a personal knowledge bank's auto-captured candidate queue. Decide \
         whether the fact below is worth keeping as a durable memory.\n\n",
    );
    s.push_str(&format!(
        "Candidate (source: {}, age: {:.0} days):\n{}\n\n",
        source.unwrap_or("unknown"),
        age_days,
        content
    ));
    let engagement = if access_count > 0 {
        format!("sessions have engaged this {} time(s)", access_count)
    } else {
        "never engaged".to_string()
    };
    s.push_str(&format!("Engagement evidence: {}\n\n", engagement));
    if neighbors.is_empty() {
        s.push_str("No closely related memories exist yet.\n\n");
    } else {
        s.push_str("Closely related existing memories (for coherence -- does this fit what's already known?):\n");
        for (id, text) in neighbors {
            s.push_str(&format!("#{}: {}\n", id, text));
        }
        s.push('\n');
    }
    s.push_str(
        "Reply with exactly one line and no other text: PROMOTE, LEAVE, or DEMOTE.\n\
         PROMOTE -- durable, coherent with what's known, plausibly useful weeks or months from now.\n\
         LEAVE -- can't yet tell; it will be re-judged on a later run once more evidence accumulates.\n\
         DEMOTE -- transient, noise, or incoherent junk; it is never deleted, just weakened.\n",
    );
    s
}

// --- strength review sampler ---

/// Outcome of parsing the strength-review judge's reply for one sampled
/// memory checked against its top-3 semantic neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrengthVerdict {
    /// Nothing among the neighbors casts doubt — bump `last_verified_at`.
    Stands,
    /// The memory is aging or weakening but isn't directly contradicted by
    /// any neighbor shown — halve its `stability` (so it decays faster)
    /// without tombstoning it; actual supersession stays the contradiction
    /// patrol's job, not this sampler's.
    Stale,
    /// A neighbor directly contradicts the memory — `with_id` names it, so
    /// the caller can route the pair into the exact same judge the
    /// contradiction patrol itself uses, rather than deciding supersession
    /// here.
    Conflict { with_id: i64 },
    /// Empty/prose output, an unrecognized line, or a `CONFLICT` naming an
    /// id outside the shown neighbors — never recorded either way, so this
    /// memory stays due and gets resampled on a later run.
    Malformed,
}

/// Parses the strength-review judge's reply for a memory checked against
/// `neighbor_ids`. Scans line by line for `STANDS`, `STALE`, or `CONFLICT
/// <id>` (tolerating leading chatter and a `#`-prefixed id, like the other
/// judges in this module); a `CONFLICT` naming an id outside
/// `neighbor_ids` — a hallucinated or stale reference — is `Malformed`
/// rather than trusted.
pub fn parse_strength_verdict(output: &str, neighbor_ids: &[i64]) -> StrengthVerdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let upper = line.to_uppercase();
        if upper == "STANDS" {
            return StrengthVerdict::Stands;
        }
        if upper == "STALE" {
            return StrengthVerdict::Stale;
        }
        if let Some(rest) = upper.strip_prefix("CONFLICT") {
            if let Some(id) = extract_dedupe_id(rest) {
                return if neighbor_ids.contains(&id) {
                    StrengthVerdict::Conflict { with_id: id }
                } else {
                    StrengthVerdict::Malformed
                };
            }
        }
    }
    StrengthVerdict::Malformed
}

/// Builds the strength-review sampler's one-haiku-call prompt: the sampled
/// memory (with its recorded date) and its top-3 semantic neighbors, asking
/// whether any of them shows newer evidence that the memory is stale or
/// wrong.
pub fn build_strength_review_prompt(claim: &str, claim_date: &str, neighbors: &[(i64, String)]) -> String {
    let mut s = String::new();
    s.push_str("A durable memory from a personal knowledge bank is due for a periodic strength check.\n\n");
    s.push_str(&format!("Memory (recorded {}):\n{}\n\n", claim_date, claim));
    s.push_str("Its closest related memories currently in the store:\n");
    for (id, content) in neighbors {
        s.push_str(&format!("[{}] {}\n", id, content));
    }
    s.push_str(
        "\nDoes any of these show newer evidence that the memory above is stale or wrong? Reply with \
         exactly one line and nothing else: STANDS (still holds), STALE (aging or weakening, but not \
         directly contradicted by any memory shown), or CONFLICT <id> (that memory directly \
         contradicts it -- name its id).\n",
    );
    s
}


// --- graph extraction: entities/relations from truth-maintained memories ---

/// Reuse threshold for entity resolution during graph extraction: an
/// embedding match at or above this cosine similarity to an existing
/// entity's name embedding reuses that entity rather than minting a
/// near-duplicate ("Claude" vs "claude" is already caught by the
/// case-insensitive exact-name index; this is for near-spellings/typos that
/// exact match misses). Deliberately a high floor -- entity names are
/// short, so even a fairly high similarity can still be two genuinely
/// different short names; only a very close match is treated as "the same
/// thing".
pub const ENTITY_RESOLUTION_SIM_THRESHOLD: f32 = 0.85;

/// At most this many not-yet-extracted memories are offered to the
/// graph-extraction judge per `mach kb reflect` run -- same bounded-cost
/// rationale as `CURATION_MAX_PER_RUN`. A large backlog (this feature's own
/// one-time backfill over every pre-existing memory, say) simply drains
/// across several runs in stable (oldest-first) order rather than being
/// reshuffled.
pub const GRAPH_EXTRACTION_MAX_PER_RUN: usize = 40;

/// At most this many triples are accepted from one extraction reply, even
/// if the model returns more -- keeps a single reply gone wrong from
/// flooding the graph.
pub const GRAPH_EXTRACTION_MAX_TRIPLES: usize = 4;

/// Default confidence for a triple whose own confidence field the model
/// left out or wrote unparseably -- not zero (a triple the model bothered
/// to extract is presumably not its least confident guess) but below a
/// clean, deliberate 1.0.
pub const GRAPH_EXTRACTION_DEFAULT_CONFIDENCE: f64 = 0.6;

/// Confidence penalty applied to an edge extracted from a memory that was
/// still unreviewed at extraction time -- mirrors
/// `store::UNREVIEWED_SEARCH_PENALTY`'s "organic, not gated" rationale for
/// raw memories: an edge from an unreviewed fact is still real evidence,
/// just less vouched-for yet.
pub const GRAPH_EXTRACTION_UNREVIEWED_PENALTY: f64 = 0.85;

/// One entity/relation triple as extracted from a single memory's content,
/// before entity resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedTriple {
    pub src_name: String,
    pub src_kind: Option<String>,
    pub predicate: String,
    pub dst_name: String,
    pub dst_kind: Option<String>,
    pub confidence: f64,
}

/// Extraction quality guard, shared by the single-fact and batch prompt
/// builders -- added after the backfill poisoned the graph twice: a fact
/// describing the "Moses-to-Ivar boss supersession test" (memory #185, a
/// fact ABOUT the memory system's own testing, not a real supersession)
/// produced a hallucinated `Ivar --supersedes-as-boss--> Moses` edge, and a
/// vague fact about the user's chain of command produced a garbage
/// `user --has-boss--> boss` edge naming a placeholder role rather than an
/// actual person. Both invalidated manually (relation ids 352, 372); this
/// guard is the prompt-level fix so the extraction judge doesn't repeat
/// either mistake.
fn extraction_quality_guard() -> &'static str {
    "Do not extract any relation from a fact that is merely describing a test, hypothetical, \
     example, or the memory system's own mechanics (e.g. a fact explaining how a supersession or \
     dedupe test works, narrating an example scenario, or documenting this very knowledge-bank \
     feature) -- such a fact yields NO relational edges about the people or things it happens to \
     mention, even if it names them by their real names.\n\n\
     Never invent a generic-role entity such as \"boss\", \"the user's boss\", or \"the project\" \
     as SRC_NAME or DST_NAME -- an entity must be an actual named thing (a real person's name, a \
     project's name, a technology's name, etc.), never a placeholder role or description. The \
     reserved name \"user\" is the one exception.\n\n"
}

/// Builds the graph-extraction judge's one-haiku-call prompt for a single
/// memory: asks for 0-4 entity/relation triples the fact actually states.
/// Entities are explicitly not just people, and affective/behavioral
/// predicates are explicitly allowed and wanted (per the user's own framing
/// of this feature: "User -> angry -> claude way of doing requests" is as
/// legitimate an edge as "user works-on Umoja"). "user" is reserved for the
/// user themself, so the model names them consistently rather than
/// inventing a synonym.
pub fn build_extraction_prompt(memory_content: &str) -> String {
    let mut s = String::new();
    s.push_str(
        "You extract a durable knowledge graph from a personal knowledge bank. An entity is any \
         durable, nameable thing -- not just a person: a project, a technology, a game engine, a \
         practice, a concept, an organization. A relation connects two entities with a short verb \
         phrase, including affective or behavioral relations (e.g. \"frustrated-by\", \"prefers\", \
         \"boss-of\", \"works-on\", \"built-with\", \"part-of\") -- these are explicitly allowed and \
         wanted, not just factual/organizational links.\n\n\
         The special entity name \"user\" is reserved for the user themself -- use it whenever the \
         fact is about the user directly, rather than inventing a name.\n\n",
    );
    s.push_str(extraction_quality_guard());
    s.push_str("Fact:\n");
    s.push_str(memory_content);
    s.push_str(
        "\n\nList only relations this fact actually states -- never infer or guess beyond what's \
         written. Reply with 0 to 4 lines, one relation per line, in exactly this form and no other \
         text:\n\
         SRC_NAME | SRC_KIND | PREDICATE | DST_NAME | DST_KIND | CONFIDENCE\n\n\
         SRC_NAME/DST_NAME -- short canonical names (\"Moses\", \"Umoja\", \"C++\"), never full \
         sentences.\n\
         SRC_KIND/DST_KIND -- a short free-text label; person, project, technology, concept, \
         practice, or organization is a good default vocabulary, but use whatever fits.\n\
         PREDICATE -- 1 to 3 words, lowercase, hyphenated (e.g. \"boss-of\", \"works-on\", \
         \"built-with\", \"frustrated-by\", \"prefers\").\n\
         CONFIDENCE -- your confidence this relation is correctly stated, 0.0 to 1.0.\n\n\
         If this fact states no relation between two nameable things, reply with exactly: NONE\n",
    );
    s
}

/// Normalizes a predicate to the extraction rule's own shape (lowercase,
/// hyphenated) regardless of exactly how the model formatted it -- spaces
/// collapse to hyphens -- so `store::relations_conflicting_with`'s exact
/// (case-insensitive) predicate match stays reliable even against a
/// slightly off-format reply.
fn normalize_predicate(raw: &str) -> String {
    raw.trim().to_lowercase().split_whitespace().collect::<Vec<_>>().join("-")
}

/// Parses the extraction judge's reply into up to
/// `GRAPH_EXTRACTION_MAX_TRIPLES` triples. Deliberately permissive -- a
/// malformed line is simply skipped rather than failing the whole reply
/// (this is still a successful LLM call, so the memory is marked extracted
/// either way); `NONE`, empty output, or a reply with no parseable lines all
/// produce an empty vec, never an error.
pub fn parse_extraction(output: &str) -> Vec<ExtractedTriple> {
    let mut out = Vec::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.eq_ignore_ascii_case("none") {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(|p| p.trim()).collect();
        if parts.len() != 6 {
            continue;
        }
        let src_name = parts[0];
        let predicate_raw = parts[2];
        let dst_name = parts[3];
        if src_name.is_empty() || dst_name.is_empty() || predicate_raw.is_empty() {
            continue;
        }
        let src_kind = if parts[1].is_empty() { None } else { Some(parts[1].to_string()) };
        let dst_kind = if parts[4].is_empty() { None } else { Some(parts[4].to_string()) };
        let confidence = parts[5]
            .parse::<f64>()
            .ok()
            .map(|c| c.clamp(0.0, 1.0))
            .unwrap_or(GRAPH_EXTRACTION_DEFAULT_CONFIDENCE);
        out.push(ExtractedTriple {
            src_name: src_name.to_string(),
            src_kind,
            predicate: normalize_predicate(predicate_raw),
            dst_name: dst_name.to_string(),
            dst_kind,
            confidence,
        });
        if out.len() >= GRAPH_EXTRACTION_MAX_TRIPLES {
            break;
        }
    }
    out
}

// --- graph extraction: batched calls ---
//
// The corpus backfill (348 entities, 447 relations over the whole pre-
// existing memory store) took hours under the one-`claude -p`-spawn-per-
// memory design above: each spawn pays 5-15s of process overhead alone,
// dwarfing the model's own latency. Batching turns a run's whole candidate
// set into a handful of calls instead of one per memory, without changing
// any of the per-memory extraction semantics `parse_extraction` and
// `build_extraction_prompt` already established -- `parse_batch_extraction`
// applies the exact same per-triple parsing rules, just per numbered fact.

/// This many candidate memories are offered to one batched haiku call,
/// rather than one call per memory -- picked from the middle of the user's
/// own "10-15 memories per call" target. `reflect::GRAPH_EXTRACTION_MAX_PER_RUN`
/// (40) still bounds how many memories a whole run examines; this only
/// changes how many `claude -p` spawns that costs (`ceil(40 / 12)` = 4
/// batches instead of 40 individual calls).
pub const GRAPH_EXTRACTION_BATCH_SIZE: usize = 12;

/// A batch call parses and replies about several facts at once, so it
/// needs more headroom than a single-fact call's `TIMEOUT_HAIKU`.
pub const TIMEOUT_HAIKU_BATCH: Duration = Duration::from_secs(60);

/// Builds the graph-extraction judge's one-call prompt for a whole batch of
/// memories at once: each fact is numbered, and the model is required to
/// address every number in its reply (either relation lines or an explicit
/// `N: NONE`) so the caller can tell "this fact yielded zero relations"
/// (`NONE`, a legitimate outcome) apart from "the model's reply never
/// touched this fact at all" (absent from the reply entirely, a parse
/// failure worth retrying) -- see `parse_batch_extraction`'s own doc
/// comment for how that distinction is used.
pub fn build_batch_extraction_prompt(facts: &[&str]) -> String {
    let mut s = String::new();
    s.push_str(
        "You extract a durable knowledge graph from a personal knowledge bank. An entity is any \
         durable, nameable thing -- not just a person: a project, a technology, a game engine, a \
         practice, a concept, an organization. A relation connects two entities with a short verb \
         phrase, including affective or behavioral relations (e.g. \"frustrated-by\", \"prefers\", \
         \"boss-of\", \"works-on\", \"built-with\", \"part-of\") -- these are explicitly allowed and \
         wanted, not just factual/organizational links.\n\n\
         The special entity name \"user\" is reserved for the user themself -- use it whenever a \
         fact is about the user directly, rather than inventing a name.\n\n",
    );
    s.push_str(extraction_quality_guard());
    s.push_str("Here are several facts, numbered:\n\n");
    for (i, fact) in facts.iter().enumerate() {
        s.push_str(&format!("{}: {}\n", i + 1, fact));
    }
    s.push_str(&format!(
        "\nFor EACH fact number above (1 to {}), list only relations that fact actually states -- \
         never infer or guess beyond what's written. Reply with, for every fact number, either one \
         or more relation lines or a single NONE line, each prefixed with that fact's number and a \
         colon, in exactly this form and no other text:\n\
         N: SRC_NAME | SRC_KIND | PREDICATE | DST_NAME | DST_KIND | CONFIDENCE\n\
         N: NONE\n\n\
         SRC_NAME/DST_NAME -- short canonical names (\"Moses\", \"Umoja\", \"C++\"), never full \
         sentences.\n\
         SRC_KIND/DST_KIND -- a short free-text label; person, project, technology, concept, \
         practice, or organization is a good default vocabulary, but use whatever fits.\n\
         PREDICATE -- 1 to 3 words, lowercase, hyphenated (e.g. \"boss-of\", \"works-on\", \
         \"built-with\", \"frustrated-by\", \"prefers\").\n\
         CONFIDENCE -- your confidence this relation is correctly stated, 0.0 to 1.0.\n\n\
         Every fact number from 1 to {} MUST appear at least once in your reply -- use `N: NONE` \
         for a fact that states no relation between two nameable things. Do not skip a number.\n",
        facts.len(),
        facts.len()
    ));
    s
}

/// Parses a batch extraction reply into a map from fact number (1-based,
/// matching `build_batch_extraction_prompt`'s own numbering) to the triples
/// extracted for it. Per-fact attribution, strict about which fact a line
/// belongs to: a line is only ever counted for the fact number it names.
///
/// A fact number is present in the returned map only when the reply
/// actually addressed it -- an explicit `N: NONE` (mapped to an empty
/// `Vec`) or at least one well-formed relation line. A malformed relation
/// line for a fact that has no other valid line for it is simply dropped,
/// same as `parse_extraction`'s own permissiveness -- but critically, that
/// dropped line does NOT insert an entry into the map. This is the house
/// rule the caller (`cli::run_graph_extraction_pass`) depends on: a fact
/// whose triples fail to parse is never watermarked as extracted, so it's
/// retried on a later run rather than silently treated as "no relations" —
/// exactly like a whole failed LLM call already isn't watermarked (see
/// `Memory::graph_extracted_at`'s own doc comment), just decided per-fact
/// instead of per-call now that one call covers many facts.
pub fn parse_batch_extraction(output: &str, num_facts: usize) -> HashMap<usize, Vec<ExtractedTriple>> {
    let mut out: HashMap<usize, Vec<ExtractedTriple>> = HashMap::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let (num_str, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let num: usize = match num_str.trim().parse() {
            Ok(n) if n >= 1 && n <= num_facts => n,
            _ => continue,
        };
        let rest = rest.trim();
        if rest.eq_ignore_ascii_case("none") {
            out.entry(num).or_default();
            continue;
        }
        let parts: Vec<&str> = rest.split('|').map(|p| p.trim()).collect();
        if parts.len() != 6 {
            continue; // malformed -- never inserted, so this fact stays unaddressed unless another line covers it
        }
        let src_name = parts[0];
        let predicate_raw = parts[2];
        let dst_name = parts[3];
        if src_name.is_empty() || dst_name.is_empty() || predicate_raw.is_empty() {
            continue;
        }
        let src_kind = if parts[1].is_empty() { None } else { Some(parts[1].to_string()) };
        let dst_kind = if parts[4].is_empty() { None } else { Some(parts[4].to_string()) };
        let confidence = parts[5]
            .parse::<f64>()
            .ok()
            .map(|c| c.clamp(0.0, 1.0))
            .unwrap_or(GRAPH_EXTRACTION_DEFAULT_CONFIDENCE);
        let entry = out.entry(num).or_default();
        if entry.len() < GRAPH_EXTRACTION_MAX_TRIPLES {
            entry.push(ExtractedTriple {
                src_name: src_name.to_string(),
                src_kind,
                predicate: normalize_predicate(predicate_raw),
                dst_name: dst_name.to_string(),
                dst_kind,
                confidence,
            });
        }
    }
    out
}

/// Outcome of the id-free edge-contradiction judge (`mach kb reflect`'s
/// graph-extraction pass, on every new edge that shares an entity and
/// predicate with an existing active one). No `ConflictRetro` counterpart
/// here (unlike the memory-level patrol's `ContradictionVerdict`): an
/// edge's own `created_at`/insertion order is this graph's only notion of
/// "current", there is no separate retrospective-note distinction to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeContradictionVerdict {
    /// The two edges describe the same role/relationship and cannot both
    /// be current. The caller always treats the newly inserted edge as the
    /// winner (it is, by construction, the more recently created one) and
    /// tombstones the other -- the model is never asked to name a winner
    /// (see `build_edge_contradiction_prompt`'s own doc comment for why).
    Conflict,
    /// Both edges are compatible (different entities, or a relation that
    /// isn't exclusive) -- neither is touched.
    BothHold,
    /// The judge couldn't tell, or its reply was empty or unrecognized --
    /// neither edge is touched, same as `BothHold`.
    Unclear,
}

/// Parses the edge-contradiction judge's reply -- same tolerant, id-free
/// line scan `parse_contradiction_verdict` uses for the memory-level
/// patrol, just a three-way verdict (no `CONFLICT_RETRO`).
pub fn parse_edge_contradiction_verdict(output: &str) -> EdgeContradictionVerdict {
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        match line.to_uppercase().as_str() {
            "CONFLICT" => return EdgeContradictionVerdict::Conflict,
            "BOTH_HOLD" => return EdgeContradictionVerdict::BothHold,
            "UNCLEAR" => return EdgeContradictionVerdict::Unclear,
            _ => continue,
        }
    }
    EdgeContradictionVerdict::Unclear
}

/// Builds the edge-contradiction judge's one-haiku-call prompt, describing
/// both edges by their entity names and predicate only -- deliberately
/// id-free, the same lesson `build_contradiction_pass_prompt` already
/// learned the hard way for memories (haiku reliably *inverts* a
/// "SUPERSEDES <id>"-shaped answer, favoring the grammatically-read loser
/// over the intended winner, no matter how the instruction is worded). The
/// model is never asked to name a winner here either -- only whether the two
/// edges conflict. Winner resolution (the newly inserted edge always wins)
/// happens entirely in Rust. `a`/`b` are each `(src_name, predicate,
/// dst_name)`.
pub fn build_edge_contradiction_prompt(a: (&str, &str, &str), b: (&str, &str, &str)) -> String {
    let mut s = String::new();
    s.push_str(
        "You are checking a personal knowledge graph for contradictions between two edges that \
         share an entity and the same kind of relation -- decide the single correct relationship \
         between them.\n\n",
    );
    s.push_str(&format!("Edge 1: {} --{}--> {}\n", a.0, a.1, a.2));
    s.push_str(&format!("Edge 2: {} --{}--> {}\n\n", b.0, b.1, b.2));
    s.push_str(
        "Reply with exactly one line and no other text: CONFLICT, BOTH_HOLD, or UNCLEAR.\n\
         CONFLICT -- these describe the same role or relationship and cannot both be current (e.g. \
         two different people both currently \"boss-of\" the same person). You don't need to say \
         which one wins; the more recently recorded edge will automatically be treated as current.\n\
         BOTH_HOLD -- these are compatible and can both be true at once (different entities, or a \
         relation that isn't exclusive).\n\
         UNCLEAR -- you cannot tell from what's shown.\n",
    );
    s
}

// --- graph extraction: batched edge-conflict judging ---
//
// The old path ran one haiku call per new edge that shared an entity and
// predicate with an existing active one -- on a backfill that inserts
// hundreds of edges, most of them never conflicting with anything, that's
// still hundreds of wasted-on-nothing spawns whenever even a handful do
// conflict. Collecting every candidate conflict pair a whole run produced
// and judging them in ONE call cuts that to (at most) one spawn regardless
// of how many pairs there are.

/// Builds the edge-contradiction judge's one-call prompt for a whole batch
/// of candidate conflict pairs at once: each pair is numbered, id-free (same
/// "never ask the model to name a winner" rationale as the single-pair
/// prompt), and the model is asked to address every number.
pub fn build_batch_edge_contradiction_prompt(pairs: &[((&str, &str, &str), (&str, &str, &str))]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are checking a personal knowledge graph for contradictions between pairs of edges. \
         Each numbered pair below shares an entity and the same kind of relation -- decide, for \
         EACH pair, the single correct relationship between its two edges.\n\n",
    );
    for (i, (a, b)) in pairs.iter().enumerate() {
        s.push_str(&format!("Pair {}:\n  Edge 1: {} --{}--> {}\n  Edge 2: {} --{}--> {}\n\n", i + 1, a.0, a.1, a.2, b.0, b.1, b.2));
    }
    s.push_str(&format!(
        "For EACH pair number above (1 to {}), reply with exactly one line in the form `N: VERDICT`, \
         where VERDICT is one of CONFLICT, BOTH_HOLD, or UNCLEAR. Every pair number from 1 to {} \
         MUST appear exactly once.\n\
         CONFLICT -- the two edges in that pair describe the same role or relationship and cannot \
         both be current (e.g. two different people both currently \"boss-of\" the same person). \
         You don't need to say which one wins; the more recently recorded edge will automatically \
         be treated as current.\n\
         BOTH_HOLD -- the two edges are compatible and can both be true at once (different \
         entities, or a relation that isn't exclusive).\n\
         UNCLEAR -- you cannot tell from what's shown.\n",
        pairs.len(),
        pairs.len()
    ));
    s
}

/// Parses a batch edge-contradiction reply into a map from pair number
/// (1-based, matching `build_batch_edge_contradiction_prompt`'s own
/// numbering) to its verdict. A pair number absent from the reply is simply
/// absent from the map -- unlike the extraction batch's own per-fact
/// watermark concern, there is no retry queue for an edge-conflict check
/// (it's a one-time judgment made while a new edge is being inserted, not a
/// backlog drained across runs), so the caller treats a missing verdict
/// exactly like an explicit `UNCLEAR`: leave both edges active.
pub fn parse_batch_edge_contradiction_verdicts(output: &str, num_pairs: usize) -> HashMap<usize, EdgeContradictionVerdict> {
    let mut out = HashMap::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let (num_str, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let num: usize = match num_str.trim().parse() {
            Ok(n) if n >= 1 && n <= num_pairs => n,
            _ => continue,
        };
        let verdict = match rest.trim().to_uppercase().as_str() {
            "CONFLICT" => EdgeContradictionVerdict::Conflict,
            "BOTH_HOLD" => EdgeContradictionVerdict::BothHold,
            "UNCLEAR" => EdgeContradictionVerdict::Unclear,
            _ => continue,
        };
        out.entry(num).or_insert(verdict);
    }
    out
}

// --- curation pass: schema-accelerated consolidation ---
//
// Tse-style schema-linked consolidation (Tse et al. 2007, 2011): new
// information consistent with an existing "schema" -- here, the insight/
// theme layer `mach kb reflect` has already built up -- consolidates faster
// than an orphan fact with nothing to attach to. Applied at the one point
// `mach kb reflect` already judges a fact's coherence with what's known
// (the curation pass's PROMOTE verdict, and its engagement fast path): no
// extra LLM call, just an extra stability multiplier on top of whatever the
// row already had.

/// Stability multiplier applied when a promoted candidate sits close enough
/// (`CURATION_SCHEMA_COHERENCE_MIN_SIM`) to an existing active insight or
/// theme -- see `cli::apply_schema_fast_path`'s own doc comment for how
/// it's applied (same 365-day cap `store::touch`/`halve_stability` use).
pub const CURATION_SCHEMA_STABILITY_MULTIPLIER: f64 = 1.5;
/// Minimum cosine similarity between a promoted candidate and its closest
/// active insight/theme for the multiplier above to apply -- reuses
/// `META_CLUSTER_MIN_SIM`'s own "genuinely related, not just superficially
/// similar" bar (the same threshold meta-reflection already uses to decide
/// two insights "genuinely share" a theme).
pub const CURATION_SCHEMA_COHERENCE_MIN_SIM: f32 = META_CLUSTER_MIN_SIM;

// --- graph hygiene pass: batched entity-merge judging ---
//
// Runs inside `mach kb reflect`, right after edge extraction and before
// dormancy. Candidate pairs (`store::entity_merge_candidate_pairs`) are
// deterministic (name-embedding similarity or a case/punctuation-
// insensitive exact match); only the SAME/DIFFERENT call itself needs a
// judge, and every candidate pair collected in one run is batched into as
// few calls as `ENTITY_MERGE_BATCH_SIZE` allows -- the same batching
// philosophy `build_batch_edge_contradiction_prompt` already established
// for edge conflicts, reused here rather than one call per pair.

/// Minimum name-embedding cosine similarity for two active entities to be a
/// merge candidate -- deliberately higher than
/// `ENTITY_RESOLUTION_SIM_THRESHOLD` (0.85, extraction's own "reuse this
/// entity" floor): a *pre-existing* pair of entities clearing this bar is
/// stronger evidence of an actual duplicate than a single fresh name being
/// matched against the store, so this pass can afford to be pickier before
/// spending a judge call on it.
pub const ENTITY_MERGE_MIN_SIM: f32 = 0.9;
/// This many candidate entity pairs are offered to one batched haiku call --
/// same bucket size as `GRAPH_EXTRACTION_BATCH_SIZE`, picked for the same
/// "several judgments per spawn" reason.
pub const ENTITY_MERGE_BATCH_SIZE: usize = 12;
/// At most this many candidate pairs are judged per `mach kb reflect` run --
/// same bounded-cost rationale as `DEDUPE_MAX_PAIRS_PER_RUN`.
pub const ENTITY_MERGE_MAX_PAIRS_PER_RUN: usize = 20;

/// Outcome of the id-free entity-merge judge for one candidate pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityMergeVerdict {
    /// The two entities are the same real-world thing under two names/
    /// spellings -- the caller keeps the older (lower-id) entity, repoints
    /// every edge off the newer one, and deletes the newer entity row.
    Same,
    /// Genuinely different things that merely look similar -- recorded in
    /// `entity_merge_seen` so the pair is never re-asked.
    Different,
}

/// Builds the entity-merge judge's one-call prompt for a whole batch of
/// candidate pairs at once: each pair described by name, kind, and up to 2
/// sample edges per entity (`store::sample_relation_descriptions`) --
/// deliberately id-free, same rationale as every other batched judge prompt
/// in this module. `pairs` are `((name, kind, sample_edges), (name, kind,
/// sample_edges))` per candidate.
pub fn build_batch_entity_merge_prompt(pairs: &[((&str, &str, &[String]), (&str, &str, &[String]))]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are checking a personal knowledge graph for duplicate entities -- the same real-world \
         person, project, or thing recorded twice under two different names or spellings. Each \
         numbered pair below was flagged as a candidate by name similarity -- decide, for EACH pair, \
         whether the two entities are the SAME thing or DIFFERENT things.\n\n",
    );
    for (i, (a, b)) in pairs.iter().enumerate() {
        s.push_str(&format!("Pair {}:\n  Entity A: \"{}\" (kind: {})\n", i + 1, a.0, a.1));
        for edge in a.2 {
            s.push_str(&format!("    - {}\n", edge));
        }
        s.push_str(&format!("  Entity B: \"{}\" (kind: {})\n", b.0, b.1));
        for edge in b.2 {
            s.push_str(&format!("    - {}\n", edge));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "For EACH pair number above (1 to {}), reply with exactly one line in the form `N: VERDICT`, \
         where VERDICT is SAME or DIFFERENT. Every pair number from 1 to {} MUST appear exactly once.\n\
         SAME -- the same real-world thing under two names/spellings (e.g. \"Umoja\" and \"umoja \
         project\", or \"C++\" and \"c ++\").\n\
         DIFFERENT -- genuinely different things that merely look similar.\n",
        pairs.len(),
        pairs.len()
    ));
    s
}

/// Parses a batch entity-merge reply into a map from pair number (1-based,
/// matching `build_batch_entity_merge_prompt`'s own numbering) to its
/// verdict -- same tolerant `N: VERDICT` line scan as
/// `parse_batch_edge_contradiction_verdicts`. A pair number absent from the
/// reply is simply absent from the map; the caller leaves an unaddressed
/// pair untouched (neither merged nor marked seen), same "never watermark a
/// judgment that never actually happened" rule the other batched passes
/// follow.
pub fn parse_batch_entity_merge_verdicts(output: &str, num_pairs: usize) -> HashMap<usize, EntityMergeVerdict> {
    let mut out = HashMap::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let (num_str, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let num: usize = match num_str.trim().parse() {
            Ok(n) if n >= 1 && n <= num_pairs => n,
            _ => continue,
        };
        let verdict = match rest.trim().to_uppercase().as_str() {
            "SAME" => EntityMergeVerdict::Same,
            "DIFFERENT" => EntityMergeVerdict::Different,
            _ => continue,
        };
        out.entry(num).or_insert(verdict);
    }
    out
}

// --- one-time (or repeatable) edge audit: `mach kb graph audit` ---
//
// Ships as its own reusable subcommand rather than folded into `mach kb
// reflect`'s own passes: it re-examines every ACTIVE edge each time it's
// run (no backlog watermark, since a clean edge costs nothing to re-check),
// which is a different shape from reflect's drained-backlog passes above.

/// This many active edges are offered to one batched haiku call at a time.
pub const GRAPH_AUDIT_BATCH_SIZE: usize = 20;

/// Outcome of the audit judge's verdict for one edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphAuditVerdict {
    /// A genuine, correctly-derived relation between two real named things.
    Keep,
    /// Derived from a fact that was merely describing a test, hypothetical,
    /// example, or the memory system's own mechanics -- not a real fact
    /// about the people/things it happens to mention (the same poisoning
    /// this pattern already guards extraction against going forward -- see
    /// `extraction_quality_guard` -- this is the backfill's one-time
    /// catch-up audit over edges that predate that guard).
    Poisoned,
    /// One endpoint is a generic-role placeholder (e.g. "boss", "the
    /// project") rather than an actual named thing.
    Generic,
}

/// Builds the audit judge's one-call prompt for a whole batch of active
/// edges at once: each edge described by its endpoint names/kinds,
/// predicate, and an evidence snippet -- id-free, same rationale as every
/// other batched judge prompt in this module. `edges` are `(src_name,
/// src_kind, predicate, dst_name, dst_kind, evidence_snippet)`.
pub fn build_batch_graph_audit_prompt(edges: &[(&str, &str, &str, &str, &str, &str)]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are auditing a personal knowledge graph's existing edges, some of which predate a \
         quality guard added after two real poisoning incidents. Decide, for EACH numbered edge \
         below, whether it should be KEPT, or whether it's POISONED or GENERIC.\n\n",
    );
    s.push_str(extraction_quality_guard());
    for (i, (src_name, src_kind, predicate, dst_name, dst_kind, evidence)) in edges.iter().enumerate() {
        s.push_str(&format!(
            "Edge {}: {} ({}) --{}--> {} ({})\n  evidence: {}\n\n",
            i + 1,
            src_name,
            src_kind,
            predicate,
            dst_name,
            dst_kind,
            evidence
        ));
    }
    s.push_str(&format!(
        "For EACH edge number above (1 to {}), reply with exactly one line in the form `N: VERDICT`, \
         where VERDICT is KEEP, POISONED, or GENERIC. Every edge number from 1 to {} MUST appear \
         exactly once.\n\
         KEEP -- a genuine, correctly-derived relation between two real named things.\n\
         POISONED -- derived from a fact merely describing a test, hypothetical, example, or the \
         memory system's own mechanics.\n\
         GENERIC -- one endpoint is a generic-role placeholder, not an actual named thing.\n",
        edges.len(),
        edges.len()
    ));
    s
}

/// Parses a batch graph-audit reply into a map from edge number (1-based,
/// matching `build_batch_graph_audit_prompt`'s own numbering) to its
/// verdict -- same tolerant `N: VERDICT` line scan as the other batched
/// judges in this module. An edge number absent from the reply is simply
/// absent from the map; the caller treats that identically to an explicit
/// `KEEP` (never destructive on doubt).
pub fn parse_batch_graph_audit_verdicts(output: &str, num_edges: usize) -> HashMap<usize, GraphAuditVerdict> {
    let mut out = HashMap::new();
    for raw_line in output.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let (num_str, rest) = match line.split_once(':') {
            Some(v) => v,
            None => continue,
        };
        let num: usize = match num_str.trim().parse() {
            Ok(n) if n >= 1 && n <= num_edges => n,
            _ => continue,
        };
        let verdict = match rest.trim().to_uppercase().as_str() {
            "KEEP" => GraphAuditVerdict::Keep,
            "POISONED" => GraphAuditVerdict::Poisoned,
            "GENERIC" => GraphAuditVerdict::Generic,
            _ => continue,
        };
        out.entry(num).or_insert(verdict);
    }
    out
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

    // --- opportunistic scheduling: watermark-advance predicate ---

    #[test]
    fn should_advance_watermark_requires_new_material() {
        assert!(!should_advance_watermark(false, false), "nothing new -- never advance, even if nothing failed");
    }

    #[test]
    fn should_advance_watermark_frozen_when_any_llm_call_failed() {
        assert!(!should_advance_watermark(true, true), "offline/degraded mid-run -- must stay retry-able");
    }

    #[test]
    fn should_advance_watermark_true_only_when_new_and_nothing_failed() {
        assert!(should_advance_watermark(true, false));
    }

    // --- nightly dedupe pass: candidate selection ---

    #[test]
    fn dedupe_candidate_pairs_respects_similarity_threshold() {
        let pool = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 1))];
        let new_ids: HashSet<i64> = [2].into_iter().collect();
        let seen: HashSet<(i64, i64)> = HashSet::new();
        // 1 vs 2 share a direction (sim 1.0, clears 0.85); 2 vs 3 are
        // orthogonal (sim 0.0) and must not appear.
        let pairs = dedupe_candidate_pairs(&pool, &new_ids, &seen, 0.85, 10);
        assert_eq!(pairs, vec![(1, 2)]);
    }

    #[test]
    fn dedupe_candidate_pairs_requires_at_least_one_side_new() {
        let pool = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0))];
        let seen: HashSet<(i64, i64)> = HashSet::new();
        // Both old (neither in new_ids) -- even though they're similar,
        // this pair must never surface: rescanning the whole store nightly
        // is exactly what this pass is meant to avoid.
        let pairs = dedupe_candidate_pairs(&pool, &HashSet::new(), &seen, 0.85, 10);
        assert!(pairs.is_empty());
    }

    #[test]
    fn dedupe_candidate_pairs_excludes_already_seen_pairs() {
        let pool = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0))];
        let new_ids: HashSet<i64> = [2].into_iter().collect();
        let seen: HashSet<(i64, i64)> = [(1, 2)].into_iter().collect();
        assert!(dedupe_candidate_pairs(&pool, &new_ids, &seen, 0.85, 10).is_empty());
    }

    #[test]
    fn dedupe_candidate_pairs_caps_and_orders_oldest_first() {
        // Every id shares a direction with every other -- 3 ids yields the
        // 3 pairwise combinations, all qualifying; the cap of 2 must keep
        // the two whose smaller id is lowest, i.e. (1,2) and (1,3), not (2,3).
        let pool = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 0)), (3, unit_vec(4, 0))];
        let new_ids: HashSet<i64> = [1, 2, 3].into_iter().collect();
        let seen: HashSet<(i64, i64)> = HashSet::new();
        let pairs = dedupe_candidate_pairs(&pool, &new_ids, &seen, 0.85, 2);
        assert_eq!(pairs, vec![(1, 2), (1, 3)]);
    }

    // --- nightly dedupe pass: verdict parsing ---

    #[test]
    fn parse_dedupe_verdict_accepts_keep_naming_either_pair_member() {
        assert_eq!(parse_dedupe_verdict("KEEP 12", 12, 45), DedupeVerdict::Keep { keep_id: 12 });
        assert_eq!(parse_dedupe_verdict("keep #45", 12, 45), DedupeVerdict::Keep { keep_id: 45 });
    }

    #[test]
    fn parse_dedupe_verdict_accepts_distinct_case_insensitively() {
        assert_eq!(parse_dedupe_verdict("DISTINCT", 1, 2), DedupeVerdict::Distinct);
        assert_eq!(parse_dedupe_verdict("  distinct  \n", 1, 2), DedupeVerdict::Distinct);
    }

    #[test]
    fn parse_dedupe_verdict_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nKEEP 12";
        assert_eq!(parse_dedupe_verdict(out, 12, 45), DedupeVerdict::Keep { keep_id: 12 });
    }

    #[test]
    fn parse_dedupe_verdict_rejects_id_outside_the_pair() {
        // A hallucinated third id must never be accepted as a keep/winner.
        assert_eq!(parse_dedupe_verdict("KEEP 99", 12, 45), DedupeVerdict::Malformed);
    }

    #[test]
    fn parse_dedupe_verdict_malformed_on_empty_prose_or_missing_id() {
        assert_eq!(parse_dedupe_verdict("", 1, 2), DedupeVerdict::Malformed);
        assert_eq!(parse_dedupe_verdict("I'm not sure.", 1, 2), DedupeVerdict::Malformed);
        assert_eq!(parse_dedupe_verdict("KEEP", 1, 2), DedupeVerdict::Malformed);
        assert_eq!(parse_dedupe_verdict("KEEP banana", 1, 2), DedupeVerdict::Malformed);
    }

    #[test]
    fn build_dedupe_prompt_includes_both_memories_and_sources() {
        let p = build_dedupe_prompt((1, "likes tea", Some("note")), (2, "likes tea a lot", None));
        assert!(p.contains("Memory #1 (source: note):\nlikes tea"));
        assert!(p.contains("Memory #2 (source: unknown):\nlikes tea a lot"));
        assert!(p.contains("KEEP"));
        assert!(p.contains("DISTINCT"));
    }

    // --- curation pass: verdict parsing ---

    #[test]
    fn parse_curation_verdict_accepts_all_three_tokens_case_insensitively() {
        assert_eq!(parse_curation_verdict("PROMOTE"), CurationVerdict::Promote);
        assert_eq!(parse_curation_verdict("  promote  \n"), CurationVerdict::Promote);
        assert_eq!(parse_curation_verdict("LEAVE"), CurationVerdict::Leave);
        assert_eq!(parse_curation_verdict("leave"), CurationVerdict::Leave);
        assert_eq!(parse_curation_verdict("DEMOTE"), CurationVerdict::Demote);
        assert_eq!(parse_curation_verdict("demote"), CurationVerdict::Demote);
    }

    #[test]
    fn parse_curation_verdict_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nPROMOTE";
        assert_eq!(parse_curation_verdict(out), CurationVerdict::Promote);
    }

    #[test]
    fn parse_curation_verdict_defaults_to_leave_on_malformed_or_empty_output() {
        // Deliberately Leave, not a separate Malformed case — see
        // CurationVerdict's own doc comment for why.
        assert_eq!(parse_curation_verdict(""), CurationVerdict::Leave);
        assert_eq!(parse_curation_verdict("I'm not sure about this one."), CurationVerdict::Leave);
        assert_eq!(parse_curation_verdict("PROMOTION"), CurationVerdict::Leave, "must not fuzzy-match a similar word");
    }

    #[test]
    fn build_curation_prompt_includes_content_engagement_and_neighbors() {
        let neighbors = vec![(7i64, "the user drinks tea".to_string())];
        let p = build_curation_prompt("candidate fact", Some("session-digest"), 5.0, 0, &neighbors);
        assert!(p.contains("candidate fact"));
        assert!(p.contains("source: session-digest"));
        assert!(p.contains("age: 5 days"));
        assert!(p.contains("never engaged"));
        assert!(p.contains("#7: the user drinks tea"));
        assert!(p.contains("PROMOTE"));
        assert!(p.contains("LEAVE"));
        assert!(p.contains("DEMOTE"));
    }

    #[test]
    fn build_curation_prompt_reports_engagement_count_when_touched() {
        let p = build_curation_prompt("candidate fact", None, 4.0, 3, &[]);
        assert!(p.contains("engaged this 3 time(s)"));
        assert!(p.contains("No closely related memories exist yet."));
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

    // --- contradiction patrol: band selection ---

    #[test]
    fn contradiction_candidate_pairs_respects_the_band_below_dedupe() {
        // Cosine similarity between two unit vectors at a known angle: build
        // a pair that lands at exactly 0.0 (orthogonal, below the band), a
        // pair at 1.0 (identical direction, above the band), and check the
        // band boundaries themselves with hand-picked vectors.
        let below = vec![(1, unit_vec(4, 0)), (2, unit_vec(4, 1))]; // sim 0.0 -- below CONTRADICTION_MIN_SIM
        let above = vec![(3, unit_vec(4, 0)), (4, unit_vec(4, 0))]; // sim 1.0 -- at/above CONTRADICTION_MAX_SIM
        let new_ids: HashSet<i64> = [1, 2, 3, 4].into_iter().collect();
        let seen: HashSet<(i64, i64)> = HashSet::new();

        assert!(contradiction_candidate_pairs(&below, &new_ids, &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 10)
            .is_empty());
        assert!(contradiction_candidate_pairs(&above, &new_ids, &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 10)
            .is_empty());
    }

    #[test]
    fn contradiction_candidate_pairs_accepts_a_mid_band_pair() {
        // Two vectors at a 45-degree angle -- cosine ~0.707, squarely inside
        // [0.60, 0.85).
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 1.0];
        let pool = vec![(1, a), (2, b)];
        let new_ids: HashSet<i64> = [1].into_iter().collect();
        let seen: HashSet<(i64, i64)> = HashSet::new();
        let pairs =
            contradiction_candidate_pairs(&pool, &new_ids, &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 10);
        assert_eq!(pairs, vec![(1, 2)]);
    }

    #[test]
    fn contradiction_candidate_pairs_requires_at_least_one_side_new() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 1.0];
        let pool = vec![(1, a), (2, b)];
        let seen: HashSet<(i64, i64)> = HashSet::new();
        assert!(contradiction_candidate_pairs(&pool, &HashSet::new(), &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 10)
            .is_empty());
    }

    #[test]
    fn contradiction_candidate_pairs_excludes_already_seen_pairs() {
        let a = vec![1.0f32, 0.0];
        let b = vec![1.0f32, 1.0];
        let pool = vec![(1, a), (2, b)];
        let new_ids: HashSet<i64> = [1].into_iter().collect();
        let seen: HashSet<(i64, i64)> = [(1, 2)].into_iter().collect();
        assert!(
            contradiction_candidate_pairs(&pool, &new_ids, &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 10)
                .is_empty()
        );
    }

    #[test]
    fn contradiction_candidate_pairs_caps_and_orders_newest_first() {
        // Three ids all at the same mid-band angle to a common seed
        // direction -- every pairing with id 1 qualifies; newest-first means
        // the cap of 2 keeps the pairs with the largest id_b, i.e. (1,4) and
        // (1,3), not (1,2).
        let seed = vec![1.0f32, 0.0];
        let mid = vec![1.0f32, 1.0];
        let pool = vec![(1, seed), (2, mid.clone()), (3, mid.clone()), (4, mid)];
        let new_ids: HashSet<i64> = [1, 2, 3, 4].into_iter().collect();
        let seen: HashSet<(i64, i64)> = HashSet::new();
        let pairs =
            contradiction_candidate_pairs(&pool, &new_ids, &seen, CONTRADICTION_MIN_SIM, CONTRADICTION_MAX_SIM, 2);
        assert_eq!(pairs, vec![(1, 4), (1, 3)]);
    }

    // --- contradiction patrol: verdict parsing ---

    #[test]
    fn parse_contradiction_verdict_accepts_conflict() {
        assert_eq!(parse_contradiction_verdict("CONFLICT"), ContradictionVerdict::Conflict);
        assert_eq!(parse_contradiction_verdict("  conflict  \n"), ContradictionVerdict::Conflict);
    }

    #[test]
    fn parse_contradiction_verdict_accepts_conflict_retro() {
        assert_eq!(parse_contradiction_verdict("CONFLICT_RETRO"), ContradictionVerdict::ConflictRetro);
        assert_eq!(parse_contradiction_verdict("  conflict_retro  \n"), ContradictionVerdict::ConflictRetro);
    }

    #[test]
    fn parse_contradiction_verdict_accepts_both_hold_case_insensitively() {
        assert_eq!(parse_contradiction_verdict("BOTH_HOLD"), ContradictionVerdict::BothHold);
        assert_eq!(parse_contradiction_verdict("  both_hold  \n"), ContradictionVerdict::BothHold);
    }

    #[test]
    fn parse_contradiction_verdict_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nCONFLICT";
        assert_eq!(parse_contradiction_verdict(out), ContradictionVerdict::Conflict);
    }

    #[test]
    fn parse_contradiction_verdict_rejects_a_token_with_a_trailing_id() {
        // The whole point of the id-free grammar: a line like "CONFLICT 12"
        // is not an exact match for any of the four tokens, so it's simply
        // skipped like any other unrecognized line, never parsed as if it
        // named a winner.
        assert_eq!(parse_contradiction_verdict("CONFLICT 12"), ContradictionVerdict::Unclear);
        assert_eq!(parse_contradiction_verdict("CONFLICT_RETRO 12"), ContradictionVerdict::Unclear);
    }

    #[test]
    fn parse_contradiction_verdict_malformed_or_explicit_unclear_both_yield_unclear() {
        assert_eq!(parse_contradiction_verdict("UNCLEAR"), ContradictionVerdict::Unclear);
        assert_eq!(parse_contradiction_verdict(""), ContradictionVerdict::Unclear);
        assert_eq!(parse_contradiction_verdict("I'm not sure."), ContradictionVerdict::Unclear);
        assert_eq!(parse_contradiction_verdict("SUPERSEDES 12"), ContradictionVerdict::Unclear, "the old grammar is gone");
    }

    // --- contradiction patrol: newer_wins ---

    #[test]
    fn newer_wins_favors_the_later_timestamp() {
        assert_eq!(newer_wins((1, "2026-03-01T00:00:00Z"), (2, "2026-09-01T00:00:00Z")), (2, 1));
        assert_eq!(newer_wins((2, "2026-09-01T00:00:00Z"), (1, "2026-03-01T00:00:00Z")), (2, 1));
    }

    #[test]
    fn newer_wins_ties_favor_b() {
        assert_eq!(newer_wins((1, "2026-03-01T00:00:00Z"), (2, "2026-03-01T00:00:00Z")), (2, 1));
    }

    #[test]
    fn build_contradiction_pass_prompt_includes_both_memories_dates_and_instructions() {
        let p = build_contradiction_pass_prompt(
            (1, "Moses is the user's boss", "2026-03-01T00:00:00Z"),
            (2, "the user's boss Ivar approved the RHI redesign", "2026-09-01T00:00:00Z"),
        );
        assert!(p.contains("Memory #1 (recorded 2026-03-01T00:00:00Z):\nMoses is the user's boss"));
        assert!(p.contains("Memory #2 (recorded 2026-09-01T00:00:00Z):\nthe user's boss Ivar approved the RHI redesign"));
        assert!(p.contains("Note: memory #2 was recorded more recently than memory #1"));
        assert!(p.contains("CONFLICT"));
        assert!(p.contains("CONFLICT_RETRO"));
        assert!(p.contains("BOTH_HOLD"));
        assert!(p.contains("UNCLEAR"));
        assert!(!p.contains("SUPERSEDES"), "the model must never be asked to name a winner id");
    }

    // --- strength review sampler: verdict parsing ---

    #[test]
    fn parse_strength_verdict_accepts_stands_and_stale() {
        assert_eq!(parse_strength_verdict("STANDS", &[1, 2, 3]), StrengthVerdict::Stands);
        assert_eq!(parse_strength_verdict("  stale  \n", &[1, 2, 3]), StrengthVerdict::Stale);
    }

    #[test]
    fn parse_strength_verdict_accepts_conflict_naming_a_known_neighbor() {
        assert_eq!(parse_strength_verdict("CONFLICT 2", &[1, 2, 3]), StrengthVerdict::Conflict { with_id: 2 });
        assert_eq!(parse_strength_verdict("conflict #3", &[1, 2, 3]), StrengthVerdict::Conflict { with_id: 3 });
    }

    #[test]
    fn parse_strength_verdict_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nSTANDS";
        assert_eq!(parse_strength_verdict(out, &[1, 2, 3]), StrengthVerdict::Stands);
    }

    #[test]
    fn parse_strength_verdict_rejects_conflict_naming_an_unknown_neighbor() {
        assert_eq!(parse_strength_verdict("CONFLICT 99", &[1, 2, 3]), StrengthVerdict::Malformed);
    }

    #[test]
    fn parse_strength_verdict_malformed_on_empty_prose_or_missing_id() {
        assert_eq!(parse_strength_verdict("", &[1, 2, 3]), StrengthVerdict::Malformed);
        assert_eq!(parse_strength_verdict("I'm not sure.", &[1, 2, 3]), StrengthVerdict::Malformed);
        assert_eq!(parse_strength_verdict("CONFLICT", &[1, 2, 3]), StrengthVerdict::Malformed);
        assert_eq!(parse_strength_verdict("CONFLICT banana", &[1, 2, 3]), StrengthVerdict::Malformed);
    }

    #[test]
    fn build_strength_review_prompt_includes_claim_date_and_neighbors() {
        let p = build_strength_review_prompt(
            "the user's boss is Moses",
            "2026-03-01T00:00:00Z",
            &[(7, "the user's boss is now Ivar".to_string())],
        );
        assert!(p.contains("Memory (recorded 2026-03-01T00:00:00Z):\nthe user's boss is Moses"));
        assert!(p.contains("[7] the user's boss is now Ivar"));
        assert!(p.contains("STANDS"));
        assert!(p.contains("STALE"));
        assert!(p.contains("CONFLICT"));
    }

    // --- graph extraction: prompt contract + parsing ---

    #[test]
    fn build_extraction_prompt_includes_fact_and_reserved_user_note() {
        let p = build_extraction_prompt("the user works on a project called Umoja");
        assert!(p.contains("the user works on a project called Umoja"));
        assert!(p.contains("\"user\""), "must explain the reserved \"user\" entity name");
        assert!(p.contains("SRC_NAME | SRC_KIND | PREDICATE | DST_NAME | DST_KIND | CONFIDENCE"));
        assert!(p.contains("NONE"));
        assert!(p.contains("frustrated-by"), "affective/behavioral predicates must be explicitly invited");
    }

    #[test]
    fn parse_extraction_accepts_a_well_formed_triple() {
        let out = "user | | works-on | Umoja | project | 0.9";
        let triples = parse_extraction(out);
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].src_name, "user");
        assert_eq!(triples[0].src_kind, None);
        assert_eq!(triples[0].predicate, "works-on");
        assert_eq!(triples[0].dst_name, "Umoja");
        assert_eq!(triples[0].dst_kind.as_deref(), Some("project"));
        assert!((triples[0].confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn parse_extraction_normalizes_predicate_spacing_and_case() {
        let out = "Moses | person | Boss Of | user | | 0.8";
        let triples = parse_extraction(out);
        assert_eq!(triples[0].predicate, "boss-of");
    }

    #[test]
    fn parse_extraction_none_or_empty_output_yields_no_triples() {
        assert!(parse_extraction("NONE").is_empty());
        assert!(parse_extraction("  none  \n").is_empty());
        assert!(parse_extraction("").is_empty());
        assert!(parse_extraction("I don't see a relation here.").is_empty());
    }

    #[test]
    fn parse_extraction_skips_malformed_lines_but_keeps_well_formed_ones() {
        let out = "this line has no pipes at all\nuser | | prefers | dark mode | concept | 0.7";
        let triples = parse_extraction(out);
        assert_eq!(triples.len(), 1);
        assert_eq!(triples[0].dst_name, "dark mode");
    }

    #[test]
    fn parse_extraction_defaults_confidence_when_unparseable() {
        let out = "user | | prefers | tea | | not-a-number";
        let triples = parse_extraction(out);
        assert_eq!(triples[0].confidence, GRAPH_EXTRACTION_DEFAULT_CONFIDENCE);
    }

    #[test]
    fn parse_extraction_clamps_out_of_range_confidence() {
        let out = "user | | prefers | tea | | 1.5";
        let triples = parse_extraction(out);
        assert_eq!(triples[0].confidence, 1.0);
    }

    #[test]
    fn parse_extraction_caps_at_max_triples_even_if_the_model_sends_more() {
        let mut out = String::new();
        for i in 0..8 {
            out.push_str(&format!("user | | knows | entity{} | | 0.5\n", i));
        }
        let triples = parse_extraction(&out);
        assert_eq!(triples.len(), GRAPH_EXTRACTION_MAX_TRIPLES);
    }

    #[test]
    fn parse_extraction_rejects_a_triple_missing_src_dst_or_predicate() {
        assert!(parse_extraction(" | person | boss-of | user | | 0.8").is_empty(), "empty src_name");
        assert!(parse_extraction("Moses | person |  | user | | 0.8").is_empty(), "empty predicate");
        assert!(parse_extraction("Moses | person | boss-of |  | | 0.8").is_empty(), "empty dst_name");
    }

    // --- graph extraction: edge-contradiction judge ---

    #[test]
    fn parse_edge_contradiction_verdict_accepts_the_three_tokens() {
        assert_eq!(parse_edge_contradiction_verdict("CONFLICT"), EdgeContradictionVerdict::Conflict);
        assert_eq!(parse_edge_contradiction_verdict("  both_hold  \n"), EdgeContradictionVerdict::BothHold);
        assert_eq!(parse_edge_contradiction_verdict("UNCLEAR"), EdgeContradictionVerdict::Unclear);
    }

    #[test]
    fn parse_edge_contradiction_verdict_scans_past_leading_chatter() {
        let out = "Sure, here's my answer:\nCONFLICT";
        assert_eq!(parse_edge_contradiction_verdict(out), EdgeContradictionVerdict::Conflict);
    }

    #[test]
    fn parse_edge_contradiction_verdict_malformed_or_empty_is_unclear() {
        assert_eq!(parse_edge_contradiction_verdict(""), EdgeContradictionVerdict::Unclear);
        assert_eq!(parse_edge_contradiction_verdict("I'm not sure."), EdgeContradictionVerdict::Unclear);
        // Deliberately strict, same as the memory-level parser: a line
        // naming an id must never match any of the three fixed tokens.
        assert_eq!(parse_edge_contradiction_verdict("CONFLICT 12"), EdgeContradictionVerdict::Unclear);
    }

    #[test]
    fn build_edge_contradiction_prompt_is_id_free_and_names_both_edges() {
        let p = build_edge_contradiction_prompt(("Moses", "boss-of", "user"), ("Ivar", "boss-of", "user"));
        assert!(p.contains("Edge 1: Moses --boss-of--> user"));
        assert!(p.contains("Edge 2: Ivar --boss-of--> user"));
        assert!(p.contains("CONFLICT"));
        assert!(p.contains("BOTH_HOLD"));
        assert!(p.contains("UNCLEAR"));
        assert!(!p.contains("SUPERSEDES"), "the model must never be asked to name a winner id");
        assert!(!p.contains('#'), "edges are described by name only, never by id");
    }

    // --- extraction quality guard ---

    #[test]
    fn build_extraction_prompt_includes_the_quality_guard() {
        let p = build_extraction_prompt("some fact");
        assert!(p.contains("test, hypothetical"), "must warn against test/hypothetical facts");
        assert!(p.contains("memory system's own mechanics"), "must warn against self-referential facts");
        assert!(p.contains("generic-role entity"), "must warn against generic-role entities like \"boss\"");
        assert!(p.contains("\"boss\""));
        assert!(p.contains("one exception"), "the reserved \"user\" name must still be explicitly allowed");
    }

    // --- graph extraction: batched calls ---

    #[test]
    fn build_batch_extraction_prompt_numbers_every_fact_and_includes_the_quality_guard() {
        let p = build_batch_extraction_prompt(&["Moses is the user's boss", "the user works on Umoja"]);
        assert!(p.contains("1: Moses is the user's boss"));
        assert!(p.contains("2: the user works on Umoja"));
        assert!(p.contains("1 to 2"), "must tell the model exactly how many facts to address");
        assert!(p.contains("test, hypothetical"));
        assert!(p.contains("generic-role entity"));
        assert!(p.contains("N: NONE"));
    }

    #[test]
    fn parse_batch_extraction_attributes_triples_to_the_right_fact_number() {
        let out = "1: user | | works-on | Umoja | project | 0.9\n2: NONE\n3: Moses | person | boss-of | user | | 0.8";
        let parsed = parse_batch_extraction(out, 3);
        assert_eq!(parsed.len(), 3, "all three facts were addressed");
        assert_eq!(parsed[&1].len(), 1);
        assert_eq!(parsed[&1][0].dst_name, "Umoja");
        assert!(parsed[&2].is_empty(), "an explicit NONE is addressed with zero triples");
        assert_eq!(parsed[&3].len(), 1);
        assert_eq!(parsed[&3][0].src_name, "Moses");
    }

    #[test]
    fn parse_batch_extraction_a_fact_can_yield_more_than_one_triple() {
        let out = "1: user | | works-on | Umoja | project | 0.9\n1: user | | prefers | Rust | language | 0.7";
        let parsed = parse_batch_extraction(out, 1);
        assert_eq!(parsed[&1].len(), 2);
    }

    #[test]
    fn parse_batch_extraction_never_marks_an_unaddressed_fact() {
        // Fact 2 never appears anywhere in the reply -- must be absent from
        // the map entirely (never watermarked, retried next run), distinct
        // from fact 2 explicitly replying NONE.
        let out = "1: user | | works-on | Umoja | project | 0.9";
        let parsed = parse_batch_extraction(out, 2);
        assert!(parsed.contains_key(&1));
        assert!(!parsed.contains_key(&2), "an unaddressed fact must not be marked seen");
    }

    #[test]
    fn parse_batch_extraction_a_malformed_line_for_a_fact_with_no_other_line_leaves_it_unaddressed() {
        // Fact 1's only line is garbled (wrong pipe-field count) -- must not
        // count as "addressed with zero triples" the way an explicit NONE
        // does; it's indistinguishable from never having been touched.
        let out = "1: this is not the right shape at all";
        let parsed = parse_batch_extraction(out, 1);
        assert!(!parsed.contains_key(&1));
    }

    #[test]
    fn parse_batch_extraction_out_of_range_fact_numbers_are_ignored() {
        let out = "0: user | | works-on | Umoja | project | 0.9\n99: user | | prefers | tea | | 0.7";
        let parsed = parse_batch_extraction(out, 3);
        assert!(parsed.is_empty());
    }

    #[test]
    fn parse_batch_extraction_caps_triples_per_fact() {
        let mut out = String::new();
        for i in 0..8 {
            out.push_str(&format!("1: user | | knows | entity{} | | 0.5\n", i));
        }
        let parsed = parse_batch_extraction(&out, 1);
        assert_eq!(parsed[&1].len(), GRAPH_EXTRACTION_MAX_TRIPLES);
    }

    // --- graph extraction: batched edge-conflict judging ---

    #[test]
    fn build_batch_edge_contradiction_prompt_numbers_every_pair_and_is_id_free() {
        let pairs = [
            (("Moses", "boss-of", "user"), ("Ivar", "boss-of", "user")),
            (("user", "prefers", "tea"), ("user", "prefers", "coffee")),
        ];
        let p = build_batch_edge_contradiction_prompt(&pairs);
        assert!(p.contains("Pair 1:"));
        assert!(p.contains("Edge 1: Moses --boss-of--> user"));
        assert!(p.contains("Pair 2:"));
        assert!(p.contains("Edge 2: user --prefers--> coffee"));
        assert!(p.contains("1 to 2"));
        assert!(!p.contains("SUPERSEDES"));
        assert!(!p.contains('#'));
    }

    #[test]
    fn parse_batch_edge_contradiction_verdicts_attributes_per_pair() {
        let out = "1: CONFLICT\n2: BOTH_HOLD";
        let verdicts = parse_batch_edge_contradiction_verdicts(out, 2);
        assert_eq!(verdicts[&1], EdgeContradictionVerdict::Conflict);
        assert_eq!(verdicts[&2], EdgeContradictionVerdict::BothHold);
    }

    #[test]
    fn parse_batch_edge_contradiction_verdicts_missing_pair_is_absent() {
        let out = "1: CONFLICT";
        let verdicts = parse_batch_edge_contradiction_verdicts(out, 2);
        assert!(verdicts.contains_key(&1));
        assert!(!verdicts.contains_key(&2), "an unaddressed pair has no verdict -- caller treats it like UNCLEAR");
    }

    #[test]
    fn parse_batch_edge_contradiction_verdicts_scans_past_chatter_and_ignores_bad_numbers() {
        let out = "Sure, here goes:\n1: CONFLICT\n99: UNCLEAR\nbanana: BOTH_HOLD";
        let verdicts = parse_batch_edge_contradiction_verdicts(out, 1);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[&1], EdgeContradictionVerdict::Conflict);
    }

    // --- graph hygiene: batched entity-merge judging ---

    #[test]
    fn build_batch_entity_merge_prompt_numbers_every_pair_and_is_id_free() {
        let samples_a = vec!["Moses --proposes-feature-for--> Umoja".to_string()];
        let samples_b: Vec<String> = vec![];
        let pairs = [(("Umoja", "project", samples_a.as_slice()), ("umoja project", "project", samples_b.as_slice()))];
        let p = build_batch_entity_merge_prompt(&pairs);
        assert!(p.contains("Pair 1:"));
        assert!(p.contains("Entity A: \"Umoja\" (kind: project)"));
        assert!(p.contains("Entity B: \"umoja project\" (kind: project)"));
        assert!(p.contains("Moses --proposes-feature-for--> Umoja"));
        assert!(p.contains("1 to 1"));
        assert!(p.contains("SAME"));
        assert!(p.contains("DIFFERENT"));
    }

    #[test]
    fn parse_batch_entity_merge_verdicts_attributes_per_pair() {
        let out = "1: SAME\n2: DIFFERENT";
        let verdicts = parse_batch_entity_merge_verdicts(out, 2);
        assert_eq!(verdicts[&1], EntityMergeVerdict::Same);
        assert_eq!(verdicts[&2], EntityMergeVerdict::Different);
    }

    #[test]
    fn parse_batch_entity_merge_verdicts_missing_pair_is_absent() {
        let out = "1: SAME";
        let verdicts = parse_batch_entity_merge_verdicts(out, 2);
        assert!(verdicts.contains_key(&1));
        assert!(!verdicts.contains_key(&2), "an unaddressed pair has no verdict -- left exactly as it is");
    }

    #[test]
    fn parse_batch_entity_merge_verdicts_scans_past_chatter_and_ignores_bad_numbers() {
        let out = "Sure, here goes:\n1: SAME\n99: DIFFERENT\nbanana: SAME";
        let verdicts = parse_batch_entity_merge_verdicts(out, 1);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[&1], EntityMergeVerdict::Same);
    }

    // --- one-time (or repeatable) edge audit: mach kb graph audit ---

    #[test]
    fn build_batch_graph_audit_prompt_numbers_every_edge_and_is_id_free() {
        let edges = [
            ("Moses", "person", "proposes-feature-for", "Umoja", "project", "Moses proposed a feature for Umoja"),
            ("user", "unspecified", "has-boss", "boss", "unspecified", "the user's chain of command"),
        ];
        let p = build_batch_graph_audit_prompt(&edges);
        assert!(p.contains("Edge 1: Moses (person) --proposes-feature-for--> Umoja (project)"));
        assert!(p.contains("Edge 2: user (unspecified) --has-boss--> boss (unspecified)"));
        assert!(p.contains("1 to 2"));
        assert!(p.contains("KEEP"));
        assert!(p.contains("POISONED"));
        assert!(p.contains("GENERIC"));
        assert!(!p.contains('#'));
    }

    #[test]
    fn parse_batch_graph_audit_verdicts_attributes_per_edge() {
        let out = "1: KEEP\n2: POISONED\n3: GENERIC";
        let verdicts = parse_batch_graph_audit_verdicts(out, 3);
        assert_eq!(verdicts[&1], GraphAuditVerdict::Keep);
        assert_eq!(verdicts[&2], GraphAuditVerdict::Poisoned);
        assert_eq!(verdicts[&3], GraphAuditVerdict::Generic);
    }

    #[test]
    fn parse_batch_graph_audit_verdicts_missing_edge_is_absent() {
        let out = "1: KEEP";
        let verdicts = parse_batch_graph_audit_verdicts(out, 2);
        assert!(verdicts.contains_key(&1));
        assert!(!verdicts.contains_key(&2), "an unaddressed edge has no verdict -- caller treats it like KEEP");
    }

    #[test]
    fn parse_batch_graph_audit_verdicts_scans_past_chatter_and_ignores_bad_numbers() {
        let out = "Sure, here goes:\n1: POISONED\n99: GENERIC\nbanana: KEEP";
        let verdicts = parse_batch_graph_audit_verdicts(out, 1);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[&1], GraphAuditVerdict::Poisoned);
    }
}
