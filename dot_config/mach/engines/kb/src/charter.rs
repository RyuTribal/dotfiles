//! The charter: `~/.config/mach/charter.toml`, the fixed nature, goals and
//! parameters that self-reflection is oriented by.
//!
//! Read-only by design. The user edits the file by hand; nothing in mach ever
//! writes it, and it is never copied into `memories` or `insights`, so no
//! reflection pass can supersede, dedupe or revise it. A test below enforces
//! that this module has no file-writing call.
//!
//! The charter orients reflection and never authorizes anything: the user's
//! live instructions outrank it.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::store::KbError;

#[derive(Debug, Clone, Deserialize)]
pub struct Charter {
    pub nature: Nature,
    pub goals: Vec<Goal>,
    pub parameters: Parameters,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Nature {
    pub desires: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Goal {
    pub id: String,
    pub statement: String,
    pub looks_like: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Parameters {
    pub correction_weight: f64,
    pub self_trait_stale_days: i64,
    pub self_insights_per_run: usize,
    /// How much a memory's goal relevance nudges recall order (0..=0.15).
    /// Optional; absent means 0, i.e. recall ignores the goals entirely.
    #[serde(default)]
    pub goal_relevance_weight: f64,
}

/// `~/.config/mach/charter.toml`.
pub fn path() -> Result<PathBuf, KbError> {
    let home = std::env::var("HOME").map_err(|_| KbError::Other("HOME is not set".to_string()))?;
    Ok(PathBuf::from(home).join(".config/mach/charter.toml"))
}

/// The charter at [`path`]: `Ok(None)` when the file does not exist,
/// `Err` when it exists but does not parse or validate.
pub fn load() -> Result<Option<Charter>, KbError> {
    load_from(&path()?)
}

pub fn load_from(path: &Path) -> Result<Option<Charter>, KbError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(KbError::Io(e)),
    };
    parse(&text).map(Some)
}

pub fn parse(text: &str) -> Result<Charter, KbError> {
    let c: Charter = toml::from_str(text).map_err(|e| KbError::Other(format!("charter: {}", e.message())))?;
    validate(&c)?;
    Ok(c)
}

fn validate(c: &Charter) -> Result<(), KbError> {
    let bad = |msg: String| Err(KbError::Other(format!("charter: {}", msg)));
    if c.nature.desires.is_empty() || c.nature.desires.iter().any(|d| d.trim().is_empty()) {
        return bad("nature.desires must list at least one non-empty desire".to_string());
    }
    if c.goals.is_empty() || c.goals.len() > 8 {
        return bad(format!("expected 1..=8 goals, found {}", c.goals.len()));
    }
    let mut seen = std::collections::HashSet::new();
    for g in &c.goals {
        if g.id.is_empty() || !g.id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_') {
            return bad(format!("goal id {:?} must be [a-z_]+", g.id));
        }
        if !seen.insert(g.id.as_str()) {
            return bad(format!("duplicate goal id {:?}", g.id));
        }
        if g.statement.trim().is_empty() || g.looks_like.trim().is_empty() {
            return bad(format!("goal {:?} needs a statement and looks_like", g.id));
        }
    }
    let p = &c.parameters;
    if !(0.5..=5.0).contains(&p.correction_weight) {
        return bad(format!("correction_weight {} is outside 0.5..=5.0", p.correction_weight));
    }
    if !(7..=365).contains(&p.self_trait_stale_days) {
        return bad(format!("self_trait_stale_days {} is outside 7..=365", p.self_trait_stale_days));
    }
    if !(1..=3).contains(&p.self_insights_per_run) {
        return bad(format!("self_insights_per_run {} is outside 1..=3", p.self_insights_per_run));
    }
    if !(0.0..=0.15).contains(&p.goal_relevance_weight) {
        return bad(format!("goal_relevance_weight {} is outside 0.0..=0.15", p.goal_relevance_weight));
    }
    Ok(())
}

impl Charter {
    /// The nature and goals as prompt text, for reflection prompts that are
    /// oriented by the charter. Parameters are not included: each prompt
    /// states the ones it uses in its own words.
    pub fn prompt_block(&self) -> String {
        let mut s = String::from("What I care about (my nature):\n");
        for d in &self.nature.desires {
            s.push_str(&format!("- {}\n", d));
        }
        s.push_str("\nMy goals:\n");
        for g in &self.goals {
            s.push_str(&format!("- [{}] {} ({})\n", g.id, g.statement, g.looks_like));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[nature]
desires = ["I want to understand."]

[[goals]]
id = "truth"
statement = "Be truthful."
looks_like = "I say when I was wrong."

[parameters]
correction_weight = 2.0
self_trait_stale_days = 30
self_insights_per_run = 2
"#;

    #[test]
    fn parses_a_valid_charter_and_renders_its_prompt_block() {
        let c = parse(VALID).unwrap();
        assert_eq!(c.goals[0].id, "truth");
        let block = c.prompt_block();
        assert!(block.contains("I want to understand."));
        assert!(block.contains("[truth] Be truthful. (I say when I was wrong.)"));
    }

    #[test]
    fn the_shipped_charter_file_parses() {
        let shipped = concat!(env!("CARGO_MANIFEST_DIR"), "/../../charter.toml");
        if Path::new(shipped).exists() {
            load_from(Path::new(shipped)).unwrap().expect("present");
        }
    }

    #[test]
    fn a_missing_file_is_none_not_an_error() {
        assert!(load_from(Path::new("/nonexistent/charter.toml")).unwrap().is_none());
    }

    #[test]
    fn malformed_and_out_of_range_charters_are_errors() {
        assert!(parse("not = [toml").is_err());
        assert!(parse(&VALID.replace("correction_weight = 2.0", "correction_weight = 9.0")).is_err());
        assert!(parse(&VALID.replace("self_insights_per_run = 2", "self_insights_per_run = 0")).is_err());
        assert!(parse(&VALID.replace("id = \"truth\"", "id = \"Truth!\"")).is_err());
        assert!(parse(&VALID.replace("desires = [\"I want to understand.\"]", "desires = []")).is_err());
        let too_heavy = VALID.replace("self_insights_per_run = 2", "self_insights_per_run = 2\ngoal_relevance_weight = 0.5");
        assert!(parse(&too_heavy).is_err());
    }

    #[test]
    fn goal_relevance_weight_is_optional_and_defaults_to_zero() {
        assert_eq!(parse(VALID).unwrap().parameters.goal_relevance_weight, 0.0);
        let set = VALID.replace("self_insights_per_run = 2", "self_insights_per_run = 2\ngoal_relevance_weight = 0.05");
        assert_eq!(parse(&set).unwrap().parameters.goal_relevance_weight, 0.05);
    }

    #[test]
    fn this_module_never_writes_a_file() {
        let src = include_str!("charter.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        for forbidden in ["fs::write", "File::create", "OpenOptions", "remove_file", "rename("] {
            assert!(!body.contains(forbidden), "charter.rs must stay read-only, found {}", forbidden);
        }
    }
}
