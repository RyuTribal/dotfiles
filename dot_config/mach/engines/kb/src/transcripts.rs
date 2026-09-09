//! Raw-transcript index: full-text search over what was actually said,
//! alongside the distilled facts in `memories`.
//!
//! The bank stores conclusions. A digest keeps a handful of durable facts
//! per session and throws the conversation away, which is the right trade
//! for recall — but it means a question about something said once, in
//! passing, has nothing to match. That gap is concrete: a meeting question
//! about NORAD ids versus satellite names needed a transcript grep by hand,
//! because no memory held it and none should have.
//!
//! So this indexes the conversations themselves and lets `mach kb ask`
//! reach them when the distilled layer comes back thin. Honcho's Dialectic
//! agent has the same two-tier shape: `search_memory` for conclusions,
//! `search_messages` for the raw material behind them.
//!
//! Scale forced two design choices. The corpus here is 962MB across 3274
//! JSONL files, one of them 194MB, so extraction runs in Rust streaming
//! line by line rather than shelling out to `kb-transcript-filter.py` once
//! per file (3274 python spawns), and files are skipped by mtime+size so a
//! re-index only pays for what changed.

/// One extracted turn: who spoke and what they said, with the transcript's
/// own timestamp when it carries one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: String,
    pub text: String,
    pub ts: Option<String>,
}

/// A block of consecutive turns, sized for retrieval rather than display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// Timestamp of the first turn in the block, when known.
    pub ts: Option<String>,
}

/// Target size of one chunk. Big enough that a question and its answer
/// usually land together (the pair is what makes a hit useful), small
/// enough that a hit points at a passage rather than an afternoon.
pub const CHUNK_CHARS: usize = 1500;

/// A single turn longer than this is truncated rather than indexed whole:
/// pasted files and command dumps are the long tail here, they are not
/// conversation, and one of them can outweigh a whole session.
pub const MAX_TURN_CHARS: usize = 4000;

/// Pulls the user/assistant text out of one Claude Code transcript line.
///
/// Returns `None` for everything else — tool calls and their results,
/// meta/system entries, summaries, and any line that does not parse. That
/// filtering is the point: tool output is most of the bytes and none of the
/// conversation, and indexing it would bury real dialogue under file
/// contents and command spew.
pub fn extract_turn(line: &str) -> Option<Turn> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v.get("isMeta").and_then(|m| m.as_bool()).unwrap_or(false) {
        return None;
    }
    let role = v.get("message")?.get("role")?.as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    let ts = v.get("timestamp").and_then(|t| t.as_str()).map(|s| s.to_string());
    let content = v.get("message")?.get("content")?;

    let mut text = String::new();
    match content {
        serde_json::Value::String(s) => text.push_str(s),
        serde_json::Value::Array(parts) => {
            for p in parts {
                // Only the prose blocks. `tool_use` and `tool_result` are
                // the machine's side of the conversation.
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
            }
        }
        _ => return None,
    }

    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Hook output and command scaffolding are injected into the user turn
    // and are not something the user said.
    if text.starts_with("<system-reminder>")
        || text.starts_with("<local-command")
        || text.starts_with("<command-name>")
        || text.starts_with("Caveat: The messages below")
    {
        return None;
    }
    let text = if text.chars().count() > MAX_TURN_CHARS {
        text.chars().take(MAX_TURN_CHARS).collect::<String>()
    } else {
        text.to_string()
    };
    Some(Turn { role: role.to_string(), text, ts })
}

/// Groups consecutive turns into `CHUNK_CHARS`-ish blocks, each rendered as
/// `role: text` lines.
///
/// A chunk boundary never splits a turn: a question cut from its answer
/// retrieves badly, and the whole reason to index raw dialogue is that the
/// exchange carries the meaning.
pub fn chunk_turns(turns: &[Turn]) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut ts: Option<String> = None;
    for t in turns {
        if !buf.is_empty() && buf.chars().count() + t.text.chars().count() > CHUNK_CHARS {
            out.push(Chunk { text: std::mem::take(&mut buf), ts: ts.take() });
        }
        if buf.is_empty() {
            ts = t.ts.clone();
        } else {
            buf.push('\n');
        }
        buf.push_str(&t.role);
        buf.push_str(": ");
        buf.push_str(&t.text);
    }
    if !buf.is_empty() {
        out.push(Chunk { text: buf, ts });
    }
    out
}

/// Openers that mark a transcript as mach talking to itself rather than a
/// conversation with the user.
///
/// mach's own chores run as `claude -p`, so each card, digest, judge and
/// `ask` call leaves a session transcript in `~/.claude/projects` exactly
/// like a real session. Indexing those was worth 855 of the first index's
/// 9536 chunks (9%), and they are the worst possible rows to keep: they are
/// phrased in the vocabulary of the knowledge bank itself, so they match
/// precisely the questions someone asks *about* the bank. The engagement
/// prompt is worse again -- it quotes a whole transcript inside itself, so
/// the same dialogue lands twice, once wrapped in scaffolding.
///
/// Matched against the first user turn, because a chore session is
/// machinery from its first line to its last.
const MACHINE_OPENERS: [&str; 5] = [
    // ingest::DIGEST_INSTRUCTIONS
    "You are extracting durable, cross-session-worthy facts about the USER",
    // ingest::build_engagement_prompt
    "You are reviewing a finished Claude Code session transcript",
    // reflect::build_entity_card_prompt
    "You are writing the CARD for one entity in a personal knowledge bank",
    // ask::build_probe_prompt
    "You are deciding whether a knowledge bank search has found the answer",
    // ask::build_answer_prompt
    "Answer a question using only the knowledge-bank entries below",
];

/// The reflect judges (dedupe, contradiction, curation, entity merge, graph
/// audit, insight and theme passes) do not share one opener -- they run
/// "You are checking / analyzing / auditing / curating ..." -- but every one
/// of them names the thing it is working on in its first line. Matching that
/// phrase covers the whole family, including judges not yet written.
const MACHINE_SUBJECTS: [&str; 2] = ["personal knowledge bank", "personal knowledge graph"];

/// How far into the first turn to look for a `MACHINE_SUBJECTS` phrase.
/// Long enough for any of the real openers, short enough that a user who
/// merely mentions their knowledge bank mid-message is not filtered out.
const MACHINE_SUBJECT_WINDOW: usize = 120;

/// Whether this transcript is one of mach's own subprocess calls.
pub fn is_machine_session(turns: &[Turn]) -> bool {
    let Some(first) = turns.iter().find(|t| t.role == "user") else {
        return false;
    };
    let text = first.text.trim_start();
    if MACHINE_OPENERS.iter().any(|o| text.starts_with(o)) {
        return true;
    }
    let head: String = text.chars().take(MACHINE_SUBJECT_WINDOW).collect();
    text.starts_with("You are ") && MACHINE_SUBJECTS.iter().any(|m| head.contains(m))
}

/// The project a transcript belongs to, decoded from Claude Code's
/// directory naming (`-home-ryutribal-programming-umoja` -> `umoja`).
pub fn project_from_dir(dir: &str) -> String {
    dir.rsplit('-').next().filter(|s| !s.is_empty()).unwrap_or(dir).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_turn_reads_a_plain_user_line() {
        let l = r#"{"type":"user","timestamp":"2026-09-08T10:00:00Z","message":{"role":"user","content":"what does umoja do"}}"#;
        let t = extract_turn(l).unwrap();
        assert_eq!(t.role, "user");
        assert_eq!(t.text, "what does umoja do");
        assert_eq!(t.ts.as_deref(), Some("2026-09-08T10:00:00Z"));
    }

    #[test]
    fn extract_turn_keeps_only_the_text_blocks_of_a_content_array() {
        let l = r#"{"message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"hidden"},
            {"type":"text","text":"first"},
            {"type":"tool_use","name":"Bash","input":{"command":"ls"}},
            {"type":"text","text":"second"}]}}"#;
        let t = extract_turn(l).unwrap();
        assert_eq!(t.text, "first\nsecond", "thinking and tool_use are not conversation");
    }

    #[test]
    fn extract_turn_skips_everything_that_is_not_dialogue() {
        for l in [
            r#"{"message":{"role":"system","content":"boot"}}"#,
            r#"{"isMeta":true,"message":{"role":"user","content":"meta"}}"#,
            r#"{"message":{"role":"user","content":[{"type":"tool_result","content":"rc=0"}]}}"#,
            r#"{"message":{"role":"user","content":"<system-reminder>hook output</system-reminder>"}}"#,
            r#"{"message":{"role":"user","content":"   "}}"#,
            "not json at all",
        ] {
            assert!(extract_turn(l).is_none(), "should have been skipped: {}", l);
        }
    }

    #[test]
    fn a_giant_pasted_turn_is_truncated_not_dropped() {
        let big = "x".repeat(MAX_TURN_CHARS * 2);
        let l = format!(r#"{{"message":{{"role":"user","content":"{}"}}}}"#, big);
        let t = extract_turn(&l).unwrap();
        assert_eq!(t.text.chars().count(), MAX_TURN_CHARS);
    }

    fn turn(role: &str, text: &str) -> Turn {
        Turn { role: role.into(), text: text.into(), ts: None }
    }

    #[test]
    fn chunk_turns_groups_up_to_the_budget_and_never_splits_a_turn() {
        let long = "a".repeat(CHUNK_CHARS - 10);
        let turns = vec![turn("user", &long), turn("assistant", &long), turn("user", "tail")];
        let chunks = chunk_turns(&turns);
        // Each near-budget turn takes its own chunk rather than being split.
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].text.starts_with("user: aaa"));
        assert!(chunks[1].text.starts_with("assistant: aaa"));
        assert_eq!(chunks[2].text, "user: tail");
        for c in &chunks {
            assert!(!c.text.is_empty());
        }
    }

    #[test]
    fn a_chunk_carries_the_timestamp_of_its_first_turn() {
        let turns = vec![
            Turn { role: "user".into(), text: "q".into(), ts: Some("2026-09-08T10:00:00Z".into()) },
            Turn { role: "assistant".into(), text: "a".into(), ts: Some("2026-09-08T10:00:05Z".into()) },
        ];
        let chunks = chunk_turns(&turns);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].ts.as_deref(), Some("2026-09-08T10:00:00Z"));
        assert_eq!(chunks[0].text, "user: q\nassistant: a");
    }

    #[test]
    fn no_turns_makes_no_chunks() {
        assert!(chunk_turns(&[]).is_empty());
    }

    #[test]
    fn a_mach_chore_session_is_recognised_as_machinery() {
        for opener in MACHINE_OPENERS {
            let turns = vec![turn("user", &format!("{} ... rest of prompt", opener))];
            assert!(is_machine_session(&turns), "should be machine: {}", opener);
        }
    }

    /// Real opener lines from reflect.rs's judge prompts.
    #[test]
    fn the_reflect_judges_are_recognised_by_their_subject() {
        for opener in [
            "You are checking a personal knowledge bank for near-duplicate memories",
            "You are checking a personal knowledge graph for duplicate entities",
            "You are analyzing a personal knowledge bank to find deeper patterns",
            "You are auditing a personal knowledge graph's existing edges, so",
            "You are curating a personal knowledge bank's auto-captured candidates",
        ] {
            assert!(is_machine_session(&[turn("user", opener)]), "should be machine: {}", opener);
        }
    }

    #[test]
    fn a_user_merely_mentioning_the_bank_is_not_filtered() {
        let t = turn("user", "can you check my personal knowledge bank for what Moses asked");
        assert!(!is_machine_session(&[t]), "only prompts that OPEN as instructions are machinery");
    }

    #[test]
    fn a_real_conversation_is_not_machinery() {
        let turns = vec![turn("assistant", "hello"), turn("user", "what does umoja do")];
        assert!(!is_machine_session(&turns));
        assert!(!is_machine_session(&[]), "an empty transcript is not a chore session");
    }

    /// The openers are copies of prompts that live elsewhere; if one of
    /// those is reworded this test fails rather than the filter silently
    /// going blind and re-polluting the index.
    #[test]
    fn the_machine_openers_still_match_the_prompts_they_came_from() {
        assert!(crate::ingest::DIGEST_INSTRUCTIONS.starts_with(MACHINE_OPENERS[0]));
        let engagement = crate::ingest::build_engagement_prompt("d", &[(1, "m".into())]);
        assert!(engagement.starts_with(MACHINE_OPENERS[1]));
        let card = crate::reflect::build_entity_card_prompt("E", None, &[(1, "m".into())], None);
        assert!(card.starts_with(MACHINE_OPENERS[2]));
        let probe = crate::ask::build_probe_prompt("q", &[], &[], 1);
        assert!(probe.starts_with(MACHINE_OPENERS[3]));
        let answer = crate::ask::build_answer_prompt("q", &[], &[]);
        assert!(answer.starts_with(MACHINE_OPENERS[4]));
    }

    #[test]
    fn project_from_dir_takes_the_last_path_segment() {
        assert_eq!(project_from_dir("-home-ryutribal-programming-umoja"), "umoja");
        assert_eq!(project_from_dir("-home-ryutribal--config"), "config");
    }
}
