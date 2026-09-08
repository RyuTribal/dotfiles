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
        Question { id: "q".into(), category: "c".into(), query: "why".into(), expect, forbid }
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
}
