
//! kb — subcommand dispatch for `mach kb ...`, matching the hand-rolled
//! arg-parsing style the sweep engine's `cli` module already uses (no
//! clap).
use std::collections::{BTreeMap, HashSet};
use std::io::{self, IsTerminal, Read, Write};

use rusqlite::Connection;
use serde::Serialize;

use crate::classify::{self, Classifier, Verdict};
use crate::embed::{Embedder, OllamaEmbedder};
use crate::export;
use crate::reflect::{self, ProcessReflectLlm, ReflectLlm, Stage2Result, ThemeResult, TIMEOUT_HAIKU, TIMEOUT_SONNET};
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
    println!("  list [--limit N] [--superseded] [--dormant]");
    println!("                          most recent memories (--superseded/--dormant: audit views)");
    println!("  forget <id>             permanently delete a memory");
    println!("  wake <id>               clear a memory's dormant status (mach kb list --dormant)");
    println!("  reflect [--meta]        examine new memories, derive/reinforce durable");
    println!("                          insights, re-verify a sample of existing ones, put");
    println!("                          stale low-importance memories to sleep (consolidating");
    println!("                          related ones), and (when triggered, or forced via");
    println!("                          --meta) find themes across insights");
    println!("  insights [--flagged]    list derived insights and themes (confidence + source ids)");
    println!("  insight-forget <id>     permanently delete an insight or theme");
    println!("  tree                    render the theme -> insight -> memory hierarchy");
    println!("  model [--json]          compact mental-model view (active themes/insights only,");
    println!("                          no source ids or memory leaves) for context injection");
    println!("  export [--out FILE]     full-fidelity JSONL backup of every row (default: stdout)");
    println!("  import FILE [--merge]   restore from a `mach kb export` file; refuses a non-empty");
    println!("                          db unless --merge (upsert by id, last-write-wins)");
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
        Some("wake") => cmd_wake(args),
        Some("reflect") => cmd_reflect(args),
        Some("insights") => cmd_insights(args),
        Some("insight-forget") => cmd_insight_forget(args),
        Some("tree") => cmd_tree(args),
        Some("model") => cmd_model(args),
        Some("export") => cmd_export(args),
        Some("import") => cmd_import(args),
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
    // Only meaningful when `derived` is true: 1 = a plain insight, 2 = a
    // level-2 theme. `None` on plain memory hits — lets `kb-recall.sh`
    // distinguish "[derived belief]" from "[derived theme]".
    level: Option<i64>,
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
        level: None,
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
        level: Some(h.insight.level),
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
                let prefix = if h.level == Some(2) { 't' } else { 'i' };
                format!("#{}{:<4}", prefix, h.id)
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
    let mut dormant_only = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(20),
            "--superseded" => superseded_only = true,
            "--dormant" => dormant_only = true,
            "-h" | "--help" => {
                println!("usage: mach kb list [--limit N] [--superseded] [--dormant]");
                return Ok(());
            }
            other => {
                eprintln!("mach kb list: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let conn: Connection = store::open().map_err(to_io)?;
    let rows = if dormant_only {
        store::list_dormant(&conn, Some(limit)).map_err(to_io)?
    } else {
        store::list(&conn, Some(limit), superseded_only).map_err(to_io)?
    };
    if rows.is_empty() {
        println!(
            "{}",
            if dormant_only {
                "no dormant memories"
            } else if superseded_only {
                "no superseded memories"
            } else {
                "no memories stored yet"
            }
        );
        return Ok(());
    }
    for m in &rows {
        let flag = if m.reviewed { ' ' } else { '*' };
        let sup = if m.is_superseded() { '!' } else { ' ' };
        let dor = if m.is_dormant() { 'z' } else { ' ' };
        println!(
            "{}{}{}{:>5}  {}  {:<10} {:<12}  {}",
            flag,
            sup,
            dor,
            m.id,
            m.created_at,
            m.source.as_deref().unwrap_or("-"),
            m.project.as_deref().unwrap_or("-"),
            truncate(&m.content, 70)
        );
    }
    println!(
        "\n(* = awaiting review — run `mach kb review`; ! = superseded — run `mach kb list --superseded`; \
         z = dormant — run `mach kb list --dormant`, wake with `mach kb wake <id>`)"
    );
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

fn cmd_wake(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let id_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("mach kb wake: missing <id> argument");
            std::process::exit(1);
        }
    };
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mach kb wake: '{}' is not a valid id", id_str);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    if store::wake(&conn, id, &now).map_err(to_io)? {
        println!("woke memory #{}", id);
        Ok(())
    } else {
        eprintln!("mach kb wake: no dormant memory with id {}", id);
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
const META_EVIDENCE_PER_CLUSTER: usize = 6;

fn cmd_reflect(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut force_meta = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--meta" => force_meta = true,
            "-h" | "--help" => {
                println!("usage: mach kb reflect [--meta]");
                println!(
                    "       --meta forces the meta-reflection (theme) pass to run this time \
                     regardless of its normal trigger conditions — for manual inspection, not \
                     routine use."
                );
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
    let has_new = !new_memories.is_empty();

    // Cheap, DB-only pre-check — no network, no `claude` spawn — so an
    // early exit costs nothing on a laptop merely waking up opportunistically
    // (see the timer's OnUnitInactiveSec re-arm). `insights_due_for_verification`
    // returns up to REFLECT_VERIFICATION_SAMPLE active insights
    // unconditionally (it has no staleness filter of its own — see its own
    // doc comment), so "empty" here precisely means zero active insights
    // exist yet, not merely that none happen to be old enough to re-check.
    // The nightly dedupe pass needs no separate check here: every candidate
    // pair requires at least one side to be "new" (see
    // `reflect::dedupe_candidate_pairs`), so `!has_new` already rules its
    // queue out too. A --meta-only invocation (e.g. for manual testing once
    // enough insights already exist) must still be able to run even when
    // there's nothing new or due — this is the only early exit before the
    // connectivity guard below.
    let verification_queue = store::insights_due_for_verification(&conn, REFLECT_VERIFICATION_SAMPLE).map_err(to_io)?;
    if !has_new && !force_meta && verification_queue.is_empty() {
        println!("mach kb reflect: nothing new");
        return Ok(());
    }

    // Connectivity guard: everything from here on may spawn `claude -p`
    // (stage 1/2, re-verification's contradiction check, dormancy
    // consolidation, the nightly dedupe judge, meta-reflection). Local
    // `ollama` embedding calls are unaffected by wifi and need no such
    // guard — only a `claude` call ever leaves the machine. A laptop that's
    // asleep, suspended, or off wifi must defer the whole pass rather than
    // burn through a string of 30s-60s timeouts and, worse, partially
    // advance state it never finished examining.
    if !reflect::claude_reachable() {
        println!("mach kb reflect: offline, deferring");
        return Ok(());
    }

    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    // Set on any `claude` call failing mid-run (a network drop after the
    // connectivity guard above passed, a spawn error, a timeout) — gates
    // the watermark advance at the very end so a degraded run stays fully
    // retry-able next time instead of orphaning whatever it never got to.
    let mut llm_failed = false;

    let mut examined = 0usize;
    let mut questions_count = 0usize;
    let mut insights_added = 0usize;
    let mut reinforced = 0usize;

    if has_new {
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
        examined = working_vec.len();

        // Step 2 (stage 1): salient questions, one haiku call over the
        // whole working set.
        let question_lines: Vec<(i64, String)> = working_vec.iter().map(|m| (m.id, m.content.clone())).collect();
        let q_prompt = reflect::build_questions_prompt(&question_lines);
        let questions: Vec<String> = match llm.call("haiku", &q_prompt, TIMEOUT_HAIKU) {
            Ok(out) => reflect::parse_questions(&out),
            Err(_) => {
                llm_failed = true;
                Vec::new()
            }
        };
        questions_count = questions.len();

        // Step 3 (stage 2): one durable insight — or a reinforcement of an
        // existing one — per question, sonnet, only when the evidence
        // actually supports it.
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
                // Can never clear the 2-citation floor regardless of what
                // the model says — skip the sonnet call entirely.
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
                Err(_) => {
                    llm_failed = true;
                    continue;
                }
            };

            let evidence_ids: HashSet<i64> = evidence.iter().map(|(m, _)| m.id).collect();
            match reflect::parse_stage2(&raw) {
                Stage2Result::Insight { text, memory_ids } => {
                    // Defensive cross-check: citations must be real
                    // evidence rows we actually showed the model, not
                    // hallucinated ids — a fabricated id must not let a
                    // claim slip past the floor.
                    let mut valid_ids: Vec<i64> = memory_ids.into_iter().filter(|id| evidence_ids.contains(id)).collect();
                    valid_ids.sort_unstable();
                    valid_ids.dedup();
                    if valid_ids.len() < 2 {
                        continue;
                    }
                    let confidence = reflect::compute_confidence(valid_ids.len());
                    let source_ids: Vec<String> = valid_ids.iter().map(|id| id.to_string()).collect();
                    let text_embedding = embedder.embed(&text).ok();
                    if store::insert_insight(&conn, &text, confidence, &source_ids, text_embedding.as_deref()).is_ok()
                    {
                        insights_added += 1;
                    }
                }
                Stage2Result::Reinforce { insight_id, memory_ids } => {
                    // Same defensive cross-check against real evidence ids,
                    // then a second one: the citations must include at
                    // least one memory id NOT already in the insight's
                    // source_ids — reinforcing with only already-cited ids
                    // is a no-op the model shouldn't get credit for.
                    let mut valid_ids: Vec<i64> = memory_ids.into_iter().filter(|id| evidence_ids.contains(id)).collect();
                    valid_ids.sort_unstable();
                    valid_ids.dedup();
                    if valid_ids.is_empty() {
                        continue;
                    }
                    match store::get_insight(&conn, insight_id) {
                        Ok(Some(target)) if target.is_active() => {
                            let existing_raw: HashSet<i64> =
                                target.source_ids.iter().filter_map(|s| s.parse::<i64>().ok()).collect();
                            let new_ids: Vec<i64> =
                                valid_ids.into_iter().filter(|id| !existing_raw.contains(id)).collect();
                            if new_ids.is_empty() {
                                continue; // only already-cited ids — reject
                            }
                            if store::reinforce_insight(&conn, insight_id, &new_ids, &now).is_ok() {
                                reinforced += 1;
                            }
                        }
                        _ => continue, // stale/hallucinated target id — never crash, just skip
                    }
                }
                Stage2Result::None | Stage2Result::Rejected => {}
            }
        }
    }

    // Step 4: re-verification, same run — up to 5 oldest insights by
    // last_verified_at (never-verified first; reuses the sample already
    // fetched by the cheap pre-check above rather than querying twice).
    // Never deletes, only flags. Runs across BOTH levels (level-2 themes
    // share this table and this query), so a theme is just as due for a
    // check as a plain insight.
    let mut flagged = 0usize;
    let mut verified = 0usize;
    let stale = verification_queue;
    for insight in &stale {
        let mut broken_citation = false;
        for sid in &insight.source_ids {
            if let Some(rest) = sid.strip_prefix('i').or_else(|| sid.strip_prefix('I')) {
                // An `i<id>` citation — most commonly a theme citing one of
                // its level-1 insights. Now actually resolved and checked:
                // if the cited insight is gone, invalidated, or flagged,
                // the citing row (the theme) is flagged too — a theme is
                // only as sound as the insights it names.
                let ok = match rest.parse::<i64>() {
                    Ok(cid) => matches!(
                        store::get_insight(&conn, cid),
                        Ok(Some(ci)) if ci.is_active() && !ci.is_flagged()
                    ),
                    Err(_) => false,
                };
                if !ok {
                    broken_citation = true;
                    break;
                }
                continue;
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

        let mut call_failed = false;
        let contradicted = match embedder.embed(&insight.text) {
            Ok(emb) => match store::top_similar_active(&conn, &emb, REFLECT_VERIFICATION_CANDIDATES) {
                Ok(candidates) if !candidates.is_empty() => {
                    let pairs: Vec<(i64, String)> =
                        candidates.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
                    let prompt = reflect::build_contradiction_prompt(&insight.text, &pairs);
                    match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
                        Ok(out) => reflect::parse_contradiction(&out).is_some(),
                        Err(_) => {
                            call_failed = true;
                            false
                        }
                    }
                }
                _ => false,
            },
            Err(_) => false,
        };

        if call_failed {
            // The judge call itself failed (offline mid-run, spawn error,
            // timeout) — leave this insight's verification state exactly
            // as it was rather than crediting a check that never actually
            // happened; it stays due and gets retried on a later run.
            llm_failed = true;
            continue;
        }

        if contradicted {
            store::flag_insight(&conn, insight.id, &now).map_err(to_io)?;
            flagged += 1;
        } else {
            store::mark_insight_verified(&conn, insight.id, &now).map_err(to_io)?;
            verified += 1;
        }
    }

    // Step 4.5: dormancy — put stale, low-importance, uncited memories (and
    // any never-curated digest row past its own 30-day floor) to sleep,
    // then consolidate this run's freshly-dormant rows into new durable
    // facts where a cluster of them shares something worth keeping. Runs
    // every invocation regardless of has_new/force_meta — it's a nightly
    // sweep over the whole active store, not gated on new material.
    let (dormant, consolidated, dormancy_llm_failed) = run_dormancy_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || dormancy_llm_failed;

    // Step 4.6: nightly dedupe — fast capture paths (`mach note`, digests)
    // deliberately skip save-time dedupe classification, so near-duplicate
    // raw memories accumulate; this catches them here, where LLM time is
    // free. Only pairs touching a memory created since the last reflect
    // run are considered, capped at reflect::DEDUPE_MAX_PAIRS_PER_RUN.
    let new_ids: HashSet<i64> = new_memories.iter().map(|m| m.id).collect();
    let (deduped, dedupe_llm_failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).map_err(to_io)?;
    llm_failed = llm_failed || dedupe_llm_failed;

    // Step 5: meta-reflection (theme) pass — only when triggered, or
    // forced via --meta for manual runs.
    let level1_active = store::count_active_insights_level(&conn, 1).map_err(to_io)?;
    let newest_theme = store::newest_active_theme(&conn).map_err(to_io)?;
    let since_newest_theme = match &newest_theme {
        Some(t) => store::count_active_level1_created_after(&conn, t.id).map_err(to_io)?,
        None => 0,
    };
    let meta_trigger = reflect::should_run_meta_pass(level1_active, newest_theme.is_some(), since_newest_theme);
    let mut themes_added = 0usize;
    if meta_trigger || force_meta {
        let (added, meta_llm_failed) = run_meta_pass(&conn, &embedder, &llm).map_err(to_io)?;
        themes_added = added;
        llm_failed = llm_failed || meta_llm_failed;
    }

    // Step 6: advance the watermark to the newest memory id actually
    // examined this run (not the bridged neighbors, which may be older) —
    // only when there was anything new AND nothing failed along the way:
    // a --meta-only invocation with nothing new must never clobber the
    // existing watermark back to NULL, and a run that went offline or hit
    // a spawn/timeout partway through must leave its unexamined remainder
    // fully retry-able next time rather than orphaning it behind an
    // advanced watermark.
    if reflect::should_advance_watermark(has_new, llm_failed) {
        let new_watermark = new_memories.iter().map(|m| m.id).max();
        store::update_reflect_state(&conn, &now, new_watermark).map_err(to_io)?;
    }

    println!(
        "mach kb reflect: examined={} questions={} insights_added={} reinforced={} \
         themes_added={} flagged={} verified={} dormant={} consolidated={} deduped={}{}",
        examined,
        questions_count,
        insights_added,
        reinforced,
        themes_added,
        flagged,
        verified,
        dormant,
        consolidated,
        deduped,
        if llm_failed { " (degraded: some claude calls failed — watermark not advanced)" } else { "" }
    );
    Ok(())
}

/// The dormancy (forgetting) pass: evaluates every active memory against
/// `store::memory_qualifies_for_dormancy` and puts qualifying rows to
/// sleep, then clusters this run's freshly-dormant rows by embedding
/// similarity (`>= DORMANCY_CONSOLIDATION_MIN_SIM`, floor
/// `DORMANCY_CONSOLIDATION_MIN_CLUSTER`) and asks one haiku call per
/// cluster for a single consolidated fact, stored as a new active memory
/// (source `consolidation:<id>,<id>,...`). Returns `(dormant_count,
/// consolidated_count, any_llm_call_failed)`. Never deletes anything —
/// dormancy is a status flag, and a consolidation summary is an addition,
/// not a replacement. Marking memories dormant needs no network at all;
/// only the consolidation sub-step's haiku call can fail (offline mid-run,
/// spawn error, timeout) — when it does, that cluster is simply skipped
/// (as before) and the caller is told via the third return value so the
/// watermark doesn't advance on a degraded run.
fn run_dormancy_pass(
    conn: &Connection,
    embedder: &OllamaEmbedder,
    llm: &ProcessReflectLlm,
    now: &str,
) -> Result<(usize, usize, bool), KbError> {
    let mut llm_failed = false;
    let cited = store::cited_memory_ids(conn)?;
    let pool = store::active_memories_for_dormancy(conn)?;
    let mut newly_dormant: Vec<i64> = Vec::new();
    for m in &pool {
        if store::memory_qualifies_for_dormancy(m, cited.contains(&m.id), now) && store::set_dormant(conn, m.id, now)? {
            newly_dormant.push(m.id);
        }
    }
    let dormant_count = newly_dormant.len();

    // Consolidation: only over this run's freshly-dormant rows, and never
    // over a row that is itself a past consolidation summary (guardrail —
    // a consolidated fact never gets folded into a later one).
    let mut consolidated = 0usize;
    if !newly_dormant.is_empty() {
        let mut rows: Vec<Memory> = Vec::new();
        for id in &newly_dormant {
            if let Some(m) = store::get(conn, *id)? {
                rows.push(m);
            }
        }
        let eligible: Vec<&Memory> =
            rows.iter().filter(|m| !m.source.as_deref().unwrap_or("").starts_with("consolidation")).collect();
        let items: Vec<(i64, Vec<f32>)> =
            eligible.iter().filter_map(|m| m.embedding.clone().map(|e| (m.id, e))).collect();
        let clusters = reflect::cluster_by_similarity(
            &items,
            reflect::DORMANCY_CONSOLIDATION_MIN_SIM,
            reflect::DORMANCY_CONSOLIDATION_MIN_CLUSTER,
        );
        let by_id: BTreeMap<i64, &Memory> = eligible.iter().map(|m| (m.id, *m)).collect();

        for cluster in clusters {
            let cluster_rows: Vec<&Memory> = cluster.iter().filter_map(|id| by_id.get(id).copied()).collect();
            let pairs: Vec<(i64, String)> = cluster_rows.iter().map(|m| (m.id, m.content.clone())).collect();
            let prompt = reflect::build_consolidation_prompt(&pairs);
            let raw = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
                Ok(out) => out,
                Err(_) => {
                    llm_failed = true;
                    continue;
                }
            };
            if let Some(fact) = reflect::parse_consolidation(&raw) {
                let cluster_ids: Vec<String> = cluster_rows.iter().map(|m| m.id.to_string()).collect();
                let source = format!("consolidation:{}", cluster_ids.join(","));
                let embedding = embedder.embed(&fact).ok();
                if store::insert(conn, &fact, Some(&source), None, true, embedding.as_deref(), 4).is_ok() {
                    consolidated += 1;
                }
            }
        }
    }

    Ok((dormant_count, consolidated, llm_failed))
}

/// The meta-reflection (theme) pass: clusters active, not-yet-themed
/// level-1 insights by embedding similarity and asks one sonnet call per
/// cluster for a single unifying theme statement. Returns `(themes_added,
/// any_llm_call_failed)`. Never touches the watermark or the per-memory
/// reinforcement/insight logic above — this is a self-contained extra
/// pass over the insights table.
fn run_meta_pass(conn: &Connection, embedder: &OllamaEmbedder, llm: &ProcessReflectLlm) -> Result<(usize, bool), KbError> {
    let themed = store::themed_insight_ids(conn)?;
    let mut pool: Vec<Insight> =
        store::active_insights_by_level(conn, 1)?.into_iter().filter(|i| !themed.contains(&i.id)).collect();
    pool.sort_by_key(|i| i.id); // oldest first — clustering's seed order

    let items: Vec<(i64, Vec<f32>)> =
        pool.iter().filter_map(|i| i.embedding.clone().map(|e| (i.id, e))).collect();
    let clusters = reflect::cluster_insights_by_similarity(&items);
    let by_id: BTreeMap<i64, &Insight> = pool.iter().map(|i| (i.id, i)).collect();

    let mut llm_failed = false;
    let mut themes_added = 0usize;
    for cluster in clusters {
        let cluster_insights: Vec<&Insight> = cluster.iter().filter_map(|id| by_id.get(id).copied()).collect();
        if cluster_insights.len() < 2 {
            continue;
        }
        let known_ids: HashSet<i64> = cluster_insights.iter().map(|i| i.id).collect();
        let insight_pairs: Vec<(i64, String)> = cluster_insights.iter().map(|i| (i.id, i.text.clone())).collect();

        // Top evidence for the cluster: pool each member insight's own
        // raw-memory citations (deduped, insertion order), cap at
        // META_EVIDENCE_PER_CLUSTER, resolve to current content.
        let mut evidence_ids: Vec<i64> = Vec::new();
        for ins in &cluster_insights {
            for sid in &ins.source_ids {
                if let Ok(mid) = sid.parse::<i64>() {
                    if !evidence_ids.contains(&mid) {
                        evidence_ids.push(mid);
                    }
                }
            }
        }
        evidence_ids.truncate(META_EVIDENCE_PER_CLUSTER);
        let mut evidence_pairs: Vec<(i64, String)> = Vec::new();
        for mid in &evidence_ids {
            if let Ok(Some(m)) = store::get(conn, *mid) {
                evidence_pairs.push((m.id, m.content));
            }
        }
        if evidence_pairs.is_empty() {
            continue; // never a theme without at least one raw memory row
        }

        let prompt = reflect::build_theme_prompt(&insight_pairs, &evidence_pairs);
        let raw = match llm.call("sonnet", &prompt, TIMEOUT_SONNET) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue;
            }
        };

        if let ThemeResult::Theme { text, insight_ids, memory_ids } = reflect::parse_theme(&raw, &known_ids) {
            // Defensive cross-check, same pattern as stage 2: raw citations
            // must be real rows actually shown to the model this call.
            let valid_evidence: HashSet<i64> = evidence_pairs.iter().map(|(id, _)| *id).collect();
            let mut valid_memory_ids: Vec<i64> = memory_ids.into_iter().filter(|id| valid_evidence.contains(id)).collect();
            valid_memory_ids.sort_unstable();
            valid_memory_ids.dedup();
            let cited_confidences: Vec<f64> =
                insight_ids.iter().filter_map(|id| by_id.get(id).map(|i| i.confidence)).collect();
            if valid_memory_ids.is_empty() || cited_confidences.len() < 2 {
                continue;
            }
            let confidence = cited_confidences.iter().cloned().fold(f64::INFINITY, f64::min);
            let mut source_ids: Vec<String> = insight_ids.iter().map(|id| format!("i{}", id)).collect();
            source_ids.extend(valid_memory_ids.iter().map(|id| id.to_string()));
            let text_embedding = embedder.embed(&text).ok();
            if store::insert_theme(conn, &text, confidence, &source_ids, text_embedding.as_deref()).is_ok() {
                themes_added += 1;
            }
        }
    }
    Ok((themes_added, llm_failed))
}

/// The nightly dedupe pass (runs inside `mach kb reflect`, after
/// dormancy): finds ACTIVE memory pairs at `>= reflect::DEDUPE_MIN_SIM`
/// cosine where at least one side was created since the last reflect run,
/// asks one haiku call per pair (capped at `reflect::DEDUPE_MAX_PAIRS_PER_RUN`,
/// oldest-first) for a DUPLICATE/SUPERSEDES/DISTINCT verdict, and applies
/// it:
///
/// - `Duplicate`/`Supersedes` merges the loser's earned reinforcement into
///   the winner and tombstones the loser via the ordinary supersession
///   mechanics, then re-points any insight citing the loser to the winner.
/// - `Distinct` is recorded in `dedupe_seen` so the pair is never re-asked.
/// - `Malformed` (or the judge call itself failing) does nothing — no
///   merge, no `dedupe_seen` entry — so the pair gets a fresh chance on a
///   later run instead of being silenced by a bad or missing reply.
///
/// Returns `(pairs_merged, any_judge_call_failed)`. Generic over
/// `ReflectLlm` so it's exercised in tests against a fake that can fail on
/// demand, without spawning a real `claude` process.
fn run_dedupe_pass<L: ReflectLlm>(
    conn: &Connection,
    llm: &L,
    new_ids: &HashSet<i64>,
    now: &str,
) -> Result<(usize, bool), KbError> {
    if new_ids.is_empty() {
        return Ok((0, false));
    }
    let pool = store::active_memories_for_dormancy(conn)?;
    let items: Vec<(i64, Vec<f32>)> = pool.iter().filter_map(|m| m.embedding.clone().map(|e| (m.id, e))).collect();
    let seen = store::dedupe_seen_pairs(conn)?;
    let pairs =
        reflect::dedupe_candidate_pairs(&items, new_ids, &seen, reflect::DEDUPE_MIN_SIM, reflect::DEDUPE_MAX_PAIRS_PER_RUN);

    let mut deduped = 0usize;
    let mut llm_failed = false;
    for (id_a, id_b) in pairs {
        let (mem_a, mem_b) = match (store::get(conn, id_a)?, store::get(conn, id_b)?) {
            (Some(a), Some(b)) => (a, b),
            _ => continue, // one side vanished between candidate generation and now
        };
        let prompt = reflect::build_dedupe_prompt(
            (mem_a.id, mem_a.content.as_str(), mem_a.source.as_deref()),
            (mem_b.id, mem_b.content.as_str(), mem_b.source.as_deref()),
        );
        let raw = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
            Ok(out) => out,
            Err(_) => {
                // The judge call itself failed — never destructive, and
                // deliberately NOT recorded in dedupe_seen either (unlike a
                // genuine Malformed reply, this pair was never actually
                // asked about): retry it next run.
                llm_failed = true;
                continue;
            }
        };
        match reflect::parse_dedupe_verdict(&raw, id_a, id_b) {
            reflect::DedupeVerdict::Duplicate { keep_id } | reflect::DedupeVerdict::Supersedes { winner_id: keep_id } => {
                let (winner_id, loser_id) = if keep_id == id_a { (id_a, id_b) } else { (id_b, id_a) };
                if store::merge_and_supersede(conn, loser_id, winner_id, now)? {
                    store::repoint_insight_citations(conn, loser_id, winner_id)?;
                    deduped += 1;
                }
                // Never recorded in dedupe_seen either way: a successful
                // merge drops the loser out of the active pool (it can't
                // resurface as a pair), and a no-op merge (loser already
                // gone) needs no record for the same reason.
            }
            reflect::DedupeVerdict::Distinct => {
                store::mark_dedupe_seen(conn, id_a, id_b)?;
            }
            reflect::DedupeVerdict::Malformed => {
                // Safe default, but deliberately not final — see the
                // Malformed variant's own doc comment.
            }
        }
    }
    Ok((deduped, llm_failed))
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
        let prefix = if ins.is_theme() { 't' } else { 'i' };
        println!(
            "{}{}{:<5} {}  (confidence {:.2}, sources: {})",
            flag,
            prefix,
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

// --- tree: render the theme -> insight -> memory hierarchy ---

fn print_insight_node(conn: &Connection, ins: &Insight, indent: &str) {
    let flag = if ins.is_flagged() { " [FLAGGED]" } else { "" };
    println!("{}i{}  (confidence {:.2}){}", indent, ins.id, ins.confidence, flag);
    let leaf_indent = format!("{}  ", indent);
    for sid in &ins.source_ids {
        if let Ok(mid) = sid.parse::<i64>() {
            if let Ok(Some(m)) = store::get(conn, mid) {
                println!("{}{:<6} {}", leaf_indent, mid, truncate(&m.content, 70));
            }
        }
    }
}

fn cmd_tree(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach kb tree");
                return Ok(());
            }
            other => {
                eprintln!("mach kb tree: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;

    let mut themes = store::active_insights_by_level(&conn, 2).map_err(to_io)?;
    themes.sort_by_key(|t| t.id);
    let mut level1 = store::active_insights_by_level(&conn, 1).map_err(to_io)?;
    level1.sort_by_key(|i| i.id);

    if themes.is_empty() && level1.is_empty() {
        println!("mach kb tree: no insights yet — run `mach kb reflect`");
        return Ok(());
    }

    let by_id: BTreeMap<i64, &Insight> = level1.iter().map(|i| (i.id, i)).collect();
    let themed = store::themed_insight_ids(&conn).map_err(to_io)?;

    // Every raw memory id cited by ANY active insight or theme — the
    // complement (over active memories) is the "uncategorized" trailing
    // count, never listed individually.
    let mut cited_memory_ids: HashSet<i64> = HashSet::new();
    for ins in level1.iter().chain(themes.iter()) {
        for sid in &ins.source_ids {
            if let Ok(mid) = sid.parse::<i64>() {
                cited_memory_ids.insert(mid);
            }
        }
    }

    for theme in &themes {
        let flag = if theme.is_flagged() { " [FLAGGED]" } else { "" };
        println!("{}  (confidence {:.2}){}", truncate(&theme.text, 90), theme.confidence, flag);
        for sid in &theme.source_ids {
            if let Some(rest) = sid.strip_prefix('i').or_else(|| sid.strip_prefix('I')) {
                if let Ok(iid) = rest.parse::<i64>() {
                    if let Some(ins) = by_id.get(&iid) {
                        print_insight_node(&conn, ins, "  ");
                    }
                }
            }
        }
        println!();
    }

    let unthemed: Vec<&Insight> = level1.iter().filter(|i| !themed.contains(&i.id)).collect();
    if !unthemed.is_empty() {
        println!("(unthemed)");
        for ins in &unthemed {
            print_insight_node(&conn, ins, "  ");
        }
        println!();
    }

    let active_memory_ids: HashSet<i64> = store::list(&conn, None, false).map_err(to_io)?.iter().map(|m| m.id).collect();
    let uncategorized = active_memory_ids.difference(&cited_memory_ids).count();
    if uncategorized > 0 {
        println!("... and {} uncategorized memories", uncategorized);
    }

    Ok(())
}

// --- model: compact always-on mental-model view for context injection ---

/// Renders one `mach kb model` line for a single row: kind and confidence
/// up front for quick scanning, doubted rows keep their line but gain a
/// trailing marker rather than being hidden — a doubted belief is still a
/// belief worth surfacing, just one to treat as a hypothesis.
fn format_model_row(row: &store::ModelRow) -> String {
    let kind = match row.kind {
        store::ModelKind::Theme => "theme",
        store::ModelKind::Belief => "belief",
    };
    let indent = if row.nested { "  " } else { "" };
    let doubt = if row.doubted { " [DOUBTED — evidence under review]" } else { "" };
    format!("{}- [{}, confidence {:.2}] {}{}", indent, kind, row.confidence, row.text, doubt)
}

#[derive(Serialize)]
struct ModelRowJson {
    kind: &'static str,
    confidence: f64,
    text: String,
    doubted: bool,
    nested: bool,
}

fn to_model_row_json(row: &store::ModelRow) -> ModelRowJson {
    ModelRowJson {
        kind: match row.kind {
            store::ModelKind::Theme => "theme",
            store::ModelKind::Belief => "belief",
        },
        confidence: row.confidence,
        text: row.text.clone(),
        doubted: row.doubted,
        nested: row.nested,
    }
}

fn cmd_model(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut json = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                println!("usage: mach kb model [--json]");
                println!(
                    "       compact mental-model view: all active (non-invalidated) insights and \
                     themes, tree-ordered — no source ids, no memory leaves. Empty store prints \
                     nothing. Meant for cheap always-on context injection (see \
                     ~/.config/claude-hooks/kb-model.sh); use `mach kb tree` or `mach kb insights` \
                     for the full picture with citations."
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb model: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let rows = store::mental_model(&conn).map_err(to_io)?;

    if json {
        let json_rows: Vec<ModelRowJson> = rows.iter().map(to_model_row_json).collect();
        println!("{}", serde_json::to_string(&json_rows)?);
    } else {
        for row in &rows {
            println!("{}", format_model_row(row));
        }
    }
    Ok(())
}

// --- export / import: full-fidelity JSONL backup ---

fn cmd_export(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut out_path: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out_path = args.next(),
            "-h" | "--help" => {
                println!("usage: mach kb export [--out FILE]");
                println!(
                    "       full-fidelity JSONL: every memories row (any state), then every \
                     insights row, then reflect_state — one JSON object per line, a header \
                     line first. Default: written to stdout (status goes to stderr, so stdout \
                     stays clean for piping/redirecting)."
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb export: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    match out_path {
        Some(path) => {
            let mut f = std::fs::File::create(&path)?;
            let counts = export::export_to_writer(&conn, &mut f).map_err(to_io)?;
            eprintln!("exported {} memories, {} insights -> {}", counts.memories, counts.insights, path);
        }
        None => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            let counts = export::export_to_writer(&conn, &mut lock).map_err(to_io)?;
            eprintln!("exported {} memories, {} insights", counts.memories, counts.insights);
        }
    }
    Ok(())
}

fn cmd_import(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut file: Option<String> = None;
    let mut merge = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--merge" => merge = true,
            "-h" | "--help" => {
                println!("usage: mach kb import FILE [--merge]");
                println!(
                    "       restores a `mach kb export` JSONL file. Without --merge, refuses \
                     outright if the database already has any memories or insights. With \
                     --merge, upserts each row by id: last-write-wins against the existing row \
                     of the same id, identical rows are skipped, embeddings are imported as-is."
                );
                return Ok(());
            }
            other => {
                if file.is_none() {
                    file = Some(other.to_string());
                } else {
                    eprintln!("mach kb import: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }
    let file = match file {
        Some(f) => f,
        None => {
            eprintln!("mach kb import: missing FILE argument");
            std::process::exit(1);
        }
    };

    let conn = store::open().map_err(to_io)?;
    let f = std::fs::File::open(&file)?;
    let reader = io::BufReader::new(f);
    match export::import_from_reader(&conn, reader, merge) {
        Ok(counts) => {
            println!(
                "imported: memories +{} ~{} ={} | insights +{} ~{} ={} | reflect_state {}",
                counts.memories_inserted,
                counts.memories_updated,
                counts.memories_skipped,
                counts.insights_inserted,
                counts.insights_updated,
                counts.insights_skipped,
                if counts.reflect_state_updated { "updated" } else { "unchanged" }
            );
            println!("(+ inserted, ~ updated, = skipped/unchanged)");
            Ok(())
        }
        Err(e) => {
            eprintln!("mach kb import: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: store::ModelKind, confidence: f64, text: &str, doubted: bool, nested: bool) -> store::ModelRow {
        store::ModelRow { kind, confidence, text: text.to_string(), doubted, nested }
    }

    #[test]
    fn format_model_row_renders_theme_and_belief_with_confidence() {
        let theme = row(store::ModelKind::Theme, 0.8, "a recurring pattern", false, false);
        assert_eq!(format_model_row(&theme), "- [theme, confidence 0.80] a recurring pattern");

        let belief = row(store::ModelKind::Belief, 0.55, "the user prefers X", false, false);
        assert_eq!(format_model_row(&belief), "- [belief, confidence 0.55] the user prefers X");
    }

    #[test]
    fn format_model_row_indents_nested_rows_one_level() {
        let nested = row(store::ModelKind::Belief, 0.6, "nested insight", false, true);
        assert_eq!(format_model_row(&nested), "  - [belief, confidence 0.60] nested insight");
    }

    #[test]
    fn format_model_row_appends_doubted_marker_but_keeps_the_line() {
        let doubted = row(store::ModelKind::Belief, 0.5, "a shaky belief", true, false);
        assert_eq!(
            format_model_row(&doubted),
            "- [belief, confidence 0.50] a shaky belief [DOUBTED — evidence under review]"
        );
    }

    // --- run_dedupe_pass: offline safety + apply mechanics ---

    use std::path::Path;

    fn mem_conn() -> Connection {
        store::open_with_path(Path::new(":memory:")).expect("open in-memory store")
    }

    fn unit_vec(dims: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        v[hot] = 1.0;
        v
    }

    fn insert_pair(conn: &Connection) -> (i64, i64) {
        // Two near-duplicate memories sharing an embedding direction, so
        // they always clear reflect::DEDUPE_MIN_SIM regardless of which
        // one the test names "new".
        let a = store::insert(conn, "the user drinks tea", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let b = store::insert(conn, "the user is fond of tea", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        (a, b)
    }

    /// A `ReflectLlm` test double that always returns a fixed reply, or
    /// always fails — never spawns a real process.
    struct FixedReflectLlm {
        reply: Result<&'static str, &'static str>,
    }

    impl ReflectLlm for FixedReflectLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            self.reply.map(|s| s.to_string()).map_err(|e| e.to_string())
        }
    }

    #[test]
    fn run_dedupe_pass_skips_entirely_when_nothing_is_new() {
        let conn = mem_conn();
        let (a, _b) = insert_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("DISTINCT") };
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &HashSet::new(), &store::now_rfc3339()).unwrap();
        assert_eq!(deduped, 0);
        assert!(!failed);
        let _ = a;
    }

    #[test]
    fn run_dedupe_pass_failed_judge_call_is_never_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        let llm = FixedReflectLlm { reply: Err("offline") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(deduped, 0, "never merges on a failed call");
        assert!(failed, "caller must freeze the watermark");
        assert!(store::dedupe_seen_pairs(&conn).unwrap().is_empty(), "a transport failure must not be recorded as seen");
        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded());
        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn run_dedupe_pass_malformed_reply_is_never_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("I'm not sure about this one.") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(deduped, 0);
        assert!(!failed, "the call itself succeeded — only its content was malformed");
        assert!(store::dedupe_seen_pairs(&conn).unwrap().is_empty(), "malformed -> retry next run, not silenced");
        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded());
        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn run_dedupe_pass_genuine_distinct_is_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("DISTINCT") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(deduped, 0);
        assert!(!failed);
        let seen = store::dedupe_seen_pairs(&conn).unwrap();
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(seen, [(lo, hi)].into_iter().collect());
    }

    /// A `ReflectLlm` test double carrying an owned reply — `FixedReflectLlm`
    /// above only holds a `&'static str`, too rigid for a reply built with
    /// `format!` from an id only known at test run time.
    struct OwnedReplyLlm {
        reply: String,
    }

    impl ReflectLlm for OwnedReplyLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn run_dedupe_pass_duplicate_merges_tombstones_and_repoints_citations() {
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        // b's earned reinforcement must survive the merge into a.
        let now = store::now_rfc3339();
        store::touch(&conn, &[b], &now).unwrap();
        store::touch(&conn, &[b], &now).unwrap();
        let insight_id = store::insert_insight(&conn, "an insight citing the loser", 0.5, &[b.to_string()], None).unwrap();

        let llm = OwnedReplyLlm { reply: format!("DUPLICATE {}", a) };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 1);
        assert!(!failed);

        let winner = store::get(&conn, a).unwrap().unwrap();
        let loser = store::get(&conn, b).unwrap().unwrap();
        assert!(loser.is_superseded());
        assert_eq!(loser.superseded_by, Some(a));
        assert_eq!(winner.access_count, 2, "winner absorbs the loser's earned reinforcement");

        let ins = store::get_insight(&conn, insight_id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec![a.to_string()], "citation of the tombstoned loser follows the merge to the winner");

        assert!(store::dedupe_seen_pairs(&conn).unwrap().is_empty(), "a merged pair needs no dedupe_seen record");
    }
}
