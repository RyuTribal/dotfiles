
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
}
