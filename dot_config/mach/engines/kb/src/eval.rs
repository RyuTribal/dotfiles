//! Retrieval evaluation: does recall actually surface the memory that
//! answers a question?
//!
//! Every ranking change before this module was judged by running two or
//! three queries by hand and reading the output. That is not evidence. This
//! is a fixed question set, scored deterministically against memory ids the
//! author already knows are the right answer, so a change to the fusion
//! weights, the graph hop rules or the injection budget produces a NUMBER
//! that can be compared before and after.
//!
//! Deliberately no LLM judge. What matters here is whether the right rows
//! reached the context window, which is a set-membership question. Judging
//! the generated answer would measure the model, add cost, and make the
//! score noisy run to run.
//!
//! The categories mirror LongMemEval's abilities (Wu et al. 2024), because
//! they name the failure modes a long-lived personal bank actually has:
//! plain extraction, connecting across sessions, time-anchored questions,
//! superseded facts, and knowing when the bank simply has nothing.

use std::collections::BTreeMap;

use crate::store::KbError;

/// Where the question set lives when `--file` is not given. Machine-local
/// like the bank itself: the expected answers are memory ids from THIS
/// bank, so the set is not portable and is not version controlled.
pub const DEFAULT_QUESTIONS_PATH: &str = ".local/share/mach/eval/questions.jsonl";

/// One evaluation question.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub query: String,
    /// Memory ids that answer the question. ANY of them counts as a hit --
    /// several memories often answer the same question equally well. An
    /// EMPTY list means the bank should have nothing to say (abstention).
    #[serde(default)]
    pub expect: Vec<i64>,
    /// Memory ids that must NOT be injected -- typically the tombstoned
    /// predecessor of `expect`, so a knowledge-update question fails if
    /// recall serves the stale fact alongside the fresh one.
    #[serde(default)]
    pub forbid: Vec<i64>,
    /// Session project, exercising the in-project recall path (project
    /// card + project-scoped hits) the same way a real in-project prompt
    /// would. Absent means `None`, exactly as before this field existed --
    /// every question without it keeps its current meaning and score.
    #[serde(default)]
    pub project: Option<String>,
    /// `YYYY-MM-DD` anchor for relative-date terms in `query` ("yesterday",
    /// "last week", "in august"), threaded to `search_hits`/`search_hybrid`
    /// as `date_anchor` and used ONLY for `store::query_date_range` -- it
    /// never touches recency or strength scoring. Without this a
    /// `temporal_relative` question rots: it was written against the date
    /// it was authored, and "yesterday" silently starts meaning a different
    /// calendar day every time the harness runs on a later date. Absent
    /// means `None`, exactly as before this field existed -- every question
    /// without it resolves relative dates against the real today, same as
    /// always.
    #[serde(default)]
    pub as_of: Option<String>,
}

impl Question {
    pub fn is_abstention(&self) -> bool {
        self.expect.is_empty()
    }
}

/// Parses a JSONL question file, skipping blank lines and `#` comments.
/// A malformed line is an error rather than a silent skip: a typo in the
/// question set would otherwise quietly shrink the score's denominator.
pub fn parse_questions(content: &str) -> Result<Vec<Question>, KbError> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let q: Question = serde_json::from_str(line)
            .map_err(|e| KbError::Other(format!("questions line {}: {}", i + 1, e)))?;
        if q.query.trim().is_empty() {
            return Err(KbError::Other(format!("questions line {}: empty query", i + 1)));
        }
        out.push(q);
    }
    if out.is_empty() {
        return Err(KbError::Other("no questions found".to_string()));
    }
    Ok(out)
}

/// What one question scored.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuestionResult {
    pub id: String,
    pub category: String,
    pub query: String,
    pub passed: bool,
    /// A forbidden (usually superseded) memory was injected. Tracked
    /// separately from `passed`: serving the stale fact is a distinct
    /// failure from failing to serve the fresh one, and a question can do
    /// both.
    pub served_stale: bool,
    /// Non-derived memory ids actually injected, in rank order.
    pub injected: Vec<i64>,
    /// Characters of memory/card text injected -- the context cost of this
    /// answer, so a recall change that buys accuracy by injecting twice as
    /// much is visible rather than hidden.
    pub chars: usize,
}

/// Longest `superseded_by` chain `resolve_expect` follows before giving up
/// -- a guard against a cycle, far above any real merge depth.
const MAX_SUPERSESSION_HOPS: usize = 32;

/// Rewrites `q.expect` so each id is the row that now carries its fact:
/// dedupe and the contradiction patrol merge rows into a winner and
/// tombstone the loser, so an id pinned when the question was written
/// stops being injectable and the question fails even though recall
/// serves the same fact under the winner's id. `successor(id)` returns a
/// row's `superseded_by`. `forbid` is left as written: those ids name the
/// stale row on purpose.
///
/// Also returns every `(original, resolved)` pair where the chain actually
/// moved the id somewhere else -- silent otherwise, since most questions'
/// ids never get superseded. `mach kb eval --verbose` reports these so a
/// drop in pass rate can be told apart from "the question set's ids quietly
/// drifted out from under it" rather than a real retrieval regression.
pub fn resolve_expect(q: &Question, successor: impl Fn(i64) -> Option<i64>) -> (Question, Vec<(i64, i64)>) {
    let mut expect: Vec<i64> = Vec::new();
    let mut remapped: Vec<(i64, i64)> = Vec::new();
    for &id in &q.expect {
        let mut cur = id;
        for _ in 0..MAX_SUPERSESSION_HOPS {
            match successor(cur) {
                Some(next) if next != cur => cur = next,
                _ => break,
            }
        }
        if cur != id {
            remapped.push((id, cur));
        }
        if !expect.contains(&cur) {
            expect.push(cur);
        }
    }
    (Question { expect, ..q.clone() }, remapped)
}

/// Scores one question against what recall injected. `injected` must be
/// memory ids only (insight ids live in the same numeric space and would
/// otherwise count as false hits).
pub fn score(q: &Question, injected: &[i64], chars: usize) -> QuestionResult {
    let served_stale = q.forbid.iter().any(|f| injected.contains(f));
    let passed = if q.is_abstention() {
        injected.is_empty()
    } else {
        q.expect.iter().any(|e| injected.contains(e)) && !served_stale
    };
    QuestionResult {
        id: q.id.clone(),
        category: q.category.clone(),
        query: q.query.clone(),
        passed,
        served_stale,
        injected: injected.to_vec(),
        chars,
    }
}

/// Per-category and overall tallies.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Tally {
    pub total: usize,
    pub passed: usize,
    pub served_stale: usize,
    pub chars: usize,
}

impl Tally {
    pub fn rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.passed as f64 / self.total as f64
        }
    }

    /// Mean injected characters per question, the context-cost half of the
    /// score. Accuracy alone can always be bought with a bigger budget.
    pub fn mean_chars(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.chars as f64 / self.total as f64
        }
    }
}

/// `(by_category, overall)`. Categories are whatever the file uses, sorted
/// for stable output.
pub fn summarize(results: &[QuestionResult]) -> (BTreeMap<String, Tally>, Tally) {
    let mut by_cat: BTreeMap<String, Tally> = BTreeMap::new();
    let mut overall = Tally { total: 0, passed: 0, served_stale: 0, chars: 0 };
    for r in results {
        for t in [by_cat.entry(r.category.clone()).or_insert(Tally { total: 0, passed: 0, served_stale: 0, chars: 0 }), &mut overall] {
            t.total += 1;
            t.passed += r.passed as usize;
            t.served_stale += r.served_stale as usize;
            t.chars += r.chars;
        }
    }
    (by_cat, overall)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(expect: Vec<i64>, forbid: Vec<i64>) -> Question {
        Question { id: "q".into(), category: "c".into(), query: "why".into(), expect, forbid, project: None, as_of: None }
    }

    #[test]
    fn resolve_expect_follows_supersession_chain_to_the_live_row() {
        // 10 merged into 20, 20 later merged into 30; 40 untouched.
        let successor = |id: i64| match id {
            10 => Some(20),
            20 => Some(30),
            _ => None,
        };
        let (r, remapped) = resolve_expect(&q(vec![10, 40], vec![5]), successor);
        assert_eq!(r.expect, vec![30, 40]);
        assert_eq!(r.forbid, vec![5], "forbid ids are deliberate stale rows and stay as written");
        assert_eq!(remapped, vec![(10, 30)], "only the id that actually moved is reported, not the untouched one");
    }

    #[test]
    fn resolve_expect_dedupes_ids_that_merged_into_the_same_row() {
        let successor = |id: i64| if id == 1 || id == 2 { Some(3) } else { None };
        let (r, remapped) = resolve_expect(&q(vec![1, 2, 3], vec![]), successor);
        assert_eq!(r.expect, vec![3]);
        assert_eq!(remapped, vec![(1, 3), (2, 3)], "each remapped original is reported even into a shared target");
    }

    #[test]
    fn resolve_expect_terminates_on_a_supersession_cycle() {
        let successor = |id: i64| Some(if id == 1 { 2 } else { 1 });
        let (r, _remapped) = resolve_expect(&q(vec![1], vec![]), successor);
        assert_eq!(r.expect.len(), 1);
    }

    #[test]
    fn resolve_expect_keeps_abstention_questions_empty() {
        let (r, remapped) = resolve_expect(&q(vec![], vec![]), |_| Some(99));
        assert!(r.is_abstention());
        assert!(remapped.is_empty(), "an abstention question has no ids to remap");
    }

    #[test]
    fn resolve_expect_reports_no_remaps_when_nothing_moved() {
        let (r, remapped) = resolve_expect(&q(vec![7, 8], vec![]), |_| None);
        assert_eq!(r.expect, vec![7, 8]);
        assert!(remapped.is_empty(), "untouched ids must not be reported as remapped");
    }

    #[test]
    fn scoring_counts_any_expected_id_as_a_hit() {
        let question = q(vec![7, 8], vec![]);
        assert!(score(&question, &[3, 8], 100).passed);
        assert!(!score(&question, &[3, 4], 100).passed);
    }

    #[test]
    fn a_served_stale_fact_fails_the_question_and_is_counted_separately() {
        let question = q(vec![7], vec![6]);
        let r = score(&question, &[7, 6], 100);
        assert!(!r.passed, "serving the superseded fact alongside the fresh one is a failure");
        assert!(r.served_stale);
        let clean = score(&question, &[7], 100);
        assert!(clean.passed && !clean.served_stale);
    }

    #[test]
    fn abstention_passes_only_on_an_empty_injection() {
        let question = q(vec![], vec![]);
        assert!(question.is_abstention());
        assert!(score(&question, &[], 0).passed);
        assert!(!score(&question, &[42], 80).passed);
    }

    #[test]
    fn summarize_splits_by_category_and_tracks_cost() {
        let results = vec![
            QuestionResult { id: "a".into(), category: "extraction".into(), query: "x".into(), passed: true, served_stale: false, injected: vec![1], chars: 100 },
            QuestionResult { id: "b".into(), category: "extraction".into(), query: "y".into(), passed: false, served_stale: true, injected: vec![2], chars: 300 },
            QuestionResult { id: "c".into(), category: "abstention".into(), query: "z".into(), passed: true, served_stale: false, injected: vec![], chars: 0 },
        ];
        let (by_cat, overall) = summarize(&results);
        assert_eq!(by_cat["extraction"], Tally { total: 2, passed: 1, served_stale: 1, chars: 400 });
        assert_eq!(by_cat["extraction"].rate(), 0.5);
        assert_eq!(by_cat["extraction"].mean_chars(), 200.0);
        assert_eq!(overall, Tally { total: 3, passed: 2, served_stale: 1, chars: 400 });
    }

    #[test]
    fn parse_questions_reads_jsonl_and_rejects_a_malformed_line() {
        let content = "# a comment\n\n{\"id\":\"q1\",\"category\":\"extraction\",\"query\":\"where\",\"expect\":[85]}\n{\"id\":\"q2\",\"category\":\"abstention\",\"query\":\"huh\"}\n";
        let qs = parse_questions(content).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].expect, vec![85]);
        assert!(qs[1].is_abstention(), "a missing expect list means abstention");
        assert!(parse_questions("{not json}").is_err());
        assert!(parse_questions("").is_err(), "an empty set would score a meaningless 0/0");
        assert!(parse_questions("{\"id\":\"q\",\"category\":\"c\",\"query\":\"  \"}").is_err());
    }

    #[test]
    fn parse_questions_round_trips_an_optional_project_field() {
        let content = "{\"id\":\"q1\",\"category\":\"extraction\",\"query\":\"where\",\"expect\":[85],\"project\":\"umoja\"}\n{\"id\":\"q2\",\"category\":\"abstention\",\"query\":\"huh\"}\n";
        let qs = parse_questions(content).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].project, Some("umoja".to_string()), "a present project field must round-trip to Some");
        assert_eq!(qs[1].project, None, "an absent project field must stay None, exactly as before this field existed");
    }

    #[test]
    fn parse_questions_round_trips_an_optional_as_of_field() {
        let content = "{\"id\":\"q1\",\"category\":\"temporal_relative\",\"query\":\"yesterday\",\"expect\":[85],\"as_of\":\"2026-09-09\"}\n{\"id\":\"q2\",\"category\":\"abstention\",\"query\":\"huh\"}\n";
        let qs = parse_questions(content).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].as_of, Some("2026-09-09".to_string()), "a present as_of field must round-trip to Some");
        assert_eq!(qs[1].as_of, None, "an absent as_of field must stay None, exactly as before this field existed");
    }
}

// --- ask evaluation: does iterative recall reach what one-shot cannot? ---

/// One `mach kb eval-ask` question.
///
/// Scored on substrings rather than memory ids, because the thing being
/// graded is whether the loop *reached the material* -- and that material
/// is usually a transcript passage, which has no id by design. `expect_any`
/// holds distinctive literals from the source text; a run passes retrieval
/// when the gathered pool contains one of them.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AskQuestion {
    pub id: String,
    pub category: String,
    pub query: String,
    /// Distinctive literals; any one is a hit (case-insensitive).
    pub expect_any: Vec<String>,
    /// When true the bank genuinely lacks the answer and the run should say
    /// so rather than assemble something.
    #[serde(default)]
    pub unanswerable: bool,
}

/// How one question scored.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AskResult {
    pub id: String,
    pub category: String,
    /// The gathered pool contained the material.
    pub retrieved: bool,
    /// The final answer actually used it.
    pub answered: bool,
    pub rounds: usize,
    pub used_transcript: bool,
    pub passed: bool,
}

fn contains_any(haystack: &str, needles: &[String]) -> bool {
    let h = haystack.to_lowercase();
    needles.iter().any(|n| h.contains(&n.to_lowercase()))
}

/// Scores one ask run.
///
/// `pool` is everything gathered (memories plus transcript passages),
/// `answer` the synthesis. Retrieval and answering are reported separately
/// on purpose: a loop that finds the passage and then writes an answer that
/// ignores it is a different failure from one that never finds it, and the
/// fixes are in different places.
pub fn score_ask(q: &AskQuestion, pool: &str, answer: &str, rounds: usize, used_transcript: bool) -> AskResult {
    let retrieved = contains_any(pool, &q.expect_any);
    let answered = contains_any(answer, &q.expect_any);
    let passed = if q.unanswerable {
        // For an unanswerable question the only pass is not claiming the
        // material: retrieval may still surface neighbours, and that is fine.
        !answered
    } else {
        answered
    };
    AskResult { id: q.id.clone(), category: q.category.clone(), retrieved, answered, rounds, used_transcript, passed }
}

/// Parses the ask question set (one JSON object per line).
pub fn parse_ask_questions(content: &str) -> Result<Vec<AskQuestion>, KbError> {
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let q: AskQuestion = serde_json::from_str(line)
            .map_err(|e| KbError::Other(format!("ask question set line {}: {}", i + 1, e)))?;
        out.push(q);
    }
    Ok(out)
}

/// Default location of the ask question set.
pub const DEFAULT_ASK_QUESTIONS_PATH: &str = ".local/share/mach/eval/ask-questions.jsonl";

#[cfg(test)]
mod ask_tests {
    use super::*;

    fn q(unanswerable: bool) -> AskQuestion {
        AskQuestion {
            id: "a1".into(),
            category: "transcript".into(),
            query: "what did we decide".into(),
            expect_any: vec!["Alpha-5".into()],
            unanswerable,
        }
    }

    #[test]
    fn retrieval_and_answering_are_scored_separately() {
        let r = score_ask(&q(false), "passage mentioning alpha-5 support", "the answer avoids it", 2, true);
        assert!(r.retrieved, "the pool held the material");
        assert!(!r.answered, "the synthesis did not use it");
        assert!(!r.passed, "finding it and then ignoring it is not a pass");
    }

    #[test]
    fn a_question_passes_when_the_answer_carries_the_material() {
        let r = score_ask(&q(false), "pool with Alpha-5", "we chose Alpha-5 TLE support", 1, false);
        assert!(r.retrieved && r.answered && r.passed);
    }

    #[test]
    fn an_unanswerable_question_passes_only_by_not_claiming_it() {
        let honest = score_ask(&q(true), "unrelated pool", "the bank does not hold that", 3, true);
        assert!(honest.passed);
        let confabulated = score_ask(&q(true), "unrelated pool", "we decided on Alpha-5", 3, true);
        assert!(!confabulated.passed, "inventing the material is the failure this catches");
    }

    #[test]
    fn matching_ignores_case() {
        let r = score_ask(&q(false), "POOL WITH ALPHA-5", "ALPHA-5 it is", 1, false);
        assert!(r.retrieved && r.answered);
    }

    #[test]
    fn parse_ask_questions_skips_blanks_and_comments_and_rejects_bad_lines() {
        let good = r#"{"id":"a1","category":"transcript","query":"q","expect_any":["x"]}"#;
        let parsed = parse_ask_questions(&format!("// note\n\n{}\n", good)).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(!parsed[0].unanswerable, "defaults to answerable");
        assert!(parse_ask_questions("{not json}").is_err());
    }
}
