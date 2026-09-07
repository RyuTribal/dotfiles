
//! kb — subcommand dispatch for `mach kb ...`, matching the hand-rolled
//! arg-parsing style the sweep engine's `cli` module already uses (no
//! clap).
use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Read, Write};

use rusqlite::Connection;
use serde::Serialize;

use crate::classify::{self, Classifier, Verdict};
use crate::embed::{Embedder, OllamaEmbedder};
use crate::reflect::{self, ProcessReflectLlm, ReflectLlm, Stage2Result, TIMEOUT_HAIKU, TIMEOUT_SONNET};
use crate::store::{self, AddOutcome, Insight, InsightHit, KbError, Memory, RankedHit};

fn to_io(e: KbError) -> io::Error {
    io::Error::other(e.to_string())
}

fn print_help() {
    println!("mach kb — personal vectorized knowledge-bank engine");
    println!();
    println!("usage: mach kb <subcommand> [args...]");
    println!();
    println!("subcommands:");
    println!("  add \"<content>\" [--source S] [--project P] [--unreviewed]");
    println!("      [--importance N] [--no-classify]");
    println!("                          embed + store a memory (content \"-\" reads stdin);");
    println!("                          N is 1-10, default 5. A close (>0.75 sim) existing");
    println!("                          memory triggers a classifier verdict unless --no-classify.");
    println!("  search \"<query>\" [--limit N] [--json] [--all] [--touch]");
    println!("      [--include-superseded] [--min-score F]");
    println!("                          ranked top-N search (sim/recency/strength blend);");
    println!("                          --touch reinforces the rows actually returned");
    println!("  supersede <old_id> <new_id>");
    println!("                          tombstone <old_id> in favor of <new_id>");
    println!("  review                  interactive review of unreviewed candidates");
    println!("  list [--limit N] [--superseded]");
    println!("                          most recent memories (--superseded: audit view)");
    println!("  forget <id>             permanently delete a memory");
    println!("  reflect                 examine new memories, derive durable insights,");
    println!("                          re-verify a sample of existing ones");
    println!("  insights [--flagged]    list derived insights (confidence + source ids)");
    println!("  insight-forget <id>     permanently delete an insight");
}

/// Runs the kb CLI given the arguments following `kb` in `mach kb ...`.
pub fn run(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    match args.next().as_deref() {
        Some("add") => cmd_add(args),
        Some("search") => cmd_search(args),
        Some("supersede") => cmd_supersede(args),
        Some("review") => cmd_review(args),
        Some("list") => cmd_list(args),
        Some("forget") => cmd_forget(args),
        Some("reflect") => cmd_reflect(args),
        Some("insights") => cmd_insights(args),
        Some("insight-forget") => cmd_insight_forget(args),
        Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("mach kb: unknown subcommand '{}'", other);
            print_help();
            std::process::exit(1);
        }
        None => {
            print_help();
            Ok(())
        }
    }
}

fn cmd_add(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut content: Option<String> = None;
    let mut source: Option<String> = None;
    let mut project: Option<String> = None;
    let mut unreviewed = false;
    let mut importance: i64 = 5;
    let mut no_classify = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--source" => source = args.next(),
            "--project" => project = args.next(),
            "--unreviewed" => unreviewed = true,
            "--importance" => {
                importance = args
                    .next()
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(5)
                    .clamp(1, 10);
            }
            "--no-classify" => no_classify = true,
            "-h" | "--help" => {
                println!(
                    "usage: mach kb add \"<content>\" [--source S] [--project P] [--unreviewed] \
                     [--importance N] [--no-classify]"
                );
                println!("       mach kb add - [--source S] [--project P]   (reads content from stdin)");
                return Ok(());
            }
            other => {
                if content.is_none() {
                    content = Some(other.to_string());
                } else {
                    eprintln!("mach kb add: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }

    let content = match content {
        Some(c) => c,
        None => {
            eprintln!("mach kb add: missing <content> argument");
            std::process::exit(1);
        }
    };
    let content = if content == "-" {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        buf.trim_end().to_string()
    } else {
        content
    };
    if content.trim().is_empty() {
        eprintln!("mach kb add: content is empty");
        std::process::exit(1);
    }

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let embedding = match embedder.embed(&content) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("mach kb add: {}", e);
            std::process::exit(1);
        }
    };

    // Save-time supersession check (Mem0-classifier / Graphiti-tombstone):
    // k-NN top-5 over active memories; a close match (>0.75 cosine) asks a
    // small non-interactive `claude -p` call for a verdict. Skipped
    // entirely with --no-classify (the session-digest hook uses this —
    // n x LLM calls per digest would be wasteful, and digest facts land
    // unreviewed anyway, so `mach kb review` is the curation point).
    let mut best_match_id: Option<i64> = None;
    let verdict = if no_classify {
        Verdict::Add
    } else {
        let similar = store::top_similar(&conn, &embedding, 5).map_err(to_io)?;
        best_match_id = similar.first().map(|(m, _)| m.id);
        match similar.first() {
            Some((_, best_sim)) if *best_sim > 0.75 => {
                let pairs: Vec<(i64, String)> = similar.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
                let classifier = classify::ProcessClassifier::new();
                classifier.classify(&content, &pairs)
            }
            _ => Verdict::Add,
        }
    };

    let now = store::now_rfc3339();
    let outcome = store::apply_verdict(
        &conn,
        verdict,
        &content,
        source.as_deref(),
        project.as_deref(),
        !unreviewed,
        &embedding,
        importance,
        &now,
        best_match_id,
    )
    .map_err(to_io)?;

    match outcome {
        AddOutcome::Added { id } => println!("stored memory #{}", id),
        AddOutcome::AddedAndTombstoned { new_id, old_id, verb } => {
            println!("stored memory #{} ({} memory #{})", new_id, verb, old_id);
        }
        AddOutcome::Skipped { reason } => println!("mach kb add: skipped — {}", reason),
    }
    Ok(())
}

#[derive(Serialize)]
struct SearchHit {
    id: i64,
    content: String,
    source: Option<String>,
    project: Option<String>,
    created_at: String,
    score: f32,
    sim: f32,
    recency: f32,
    strength: f32,
    importance: i64,
    superseded: bool,
    // Insight hits (from mach kb reflect) blended into recall: true marks a
    // row that came from the insights table rather than memories. Present
    // (and false) on memory hits too, so a consumer never has to treat its
    // absence as meaningful.
    derived: bool,
    // Only meaningful when `derived` is true.
    confidence: Option<f64>,
}

fn to_hit(h: RankedHit) -> SearchHit {
    SearchHit {
        id: h.memory.id,
        content: h.memory.content,
        source: h.memory.source,
        project: h.memory.project,
        created_at: h.memory.created_at,
        score: h.score,
        sim: h.sim,
        recency: h.recency,
        strength: h.strength,
        importance: h.memory.importance,
        superseded: h.superseded,
        derived: false,
        confidence: None,
    }
}

fn insight_to_hit(h: InsightHit) -> SearchHit {
    let flagged = h.insight.is_flagged();
    SearchHit {
        id: h.insight.id,
        content: h.insight.text,
        source: Some("derived".to_string()),
        project: None,
        created_at: h.insight.created_at,
        score: h.score,
        sim: h.sim,
        recency: h.recency,
        strength: 0.0,
        importance: 0,
        superseded: flagged,
        derived: true,
        confidence: Some(h.insight.confidence),
    }
}

fn cmd_search(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut query: Option<String> = None;
    let mut limit: usize = 10;
    let mut json = false;
    let mut all = false;
    let mut include_superseded = false;
    let mut touch = false;
    let mut min_score: f32 = 0.0;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--json" => json = true,
            "--all" => all = true,
            "--include-superseded" => include_superseded = true,
            "--touch" => touch = true,
            "--min-score" => min_score = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "-h" | "--help" => {
                println!(
                    "usage: mach kb search \"<query>\" [--limit N] [--json] [--all] [--touch] \
                     [--include-superseded] [--min-score F]"
                );
                return Ok(());
            }
            other => {
                if query.is_none() {
                    query = Some(other.to_string());
                } else {
                    eprintln!("mach kb search: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }

    let query = match query {
        Some(q) => q,
        None => {
            eprintln!("mach kb search: missing <query> argument");
            std::process::exit(1);
        }
    };

    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    let embedder = OllamaEmbedder::new();
    // Blend memory hits and insight hits into one ranked list: fetch up to
    // `limit` unfiltered candidates from each side, merge, sort by score,
    // truncate to `limit`, then apply `min_score` — this reduces to the
    // original single-source behavior whenever there are no insights yet.
    let mut hits: Vec<SearchHit> = match embedder.embed(&query) {
        Ok(q_emb) => {
            let mem_hits =
                store::search_ranked(&conn, &q_emb, limit, all, include_superseded, 0.0, &now).map_err(to_io)?;
            if touch {
                let ids: Vec<i64> = mem_hits.iter().map(|h| h.memory.id).collect();
                store::touch(&conn, &ids, &now).map_err(to_io)?;
            }
            let insight_hits = store::search_insights_ranked(&conn, &q_emb, limit, &now).map_err(to_io)?;
            let mut combined: Vec<SearchHit> =
                mem_hits.into_iter().map(to_hit).chain(insight_hits.into_iter().map(insight_to_hit)).collect();
            combined.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
            combined.truncate(limit);
            combined
        }
        Err(e) => {
            eprintln!(
                "mach kb search: warning: {} — falling back to substring match",
                e
            );
            // No insight fallback here: substring match forces every hit to
            // score 1.0 (not a meaningful ranking to begin with), and
            // insights have no independent text-substring path worth adding
            // for what's already a degraded mode.
            let subs = store::search_substring(&conn, &query, limit, all, include_superseded).map_err(to_io)?;
            let ids: Vec<i64> = subs.iter().map(|(m, _)| m.id).collect();
            if touch {
                store::touch(&conn, &ids, &now).map_err(to_io)?;
            }
            subs.into_iter()
                .map(|(m, score)| {
                    let superseded = m.is_superseded();
                    to_hit(RankedHit { memory: m, score, sim: score, recency: 0.0, strength: 0.0, superseded })
                })
                .collect()
        }
    };

    if min_score > 0.0 {
        hits.retain(|h| h.score >= min_score);
    }

    if json {
        println!("{}", serde_json::to_string(&hits)?);
    } else if hits.is_empty() {
        println!("no matches");
    } else {
        let mut any_superseded = false;
        for h in &hits {
            any_superseded |= h.superseded && !h.derived;
            let flag = if h.derived { 'D' } else { ' ' };
            let sup = if h.superseded { '!' } else { ' ' };
            let label = if h.derived {
                format!("#i{:<4}", h.id)
            } else {
                format!("#{:<5}", h.id)
            };
            let suffix = match h.confidence {
                Some(c) => format!("  (confidence {:.2})", c),
                None => String::new(),
            };
            println!(
                "{}{}{:>6.3}  {} {}{}",
                flag,
                sup,
                h.score,
                label,
                truncate(&h.content, 90),
                suffix
            );
        }
        if hits.iter().any(|h| h.derived) {
            println!("\n(D = derived insight from `mach kb reflect` — see `mach kb insights`)");
        }
        if any_superseded {
            println!("(! = superseded — scored ×0.1, shown via --include-superseded)");
        }
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn cmd_supersede(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let old_id: i64 = match args.next().and_then(|s| s.parse().ok()) {
        Some(v) => v,
        None => {
            eprintln!("mach kb supersede: missing or invalid <old_id>");
            std::process::exit(1);
        }
    };
    let new_id: i64 = match args.next().and_then(|s| s.parse().ok()) {
        Some(v) => v,
        None => {
            eprintln!("mach kb supersede: missing or invalid <new_id>");
            std::process::exit(1);
        }
    };

    let conn = store::open().map_err(to_io)?;
    if store::get(&conn, old_id).map_err(to_io)?.is_none() {
        eprintln!("mach kb supersede: no memory with id {}", old_id);
        std::process::exit(1);
    }
    if store::get(&conn, new_id).map_err(to_io)?.is_none() {
        eprintln!("mach kb supersede: no memory with id {}", new_id);
        std::process::exit(1);
    }

    let now = store::now_rfc3339();
    if store::supersede(&conn, old_id, new_id, &now).map_err(to_io)? {
        println!("superseded memory #{} -> #{}", old_id, new_id);
        Ok(())
    } else {
        eprintln!("mach kb supersede: memory #{} is already superseded", old_id);
        std::process::exit(1);
    }
}

fn cmd_review(_args: impl Iterator<Item = String>) -> io::Result<()> {
    let conn = store::open().map_err(to_io)?;
    let mut pending = store::unreviewed(&conn).map_err(to_io)?;
    if pending.is_empty() {
        println!("mach kb review: no unreviewed candidates");
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        eprintln!("mach kb review: stdin is not a terminal — this is an interactive command");
    }

    let embedder = OllamaEmbedder::new();
    let total = pending.len();
    let mut remaining = total;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    'outer: for (i, mem) in pending.iter_mut().enumerate() {
        loop {
            println!(
                "\n[{}/{}] #{}  (source: {}, project: {})",
                i + 1,
                total,
                mem.id,
                mem.source.as_deref().unwrap_or("-"),
                mem.project.as_deref().unwrap_or("-"),
            );
            println!("    {}", mem.content);
            print!("  [k]eep  [d]elete  [e]dit  [s]kip  [q]uit > ");
            stdout.flush()?;

            let mut line = String::new();
            if stdin.read_line(&mut line)? == 0 {
                println!();
                break 'outer; // EOF
            }
            match line.trim().to_lowercase().as_str() {
                "k" | "keep" => {
                    store::set_reviewed(&conn, mem.id, true).map_err(to_io)?;
                    remaining -= 1;
                    break;
                }
                "d" | "delete" => {
                    store::delete(&conn, mem.id).map_err(to_io)?;
                    remaining -= 1;
                    break;
                }
                "e" | "edit" => {
                    print!("  new content (blank line = keep current text): ");
                    stdout.flush()?;
                    let mut edit = String::new();
                    stdin.read_line(&mut edit)?;
                    let edit = edit.trim();
                    if !edit.is_empty() {
                        let new_embedding = match embedder.embed(edit) {
                            Ok(v) => Some(v),
                            Err(e) => {
                                eprintln!("  warning: re-embed failed ({}) — keeping old vector", e);
                                None
                            }
                        };
                        store::update_content(&conn, mem.id, edit, new_embedding.as_deref())
                            .map_err(to_io)?;
                        mem.content = edit.to_string();
                    }
                    store::set_reviewed(&conn, mem.id, true).map_err(to_io)?;
                    remaining -= 1;
                    break;
                }
                "s" | "skip" => break,
                "q" | "quit" => {
                    println!("stopped review — {} still unreviewed", remaining);
                    break 'outer;
                }
                other => {
                    println!("  unrecognized input '{}' — try k/d/e/s/q", other);
                    continue;
                }
            }
        }
    }
    if remaining == 0 {
        println!("\nreview complete — nothing left unreviewed");
    }
    Ok(())
}

fn cmd_list(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut limit: usize = 20;
    let mut superseded_only = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(20),
            "--superseded" => superseded_only = true,
            "-h" | "--help" => {
                println!("usage: mach kb list [--limit N] [--superseded]");
                return Ok(());
            }
            other => {
                eprintln!("mach kb list: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let conn: Connection = store::open().map_err(to_io)?;
    let rows = store::list(&conn, Some(limit), superseded_only).map_err(to_io)?;
    if rows.is_empty() {
        println!(
            "{}",
            if superseded_only { "no superseded memories" } else { "no memories stored yet" }
        );
        return Ok(());
    }
    for m in &rows {
        let flag = if m.reviewed { ' ' } else { '*' };
        let sup = if m.is_superseded() { '!' } else { ' ' };
        println!(
            "{}{}{:>5}  {}  {:<10} {:<12}  {}",
            flag,
            sup,
            m.id,
            m.created_at,
            m.source.as_deref().unwrap_or("-"),
            m.project.as_deref().unwrap_or("-"),
            truncate(&m.content, 70)
        );
    }
    println!("\n(* = awaiting review — run `mach kb review`; ! = superseded — run `mach kb list --superseded`)");
    Ok(())
}

fn cmd_forget(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let id_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("mach kb forget: missing <id> argument");
            std::process::exit(1);
        }
    };
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mach kb forget: '{}' is not a valid id", id_str);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    let existed = store::delete(&conn, id).map_err(to_io)?;
    if existed {
        println!("forgot memory #{}", id);
        Ok(())
    } else {
        eprintln!("mach kb forget: no memory with id {}", id);
        std::process::exit(1);
    }
}

// --- reflect: the periodic reflection pass ---

const REFLECT_WORKING_SET_CAP: usize = 60;
const REFLECT_NEIGHBORS_PER_MEMORY: usize = 3;
const REFLECT_EVIDENCE_PER_QUESTION: usize = 8;
const REFLECT_EXISTING_INSIGHTS_CONTEXT: usize = 3;
const REFLECT_VERIFICATION_SAMPLE: usize = 5;
const REFLECT_VERIFICATION_CANDIDATES: usize = 5;

fn cmd_reflect(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach kb reflect");
                return Ok(());
            }
            other => {
                eprintln!("mach kb reflect: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    let state = store::get_reflect_state(&conn).map_err(to_io)?;
    let last_id = state.last_memory_id.unwrap_or(0);

    // Step 1: input selection.
    let new_memories = store::memories_since(&conn, last_id).map_err(to_io)?;
    if new_memories.is_empty() {
        println!("mach kb reflect: nothing new");
        return Ok(());
    }

    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();

    // Working set: the new memories, bridged with each one's top-3
    // neighbors from the whole active store (reviewed and unreviewed
    // alike) so a fresh fact can connect to an older pattern. Deduped by
    // id via the BTreeMap; capped at 60, most-recent-first when over.
    let mut working: BTreeMap<i64, Memory> = BTreeMap::new();
    for m in &new_memories {
        working.insert(m.id, m.clone());
    }
    for m in &new_memories {
        if let Some(emb) = &m.embedding {
            if let Ok(neighbors) = store::top_similar_active(&conn, emb, REFLECT_NEIGHBORS_PER_MEMORY) {
                for (nm, _) in neighbors {
                    working.entry(nm.id).or_insert(nm);
                }
            }
        }
    }
    let mut working_vec: Vec<Memory> = working.into_values().collect();
    if working_vec.len() > REFLECT_WORKING_SET_CAP {
        working_vec.sort_by(|a, b| b.id.cmp(&a.id));
        working_vec.truncate(REFLECT_WORKING_SET_CAP);
    }
    working_vec.sort_by_key(|m| m.id);
    let examined = working_vec.len();

    // Step 2 (stage 1): salient questions, one haiku call over the whole
    // working set.
    let question_lines: Vec<(i64, String)> = working_vec.iter().map(|m| (m.id, m.content.clone())).collect();
    let q_prompt = reflect::build_questions_prompt(&question_lines);
    let questions: Vec<String> = match llm.call("haiku", &q_prompt, TIMEOUT_HAIKU) {
        Ok(out) => reflect::parse_questions(&out),
        Err(_) => Vec::new(),
    };

    // Step 3 (stage 2): one durable insight per question, sonnet — only
    // when the evidence actually supports it.
    let mut insights_added = 0usize;
    for question in &questions {
        let q_emb = match embedder.embed(question) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let evidence = match store::top_similar_active(&conn, &q_emb, REFLECT_EVIDENCE_PER_QUESTION) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if evidence.len() < 2 {
            // Can never clear the 2-citation floor regardless of what the
            // model says — skip the sonnet call entirely.
            continue;
        }
        let existing_near =
            store::top_similar_insights(&conn, &q_emb, REFLECT_EXISTING_INSIGHTS_CONTEXT).unwrap_or_default();

        let evidence_pairs: Vec<(i64, String)> = evidence.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
        let existing_pairs: Vec<(i64, String)> =
            existing_near.iter().map(|(ins, _)| (ins.id, ins.text.clone())).collect();
        let prompt = reflect::build_insight_prompt(question, &evidence_pairs, &existing_pairs);

        let raw = match llm.call("sonnet", &prompt, TIMEOUT_SONNET) {
            Ok(out) => out,
            Err(_) => continue,
        };

        if let Stage2Result::Insight { text, memory_ids } = reflect::parse_stage2(&raw) {
            // Defensive cross-check: citations must be real evidence rows
            // we actually showed the model, not hallucinated ids — a
            // fabricated id must not let a claim slip past the floor.
            let evidence_ids: std::collections::HashSet<i64> = evidence.iter().map(|(m, _)| m.id).collect();
            let mut valid_ids: Vec<i64> = memory_ids.into_iter().filter(|id| evidence_ids.contains(id)).collect();
            valid_ids.sort_unstable();
            valid_ids.dedup();
            if valid_ids.len() < 2 {
                continue;
            }
            let confidence = reflect::compute_confidence(valid_ids.len());
            let source_ids: Vec<String> = valid_ids.iter().map(|id| id.to_string()).collect();
            let text_embedding = embedder.embed(&text).ok();
            if store::insert_insight(&conn, &text, confidence, &source_ids, text_embedding.as_deref()).is_ok() {
                insights_added += 1;
            }
        }
    }

    // Step 4: re-verification, same run — up to 5 oldest insights by
    // last_verified_at (never-verified first). Never deletes, only flags.
    let mut flagged = 0usize;
    let mut verified = 0usize;
    let stale = store::insights_due_for_verification(&conn, REFLECT_VERIFICATION_SAMPLE).map_err(to_io)?;
    for insight in &stale {
        let mut broken_citation = false;
        for sid in &insight.source_ids {
            if sid.starts_with('i') || sid.starts_with('I') {
                continue; // insight references aren't re-checked here
            }
            let still_active = match sid.parse::<i64>() {
                Ok(mid) => matches!(store::get(&conn, mid), Ok(Some(m)) if m.invalidated_at.is_none()),
                Err(_) => false,
            };
            if !still_active {
                broken_citation = true;
                break;
            }
        }

        if broken_citation {
            store::flag_insight(&conn, insight.id, &now).map_err(to_io)?;
            flagged += 1;
            continue;
        }

        let contradicted = match embedder.embed(&insight.text) {
            Ok(emb) => match store::top_similar_active(&conn, &emb, REFLECT_VERIFICATION_CANDIDATES) {
                Ok(candidates) if !candidates.is_empty() => {
                    let pairs: Vec<(i64, String)> =
                        candidates.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
                    let prompt = reflect::build_contradiction_prompt(&insight.text, &pairs);
                    match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
                        Ok(out) => reflect::parse_contradiction(&out).is_some(),
                        Err(_) => false,
                    }
                }
                _ => false,
            },
            Err(_) => false,
        };

        if contradicted {
            store::flag_insight(&conn, insight.id, &now).map_err(to_io)?;
            flagged += 1;
        } else {
            store::mark_insight_verified(&conn, insight.id, &now).map_err(to_io)?;
            verified += 1;
        }
    }

    // Step 5: advance the watermark to the newest memory id actually
    // examined this run (not the bridged neighbors, which may be older).
    let new_watermark = new_memories.iter().map(|m| m.id).max();
    store::update_reflect_state(&conn, &now, new_watermark).map_err(to_io)?;

    println!(
        "mach kb reflect: examined={} questions={} insights_added={} flagged={} verified={}",
        examined,
        questions.len(),
        insights_added,
        flagged,
        verified
    );
    Ok(())
}

fn cmd_insights(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut flagged_only = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--flagged" => flagged_only = true,
            "-h" | "--help" => {
                println!("usage: mach kb insights [--flagged]");
                return Ok(());
            }
            other => {
                eprintln!("mach kb insights: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let conn = store::open().map_err(to_io)?;
    let rows: Vec<Insight> = store::list_insights(&conn, flagged_only).map_err(to_io)?;
    if rows.is_empty() {
        println!("{}", if flagged_only { "no flagged insights" } else { "no insights yet — run `mach kb reflect`" });
        return Ok(());
    }
    for ins in &rows {
        let flag = if ins.is_flagged() { '!' } else { ' ' };
        println!(
            "{}i{:<5} {}  (confidence {:.2}, sources: {})",
            flag,
            ins.id,
            truncate(&ins.text, 80),
            ins.confidence,
            ins.source_ids.join(", "),
        );
    }
    println!(
        "\n(! = flagged by re-verification — evidence no longer supports it; \
         `mach kb insight-forget <id>` removes one)"
    );
    Ok(())
}

fn cmd_insight_forget(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let id_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("mach kb insight-forget: missing <id> argument");
            std::process::exit(1);
        }
    };
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mach kb insight-forget: '{}' is not a valid id", id_str);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    let existed = store::delete_insight(&conn, id).map_err(to_io)?;
    if existed {
        println!("forgot insight #{}", id);
        Ok(())
    } else {
        eprintln!("mach kb insight-forget: no insight with id {}", id);
        std::process::exit(1);
    }
}
