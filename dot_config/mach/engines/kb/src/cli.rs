
//! kb — subcommand dispatch for `mach kb ...`, matching the hand-rolled
//! arg-parsing style the sweep engine's `cli` module already uses (no
//! clap).
use std::collections::{BTreeMap, HashSet};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::classify::{self, Classifier, Verdict};
use crate::embed::{Embedder, OllamaEmbedder};
use crate::export;
use crate::ingest;
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
    println!("  search \"<query>\" [--limit N] [--json] [--reviewed-only] [--touch]");
    println!("      [--include-superseded] [--min-score F]");
    println!("                          ranked top-N search (sim/recency/strength blend);");
    println!("                          unreviewed rows are included by default at a small");
    println!("                          confidence penalty — --reviewed-only excludes them;");
    println!("                          --touch reinforces the rows actually returned");
    println!("  supersede <old_id> <new_id>");
    println!("                          tombstone <old_id> in favor of <new_id>");
    println!("  review                  interactive review of unreviewed candidates — optional");
    println!("                          human override; `mach kb reflect`'s curation pass");
    println!("                          already promotes/demotes this queue on its own");
    println!("  list [--limit N] [--superseded] [--dormant]");
    println!("                          most recent memories (--superseded/--dormant: audit views)");
    println!("  forget <id>             permanently delete a memory");
    println!("  wake <id>               clear a memory's dormant status (mach kb list --dormant)");
    println!("  reflect [--meta]        examine new memories, derive/reinforce durable");
    println!("                          insights, re-verify a sample of existing ones, curate");
    println!("                          the unreviewed queue (promote/leave/demote), put");
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
    println!("  ingest-sessions [--session-id ID]");
    println!("                          engagement-gated reinforcement: judges which memories");
    println!("                          injected into a finished session were actually engaged");
    println!("                          with (touch, don't just recall), and extracts durable");
    println!("                          facts from the same transcript (absorbs the old");
    println!("                          session-digest hook). No args: sweeps every session");
    println!("                          transcript idle >=10min and not yet processed. --session-id:");
    println!("                          processes exactly that session now (the SessionEnd trigger)");
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
        Some("ingest-sessions") => cmd_ingest_sessions(args),
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

// `pub` (fields included): shared with `socket::run` (the kb socket
// daemon serving `$XDG_RUNTIME_DIR/mach-kb.sock`) via `search_hits` below,
// so its response is byte-identical in shape to `mach kb search --json`'s
// stdout, and `kb-recall.sh`'s downstream JSON renderer needs no branching
// on which path served a given prompt.
#[derive(Serialize)]
pub struct SearchHit {
    pub id: i64,
    pub content: String,
    pub source: Option<String>,
    pub project: Option<String>,
    pub created_at: String,
    pub score: f32,
    pub sim: f32,
    pub recency: f32,
    pub strength: f32,
    pub importance: i64,
    pub superseded: bool,
    // Insight hits (from mach kb reflect) blended into recall: true marks a
    // row that came from the insights table rather than memories. Present
    // (and false) on memory hits too, so a consumer never has to treat its
    // absence as meaningful.
    pub derived: bool,
    // Only meaningful when `derived` is true.
    pub confidence: Option<f64>,
    // Only meaningful when `derived` is true: 1 = a plain insight, 2 = a
    // level-2 theme. `None` on plain memory hits — lets `kb-recall.sh`
    // distinguish "[derived belief]" from "[derived theme]".
    pub level: Option<i64>,
}

pub(crate) fn to_hit(h: RankedHit) -> SearchHit {
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

pub(crate) fn insight_to_hit(h: InsightHit) -> SearchHit {
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

/// Ranked top-N search + insight blend, embedding `query` itself — the same
/// merge `cmd_search` performs for `mach kb search --json` (mem hits and
/// insight hits fetched independently, combined, sorted by score, truncated
/// to `limit`, then `min_score`-filtered). Extracted as its own `pub`
/// function so the kb socket daemon (`socket::run`, serving
/// `$XDG_RUNTIME_DIR/mach-kb.sock` for `kb-recall.sh`'s fast path) can
/// produce the exact same `SearchHit` shape without going through a
/// subprocess.
///
/// Unlike `cmd_search`, there is no substring-fallback branch here: an
/// embed failure (ollama unreachable) is returned as a `KbError` rather
/// than degraded into a noisy substring match — the socket daemon treats
/// any error here as "bounce the caller back to the cold subprocess path",
/// which already has its own (separately noisy-guarded) fallback. `--touch`
/// reinforcement is also out of scope here — `kb-recall.sh` never sets it
/// (engagement-gated reinforcement now happens later, via `mach kb
/// ingest-sessions`), so neither call site needs it.
pub fn search_hits<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    query: &str,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<SearchHit>, KbError> {
    let q_emb = embedder.embed(query)?;
    let mem_hits = store::search_ranked(conn, &q_emb, limit, reviewed_only, include_superseded, 0.0, now)?;
    let insight_hits = store::search_insights_ranked(conn, &q_emb, limit, now)?;
    let mut combined: Vec<SearchHit> =
        mem_hits.into_iter().map(to_hit).chain(insight_hits.into_iter().map(insight_to_hit)).collect();
    combined.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    combined.truncate(limit);
    if min_score > 0.0 {
        combined.retain(|h| h.score >= min_score);
    }
    Ok(combined)
}

fn cmd_search(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut query: Option<String> = None;
    let mut limit: usize = 10;
    let mut json = false;
    let mut reviewed_only = false;
    let mut include_superseded = false;
    let mut touch = false;
    let mut min_score: f32 = 0.0;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--json" => json = true,
            "--reviewed-only" => reviewed_only = true,
            "--include-superseded" => include_superseded = true,
            "--touch" => touch = true,
            "--min-score" => min_score = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "-h" | "--help" => {
                println!(
                    "usage: mach kb search \"<query>\" [--limit N] [--json] [--reviewed-only] [--touch] \
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
                store::search_ranked(&conn, &q_emb, limit, reviewed_only, include_superseded, 0.0, &now).map_err(to_io)?;
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
            let subs = store::search_substring(&conn, &query, limit, reviewed_only, include_superseded).map_err(to_io)?;
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
// Strength review sampler (extends re-verification to raw memories, not
// just insights — see `run_strength_review_pass`).
const REFLECT_STRENGTH_SAMPLE: usize = 5;
const STRENGTH_MIN_IMPORTANCE: i64 = 6;
const STRENGTH_NEIGHBORS: usize = 3;

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

    // Step 4.5: nightly dedupe — fast capture paths (`mach note`, digests)
    // deliberately skip save-time dedupe classification, so near-duplicate
    // raw memories accumulate; this catches them here, where LLM time is
    // free. Only pairs touching a memory created since the last reflect
    // run are considered, capped at reflect::DEDUPE_MAX_PAIRS_PER_RUN.
    // Deliberately runs BEFORE dormancy (below): a memory old/stale/uncited
    // enough to qualify for dormancy this very run is exactly the kind of
    // long-neglected fact a dedupe or contradiction pair most needs to
    // catch — dormancy would otherwise drop it out of
    // `active_memories_for_dormancy`'s pool (dormant rows are excluded from
    // it) before either judge ever got a look at it.
    let new_ids: HashSet<i64> = new_memories.iter().map(|m| m.id).collect();
    let (deduped, dedupe_llm_failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).map_err(to_io)?;
    llm_failed = llm_failed || dedupe_llm_failed;

    // Step 4.55: contradiction patrol (after dedupe) — catches the case the
    // nightly dedupe pass's own similarity band never sees: two memories
    // about the same changing thing (e.g. a changed boss) that are similar
    // enough to be semantic siblings but too textually different to ever
    // land at dedupe's >= 0.85 floor, so the stale fact would otherwise
    // just sit there accumulating reinforcement forever. Only pairs
    // touching a memory created since the last reflect run are considered,
    // same "new-vs-all" shape as the dedupe pass, capped at
    // reflect::CONTRADICTION_MAX_PAIRS_PER_RUN. Same before-dormancy
    // rationale as the dedupe pass above.
    let (contradictions, contradiction_llm_failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).map_err(to_io)?;
    llm_failed = llm_failed || contradiction_llm_failed;

    // Step 4.57: curation — the organic promotion path for the unreviewed
    // queue (`mach kb add --unreviewed`/digests): judges ACTIVE unreviewed
    // rows past their 3-day settling period (reflect::CURATION_MIN_AGE_DAYS),
    // oldest-first, capped at reflect::CURATION_MAX_PER_RUN. `mach kb review`
    // remains an optional, immediate human override over the same rows —
    // this pass is what makes it optional rather than a required gate. Runs
    // every invocation regardless of has_new (like dormancy, below — it's a
    // sweep over the whole unreviewed queue, not gated on new material).
    // Deliberately before dormancy: not because same-run burial is otherwise
    // possible (a row just DEMOTEd here is at most a few days old, and
    // dormancy's own floor is store::DORMANCY_MIN_AGE_DAYS = 90 days, so it
    // can never qualify in the same run regardless of ordering) but to keep
    // this pass grouped with the other judge-driven sweeps above it.
    let (curated, promoted, demoted, curation_llm_failed) = run_curation_pass(&conn, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || curation_llm_failed;

    // Step 4.6: strength review — extends re-verification (step 4, above)
    // from insights to raw memories themselves: samples up to
    // REFLECT_STRENGTH_SAMPLE oldest-verified ACTIVE memories at importance
    // >= STRENGTH_MIN_IMPORTANCE and checks each against its own top-3
    // semantic neighbors, routing any genuine conflict through the same
    // judge the contradiction patrol above uses. Also runs before dormancy
    // for the same reason.
    let (mem_verified, mem_stale, mem_routed, strength_llm_failed) =
        run_strength_review_pass(&conn, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || strength_llm_failed;

    // Step 4.65: dormancy — put stale, low-importance, uncited memories to
    // sleep (reviewed and unreviewed alike, on the exact same criteria —
    // see store::memory_qualifies_for_dormancy's own doc comment; there is
    // no separate absolute clock for an unreviewed row anymore, the
    // curation pass above is what handles that queue now), then consolidate
    // this run's freshly-dormant rows into new durable facts where a
    // cluster of them shares something worth keeping. Runs
    // every invocation regardless of has_new/force_meta — it's a nightly
    // sweep over the whole active store, not gated on new material. Runs
    // last among these sweeps so a memory the contradiction patrol just
    // tombstoned above is already excluded from its pool (tombstoned rows
    // never qualify for dormancy in the first place) rather than racing it.
    let (dormant, consolidated, dormancy_llm_failed) = run_dormancy_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || dormancy_llm_failed;

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
         themes_added={} flagged={} verified={} curated={} promoted={} demoted={} dormant={} \
         consolidated={} deduped={} contradictions={} mem_verified={} mem_stale={} mem_routed={}{}",
        examined,
        questions_count,
        insights_added,
        reinforced,
        themes_added,
        flagged,
        verified,
        curated,
        promoted,
        demoted,
        dormant,
        consolidated,
        deduped,
        contradictions,
        mem_verified,
        mem_stale,
        mem_routed,
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
/// oldest-first) for a KEEP/DISTINCT verdict, and applies
/// it:
///
/// - `Keep` merges the loser's earned reinforcement into the winner and
///   tombstones the loser via the ordinary supersession mechanics, then
///   re-points any insight citing the loser to the winner.
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
            reflect::DedupeVerdict::Keep { keep_id } => {
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

/// The contradiction patrol (runs inside `mach kb reflect`, right after the
/// nightly dedupe pass): finds ACTIVE memory pairs at
/// `reflect::CONTRADICTION_MIN_SIM..reflect::CONTRADICTION_MAX_SIM` cosine —
/// semantic siblings sitting just below the dedupe band, textually
/// different enough that dedupe's own classifier never sees them as a pair
/// — where at least one side was created since the last reflect run, asks
/// one haiku call per pair (capped at `reflect::CONTRADICTION_MAX_PAIRS_PER_RUN`,
/// newest-first) for a CONFLICT/BOTH_HOLD/UNCLEAR verdict, and applies it
/// via `apply_contradiction_verdict`.
///
/// Returns `(contradictions_resolved, any_judge_call_failed)`. Generic over
/// `ReflectLlm`, same as `run_dedupe_pass`, so it's exercised in tests
/// against a fake that can fail on demand.
fn run_contradiction_pass<L: ReflectLlm>(
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
    let mut seen = store::dedupe_seen_pairs(conn)?;
    seen.extend(store::contradiction_seen_pairs(conn)?);
    let pairs = reflect::contradiction_candidate_pairs(
        &items,
        new_ids,
        &seen,
        reflect::CONTRADICTION_MIN_SIM,
        reflect::CONTRADICTION_MAX_SIM,
        reflect::CONTRADICTION_MAX_PAIRS_PER_RUN,
    );

    let mut resolved = 0usize;
    let mut llm_failed = false;
    for (id_a, id_b) in pairs {
        let (mem_a, mem_b) = match (store::get(conn, id_a)?, store::get(conn, id_b)?) {
            (Some(a), Some(b)) => (a, b),
            _ => continue, // one side vanished between candidate generation and now
        };
        if mem_a.is_superseded() || mem_b.is_superseded() {
            // Already resolved by an earlier pair examined this same run.
            continue;
        }
        let prompt = reflect::build_contradiction_pass_prompt(
            (mem_a.id, mem_a.content.as_str(), mem_a.created_at.as_str()),
            (mem_b.id, mem_b.content.as_str(), mem_b.created_at.as_str()),
        );
        let raw = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
            Ok(out) => out,
            Err(_) => {
                // The judge call itself failed — never destructive, and
                // deliberately NOT recorded in contradiction_seen either
                // (this pair was never actually asked about): retry it
                // next run.
                llm_failed = true;
                continue;
            }
        };
        if apply_contradiction_verdict(
            conn,
            reflect::parse_contradiction_verdict(&raw),
            id_a,
            &mem_a.created_at,
            id_b,
            &mem_b.created_at,
            now,
        )? {
            resolved += 1;
        }
    }
    Ok((resolved, llm_failed))
}

/// Applies one contradiction verdict — shared by `run_contradiction_pass`
/// and the strength-review sampler's own conflict routing (below), so both
/// apply the exact same tombstone/flag/seen mechanics:
///
/// - `Conflict` resolves the winner via `reflect::newer_wins` as-is (later
///   record wins — the model itself never names an id; see
///   `ContradictionVerdict`'s own doc comment) and tombstones the loser via
///   plain `store::supersede` (never `merge_and_supersede` — unlike a
///   dedupe merge, the loser was once genuinely true, so its earned access
///   stats stay exactly as they are, an honest history rather than folded
///   into the winner) and flags (never repoints) any insight citing the
///   loser — the insight may rest on the now-outdated fact, so it needs
///   human review, not a silent citation swap.
/// - `ConflictRetro` applies the exact same `supersede`/flag mechanics but
///   with `newer_wins`'s output consumed swapped: the *later*-recorded
///   memory is the retrospective one and loses, the *earlier*-recorded
///   memory is the one that's actually current and wins. Recording time
///   and event time aren't the same thing — see `ContradictionVerdict`'s
///   own doc comment.
/// - `BothHold` is recorded in `contradiction_seen` so the pair is never
///   re-asked.
/// - `Unclear` does nothing.
///
/// Returns whether a supersession was actually applied (for the caller's own
/// resolved-count bookkeeping).
#[allow(clippy::too_many_arguments)]
fn apply_contradiction_verdict(
    conn: &Connection,
    verdict: reflect::ContradictionVerdict,
    id_a: i64,
    date_a: &str,
    id_b: i64,
    date_b: &str,
    now: &str,
) -> Result<bool, KbError> {
    let apply_supersede = |winner_id: i64, loser_id: i64| -> Result<bool, KbError> {
        if store::supersede(conn, loser_id, winner_id, now)? {
            store::flag_insights_citing_memory(conn, loser_id, now)?;
            Ok(true)
        } else {
            Ok(false)
        }
    };
    match verdict {
        reflect::ContradictionVerdict::Conflict => {
            let (winner_id, loser_id) = reflect::newer_wins((id_a, date_a), (id_b, date_b));
            apply_supersede(winner_id, loser_id)
        }
        reflect::ContradictionVerdict::ConflictRetro => {
            // newer_wins returns (later, earlier); retro swaps who wins.
            let (later_id, earlier_id) = reflect::newer_wins((id_a, date_a), (id_b, date_b));
            apply_supersede(earlier_id, later_id)
        }
        reflect::ContradictionVerdict::BothHold => {
            store::mark_contradiction_seen(conn, id_a, id_b)?;
            Ok(false)
        }
        reflect::ContradictionVerdict::Unclear => Ok(false),
    }
}

/// The curation pass (runs inside `mach kb reflect`, right after the
/// contradiction patrol): judges the unreviewed queue itself so `mach kb
/// review` becomes optional curation rather than a required gate — see
/// `reflect`'s own "curation pass" section doc comment for the full
/// rationale. Candidates: `store::curation_candidates` (ACTIVE unreviewed
/// rows past `reflect::CURATION_MIN_AGE_DAYS`, oldest-first, capped at
/// `reflect::CURATION_MAX_PER_RUN`).
///
/// A row touched at least `reflect::CURATION_ENGAGEMENT_FAST_PATH` times
/// auto-promotes with no LLM call. Otherwise one haiku call per row, built
/// from the row's content/source/age/engagement plus its top-3 semantic
/// neighbors (via `store::top_similar_active`, self excluded) for a
/// coherence check:
///
/// - `Promote` sets `reviewed = 1` (`store::set_reviewed`) — this IS the
///   review; the search penalty for an unreviewed row goes away.
/// - `Leave` records nothing at all — the row simply re-enters the pool on
///   a later run once more evidence has accumulated. Never marked "seen":
///   unlike a dedupe/contradiction pair, there is no paired judge here to
///   avoid re-asking.
/// - `Demote` pins `importance` to `reflect::CURATION_DEMOTE_IMPORTANCE`
///   (`store::set_importance`) and halves `stability`
///   (`store::halve_stability`) — never deletes; ordinary dormancy criteria
///   (`store::DORMANCY_MIN_AGE_DAYS` = 90 days) finish the job later, so a
///   row demoted this run can never be swept as dormant in this same run
///   regardless of pass ordering.
/// - A failed judge call is treated exactly like `Leave` (nothing
///   recorded) but does set the caller's `llm_failed` flag so the
///   watermark doesn't advance on a degraded run.
///
/// Returns `(examined, promoted, demoted, any_judge_call_failed)`. Generic
/// over `ReflectLlm`, same as `run_dedupe_pass`/`run_contradiction_pass`,
/// so it's exercised in tests against a fake that can fail on demand.
fn run_curation_pass<L: ReflectLlm>(conn: &Connection, llm: &L, now: &str) -> Result<(usize, usize, usize, bool), KbError> {
    let candidates =
        store::curation_candidates(conn, now, reflect::CURATION_MIN_AGE_DAYS, reflect::CURATION_MAX_PER_RUN)?;
    let examined = candidates.len();
    let mut promoted = 0usize;
    let mut demoted = 0usize;
    let mut llm_failed = false;

    for m in candidates {
        if m.access_count >= reflect::CURATION_ENGAGEMENT_FAST_PATH {
            store::set_reviewed(conn, m.id, true)?;
            promoted += 1;
            continue;
        }

        let neighbors: Vec<(i64, String)> = match &m.embedding {
            Some(emb) => store::top_similar_active(conn, emb, 4)
                .unwrap_or_default()
                .into_iter()
                .filter(|(nm, _)| nm.id != m.id)
                .take(3)
                .map(|(nm, _)| (nm.id, nm.content))
                .collect(),
            None => Vec::new(),
        };
        let age = store::age_days(&m.created_at, now);
        let prompt = reflect::build_curation_prompt(&m.content, m.source.as_deref(), age, m.access_count, &neighbors);

        let raw = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
            Ok(out) => out,
            Err(_) => {
                // The judge call itself failed — Leave semantics (nothing
                // recorded, retried next run) but the caller must still
                // freeze the watermark.
                llm_failed = true;
                continue;
            }
        };
        match reflect::parse_curation_verdict(&raw) {
            reflect::CurationVerdict::Promote => {
                store::set_reviewed(conn, m.id, true)?;
                promoted += 1;
            }
            reflect::CurationVerdict::Demote => {
                store::set_importance(conn, m.id, reflect::CURATION_DEMOTE_IMPORTANCE)?;
                store::halve_stability(conn, m.id)?;
                demoted += 1;
            }
            reflect::CurationVerdict::Leave => {}
        }
    }
    Ok((examined, promoted, demoted, llm_failed))
}

/// The strength-review sampler (runs inside `mach kb reflect`, right after
/// the contradiction patrol): extends the same re-verification idea from
/// insights (step 4, above) to raw memories. Samples up to
/// `REFLECT_STRENGTH_SAMPLE` ACTIVE memories at `importance >=
/// STRENGTH_MIN_IMPORTANCE`, oldest `last_verified_at` first (mirrors
/// `insights_due_for_verification`'s own ordering), and checks each against
/// its own top-`STRENGTH_NEIGHBORS` semantic neighbors with one haiku call:
///
/// - `Stands` bumps `last_verified_at` — reviewed, still holds.
/// - `Stale` halves `stability` (so it decays and gets recalled/reinforced
///   less, without being tombstoned — actual supersession stays the
///   contradiction patrol's job) and also bumps `last_verified_at`, so this
///   memory doesn't monopolize every future run's sample.
/// - `Conflict` routes the specific `(memory, neighbor)` pair through the
///   exact same judge prompt/parse/apply path the contradiction patrol
///   itself uses (skipping it if the pair is already in `dedupe_seen` or
///   `contradiction_seen` — no need to ask twice). This sampler's own call
///   already succeeded (it got a verdict), so `last_verified_at` is bumped
///   regardless of how that follow-up call turns out; a failure in the
///   follow-up call still marks the run degraded.
/// - `Malformed` (or the sampler's own judge call failing) does nothing —
///   the memory stays due and gets resampled on a later run.
///
/// Returns `(stands, stale, routed_to_contradiction, any_judge_call_failed)`.
fn run_strength_review_pass<L: ReflectLlm>(conn: &Connection, llm: &L, now: &str) -> Result<(usize, usize, usize, bool), KbError> {
    let candidates = store::memories_due_for_strength_review(conn, REFLECT_STRENGTH_SAMPLE, STRENGTH_MIN_IMPORTANCE)?;
    let mut stands = 0usize;
    let mut stale = 0usize;
    let mut routed = 0usize;
    let mut llm_failed = false;

    for stale_mem in &candidates {
        // Re-fetch rather than trust the up-front snapshot: an earlier
        // iteration in this same loop may already have superseded this
        // exact memory (as the loser of a routed conflict against a
        // different sampled row).
        let mem = match store::get(conn, stale_mem.id)? {
            Some(m) if !m.is_superseded() => m,
            _ => continue,
        };
        let emb = match &mem.embedding {
            Some(e) if !e.is_empty() => e.clone(),
            _ => continue,
        };
        let emb = emb.as_slice();
        let neighbor_hits = store::top_similar_active(conn, emb, STRENGTH_NEIGHBORS + 1)?;
        let neighbor_pairs: Vec<(i64, String)> = neighbor_hits
            .into_iter()
            .filter(|(m, _)| m.id != mem.id)
            .take(STRENGTH_NEIGHBORS)
            .map(|(m, _)| (m.id, m.content))
            .collect();
        if neighbor_pairs.is_empty() {
            continue; // nothing to compare against yet
        }

        let prompt = reflect::build_strength_review_prompt(&mem.content, &mem.created_at, &neighbor_pairs);
        let raw = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue;
            }
        };
        let neighbor_ids: Vec<i64> = neighbor_pairs.iter().map(|(id, _)| *id).collect();
        match reflect::parse_strength_verdict(&raw, &neighbor_ids) {
            reflect::StrengthVerdict::Stands => {
                store::mark_memory_verified(conn, mem.id, now)?;
                stands += 1;
            }
            reflect::StrengthVerdict::Stale => {
                store::halve_stability(conn, mem.id)?;
                store::mark_memory_verified(conn, mem.id, now)?;
                stale += 1;
            }
            reflect::StrengthVerdict::Conflict { with_id } => {
                store::mark_memory_verified(conn, mem.id, now)?;
                routed += 1;

                let (id_a, id_b) = if mem.id < with_id { (mem.id, with_id) } else { (with_id, mem.id) };
                let mut seen = store::dedupe_seen_pairs(conn)?;
                seen.extend(store::contradiction_seen_pairs(conn)?);
                if seen.contains(&(id_a, id_b)) {
                    continue; // already ruled on by either judge — don't re-ask
                }
                let (neighbor_mem, this_mem) = match store::get(conn, with_id)? {
                    Some(n) if !n.is_superseded() => (n, mem.clone()),
                    _ => continue,
                };
                let prompt = reflect::build_contradiction_pass_prompt(
                    (this_mem.id, this_mem.content.as_str(), this_mem.created_at.as_str()),
                    (neighbor_mem.id, neighbor_mem.content.as_str(), neighbor_mem.created_at.as_str()),
                );
                match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
                    Ok(craw) => {
                        apply_contradiction_verdict(
                            conn,
                            reflect::parse_contradiction_verdict(&craw),
                            this_mem.id,
                            &this_mem.created_at,
                            neighbor_mem.id,
                            &neighbor_mem.created_at,
                            now,
                        )?;
                    }
                    Err(_) => {
                        llm_failed = true;
                    }
                }
            }
            reflect::StrengthVerdict::Malformed => {}
        }
    }
    Ok((stands, stale, routed, llm_failed))
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

// --- ingest-sessions: engagement-gated reinforcement ---

/// A trivial session (nothing usable came out of transcript filtering, and
/// nothing was ever injected into it) never needs a `claude` call at all —
/// this is that raw JSONL-line floor below which even the fact-digest gate
/// (`ingest::DIGEST_MIN_TRANSCRIPT_LINES`) wouldn't fire anyway, kept
/// separate so the "nothing to do" short-circuit doesn't depend on the
/// digest gate's own threshold changing later.
const INGEST_TRIVIAL_LINE_FLOOR: usize = 1;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct IngestSummary {
    scanned: usize,
    processed: usize,
    engaged: usize,
    shown: usize,
    facts_added: usize,
    deferred_offline: usize,
}

/// Every `.jsonl` transcript under `~/.claude/projects/*/*.jsonl` —
/// `projects_root` is that `projects` directory itself. Tolerant of a
/// missing root (a fresh machine with no Claude Code sessions yet) and of
/// individual unreadable entries, rather than erroring the whole sweep.
fn list_transcript_files(projects_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(&path) else {
            continue;
        };
        for f in inner.flatten() {
            let p = f.path();
            if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(p);
            }
        }
    }
    out
}

/// Locates the one transcript file for `session_id`, regardless of its
/// mtime — used by the `--session-id` fast-trigger path, which already
/// knows this exact session just ended and must not wait for the
/// opportunistic sweep's staleness gate.
fn find_transcript_by_session(projects_root: &Path, session_id: &str) -> Option<PathBuf> {
    list_transcript_files(projects_root).into_iter().find(|p| ingest::session_id_from_path(p).as_deref() == Some(session_id))
}

/// The engagement-gated reinforcement + fact-digest pass over one or more
/// finished sessions. Generic over `Embedder`/`ReflectLlm`/
/// `ingest::TranscriptFilter` so it's exercised in tests against fakes,
/// without spawning a real `ollama`/`claude`/`python3` process — mirrors
/// `run_dedupe_pass`'s own genericity. `only_session` selects the
/// `--session-id` fast-trigger path (bypasses the staleness gate, still
/// respects the processed-set and offline discipline below); `None` is the
/// opportunistic sweep (every transcript idle >= `ingest::SWEEP_MIN_IDLE_SECS`
/// and not yet processed).
///
/// Offline discipline: a session that needs a `claude` call this run (an
/// engagement verdict, a fact digest, or both) is marked ingested ONLY if
/// every such call it needed actually succeeded this run — a failure
/// (offline, spawn error, timeout) leaves it entirely unprocessed, so it
/// gets a fresh chance on a later run instead of silently losing its
/// judgment. A session needing no `claude` call at all (no ids were ever
/// injected into it, and its transcript is too trivial to digest) is always
/// safe to mark processed immediately, regardless of connectivity.
#[allow(clippy::too_many_arguments)]
fn run_ingest_sessions<E: Embedder, L: ReflectLlm, F: ingest::TranscriptFilter>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    filter: &F,
    projects_root: &Path,
    recall_log_root: &Path,
    only_session: Option<&str>,
    now: &str,
) -> Result<IngestSummary, KbError> {
    let mut summary = IngestSummary::default();

    let candidates: Vec<PathBuf> = match only_session {
        Some(sid) => find_transcript_by_session(projects_root, sid).into_iter().collect(),
        None => list_transcript_files(projects_root)
            .into_iter()
            .filter(|p| {
                let age = std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|mtime| mtime.elapsed().ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                ingest::is_stale_enough(age)
            })
            .collect(),
    };

    for path in candidates {
        summary.scanned += 1;
        let Some(session_id) = ingest::session_id_from_path(&path) else {
            continue;
        };
        if store::is_session_ingested(conn, &session_id)? {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue; // unreadable this run -- stays unprocessed, retried later
        };

        let recall_log_path = recall_log_root.join(format!("{}.jsonl", session_id));
        let injected_ids: Vec<i64> =
            std::fs::read_to_string(&recall_log_path).map(|c| ingest::parse_recall_log(&c)).unwrap_or_default();

        let raw_line_count = raw.lines().count();
        if raw_line_count < INGEST_TRIVIAL_LINE_FLOOR && injected_ids.is_empty() {
            store::mark_session_ingested(conn, &session_id, now)?;
            summary.processed += 1;
            continue;
        }

        let dialogue = filter.filter(&raw);
        let dialogue_text = match dialogue.as_deref() {
            Some(d) if !d.trim().is_empty() => d,
            _ => {
                // No usable dialogue -- can't honestly judge engagement or
                // extract anything; every injected id defaults to shown
                // (fails closed, same as an unparseable verdict reply), and
                // no `claude` call was needed, so this is always safe to
                // mark processed.
                summary.shown += injected_ids.len();
                store::mark_session_ingested(conn, &session_id, now)?;
                summary.processed += 1;
                continue;
            }
        };

        let mut needs_retry = false;

        // --- engagement verdicts ---
        if !injected_ids.is_empty() {
            let mut memories: Vec<(i64, String)> = Vec::new();
            for id in &injected_ids {
                if let Some(m) = store::get(conn, *id)? {
                    memories.push((*id, m.content));
                }
            }
            if !memories.is_empty() {
                let prompt = ingest::build_engagement_prompt(dialogue_text, &memories);
                match llm.call("haiku", &prompt, ingest::TIMEOUT_INGEST) {
                    Ok(out) => {
                        let known_ids: Vec<i64> = memories.iter().map(|(id, _)| *id).collect();
                        let verdicts = ingest::parse_engagement_verdicts(&out, &known_ids);
                        let engaged_ids: Vec<i64> = verdicts
                            .iter()
                            .filter(|(_, v)| **v == ingest::EngagementVerdict::Engaged)
                            .map(|(id, _)| *id)
                            .collect();
                        if !engaged_ids.is_empty() {
                            store::touch(conn, &engaged_ids, now)?;
                        }
                        summary.engaged += engaged_ids.len();
                        summary.shown += verdicts.len() - engaged_ids.len();
                    }
                    Err(_) => needs_retry = true,
                }
            }
            // every injected id has since been forgotten -- nothing left to
            // judge or touch, and no call was needed for it.
        }

        // --- fact digest (absorbed kb-capture.sh) ---
        if !needs_retry && raw_line_count >= ingest::DIGEST_MIN_TRANSCRIPT_LINES {
            let digest_prompt = ingest::build_digest_prompt(dialogue_text);
            match llm.call("haiku", &digest_prompt, ingest::TIMEOUT_INGEST) {
                Ok(out) => {
                    for fact in ingest::parse_digest_facts(&out) {
                        let embedding = embedder.embed(&fact).ok();
                        if store::insert(conn, &fact, Some("session-digest"), None, false, embedding.as_deref(), 5)
                            .is_ok()
                        {
                            summary.facts_added += 1;
                        }
                    }
                }
                Err(_) => needs_retry = true,
            }
        }

        if needs_retry {
            summary.deferred_offline += 1;
            continue; // leave unprocessed -- retried on a later run
        }

        store::mark_session_ingested(conn, &session_id, now)?;
        summary.processed += 1;
    }

    Ok(summary)
}

fn cmd_ingest_sessions(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut only_session: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--session-id" => only_session = args.next(),
            "-h" | "--help" => {
                println!("usage: mach kb ingest-sessions [--session-id ID]");
                println!(
                    "       engagement-gated reinforcement: for each finished session, judges \
                     which memories injected into it (via kb-recall.sh's recall log) were \
                     actually engaged with -- reinforcing only those -- and extracts durable \
                     facts from the same transcript (absorbs the old kb-capture.sh digest)."
                );
                println!(
                    "       no args: sweeps every transcript under ~/.claude/projects idle >= \
                     10min and not yet processed (the crash-safe timer path)."
                );
                println!(
                    "       --session-id ID: processes exactly that session now, regardless of \
                     idle time (the SessionEnd fast trigger)."
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb ingest-sessions: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => {
            eprintln!("mach kb ingest-sessions: HOME is not set");
            std::process::exit(1);
        }
    };
    let projects_root = Path::new(&home).join(".claude/projects");
    let recall_log_root = Path::new(&home).join(".local/share/mach/recall-log");

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    let filter = ingest::ProcessTranscriptFilter::new();
    let now = store::now_rfc3339();

    let summary =
        run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, only_session.as_deref(), &now)
            .map_err(to_io)?;

    println!(
        "mach kb ingest-sessions: scanned={} processed={} engaged={} shown={} facts_added={} deferred_offline={}",
        summary.scanned, summary.processed, summary.engaged, summary.shown, summary.facts_added, summary.deferred_offline
    );
    Ok(())
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

    use rusqlite::params;
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
    fn run_dedupe_pass_keep_merges_tombstones_and_repoints_citations() {
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        // b's earned reinforcement must survive the merge into a.
        let now = store::now_rfc3339();
        store::touch(&conn, &[b], &now).unwrap();
        store::touch(&conn, &[b], &now).unwrap();
        let insight_id = store::insert_insight(&conn, "an insight citing the loser", 0.5, &[b.to_string()], None).unwrap();

        let llm = OwnedReplyLlm { reply: format!("KEEP {}", a) };
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

    // --- run_contradiction_pass: band selection, offline safety, apply mechanics ---

    /// Two memories at ~0.707 cosine (a 45-degree angle) — squarely inside
    /// the contradiction band (0.60-0.85) and well below the dedupe band
    /// (>= 0.85), unlike `insert_pair`'s identical-direction vectors.
    fn insert_contradiction_pair(conn: &Connection) -> (i64, i64) {
        let a = store::insert(conn, "TESTBOSS: Moses is the user's boss", None, None, true, Some(&[1.0f32, 0.0]), 5).unwrap();
        let b = store::insert(
            conn,
            "TESTBOSS: the user's boss Ivar approved the RHI redesign",
            None,
            None,
            true,
            Some(&[1.0f32, 1.0]),
            5,
        )
        .unwrap();
        (a, b)
    }

    #[test]
    fn run_contradiction_pass_skips_entirely_when_nothing_is_new() {
        let conn = mem_conn();
        let (a, _b) = insert_contradiction_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("BOTH_HOLD") };
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &HashSet::new(), &store::now_rfc3339()).unwrap();
        assert_eq!(resolved, 0);
        assert!(!failed);
        let _ = a;
    }

    #[test]
    fn run_contradiction_pass_failed_judge_call_is_never_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn);
        let llm = FixedReflectLlm { reply: Err("offline") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(resolved, 0, "never tombstones on a failed call");
        assert!(failed, "caller must freeze the watermark");
        assert!(store::contradiction_seen_pairs(&conn).unwrap().is_empty(), "a transport failure must not be recorded as seen");
        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded());
        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn run_contradiction_pass_unclear_reply_is_never_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("UNCLEAR") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(resolved, 0);
        assert!(!failed, "the call itself succeeded — only its verdict was UNCLEAR");
        assert!(store::contradiction_seen_pairs(&conn).unwrap().is_empty(), "UNCLEAR -> retry next run, not silenced");
        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded());
        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn run_contradiction_pass_both_hold_is_recorded_as_seen() {
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("BOTH_HOLD") };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(resolved, 0);
        assert!(!failed);
        let seen = store::contradiction_seen_pairs(&conn).unwrap();
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(seen, [(lo, hi)].into_iter().collect());
    }

    #[test]
    fn run_contradiction_pass_conflict_tombstones_the_older_without_absorbing_loser_stats() {
        // Unlike a dedupe merge, a contradiction loser was once genuinely
        // true — its earned access stats must survive untouched as history,
        // not be folded into the winner (`merge_and_supersede`'s job, never
        // called here). The verdict itself carries no id — `newer_wins`
        // (keyed on `created_at`, not anything the model says) picks b
        // (Ivar, inserted after a/Moses) as the winner.
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn); // a = Moses (old), b = Ivar (new)
        let now = store::now_rfc3339();
        store::touch(&conn, &[a], &now).unwrap();
        store::touch(&conn, &[a], &now).unwrap();
        let winner_stability_before = store::get(&conn, b).unwrap().unwrap().effective_stability();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 1);
        assert!(!failed);

        let loser = store::get(&conn, a).unwrap().unwrap();
        let winner = store::get(&conn, b).unwrap().unwrap();
        assert!(loser.is_superseded());
        assert_eq!(loser.superseded_by, Some(b));
        assert_eq!(loser.access_count, 2, "the loser's earned reinforcement stays on the loser's own row, as history");
        assert_eq!(
            winner.effective_stability(),
            winner_stability_before,
            "the winner's own stability must be untouched — no merge happens here"
        );
        assert!(
            store::contradiction_seen_pairs(&conn).unwrap().is_empty(),
            "a resolved pair needs no contradiction_seen record"
        );
    }

    #[test]
    fn run_contradiction_pass_conflict_flags_but_never_repoints_citing_insights() {
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn); // a = Moses (loser), b = Ivar (winner)
        let now = store::now_rfc3339();
        let insight_id = store::insert_insight(&conn, "the user reports to Moses", 0.5, &[a.to_string()], None).unwrap();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, _failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 1);

        let ins = store::get_insight(&conn, insight_id).unwrap().unwrap();
        assert!(ins.is_flagged(), "an insight citing the now-superseded loser must be flagged for review");
        assert_eq!(ins.source_ids, vec![a.to_string()], "the citation itself must NOT be repointed to the winner");
    }

    #[test]
    fn run_contradiction_pass_conflict_retro_tombstones_the_newer_recorded_retrospective_note() {
        // b is recorded after a, but CONFLICT_RETRO says it's b describing a
        // PAST state retrospectively -- the swap from a plain Conflict must
        // hold: a (older-recorded, still current) stays active, b
        // (newer-recorded, retrospective) is the one tombstoned, even though
        // it's the later row (newer_wins would pick b if this were a plain
        // Conflict).
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn); // a = older-recorded, b = newer-recorded retrospective note
        let now = store::now_rfc3339();
        let insight_id =
            store::insert_insight(&conn, "an insight citing the retrospective note", 0.5, &[b.to_string()], None).unwrap();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT_RETRO") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 1);
        assert!(!failed);

        let older = store::get(&conn, a).unwrap().unwrap();
        let newer = store::get(&conn, b).unwrap().unwrap();
        assert!(!older.is_superseded(), "the older-recorded, still-current fact must stay active");
        assert!(newer.is_superseded(), "the newer-recorded retrospective note is the one tombstoned");
        assert_eq!(newer.superseded_by, Some(a));

        let ins = store::get_insight(&conn, insight_id).unwrap().unwrap();
        assert!(ins.is_flagged(), "an insight citing the now-tombstoned retrospective note must be flagged");
    }

    // --- run_curation_pass: candidate selection, apply mechanics, offline safety ---

    /// Inserts an unreviewed row and backdates it past the curation pass's
    /// settling period so it actually shows up as a candidate.
    fn insert_settled_unreviewed(conn: &Connection, content: &str, importance: i64) -> i64 {
        let id = store::insert(conn, content, Some("session-digest"), None, false, None, importance).unwrap();
        let old_ts = store::now_rfc3339_from_secs(store::now_secs() - 10 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![old_ts, id]).unwrap();
        id
    }

    #[test]
    fn run_curation_pass_ignores_a_row_still_inside_its_settling_period() {
        let conn = mem_conn();
        // Not backdated -- created "now", well inside the 3-day floor.
        let id = store::insert(&conn, "too fresh to judge", None, None, false, None, 5).unwrap();
        let llm = FixedReflectLlm { reply: Ok("PROMOTE") };
        let (examined, promoted, demoted, failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!((examined, promoted, demoted), (0, 0, 0));
        assert!(!failed);
        assert!(!store::get(&conn, id).unwrap().unwrap().reviewed);
    }

    #[test]
    fn run_curation_pass_engagement_fast_path_promotes_without_any_llm_call() {
        let conn = mem_conn();
        let id = insert_settled_unreviewed(&conn, "used twice already", 5);
        let now = store::now_rfc3339();
        store::touch(&conn, &[id], &now).unwrap();
        store::touch(&conn, &[id], &now).unwrap();
        assert_eq!(store::get(&conn, id).unwrap().unwrap().access_count, 2);

        // A call here would itself be a test failure via llm_failed, since
        // the fast path must never invoke the judge at all.
        let llm = FixedReflectLlm { reply: Err("must never be called") };
        let (examined, promoted, demoted, failed) = run_curation_pass(&conn, &llm, &now).unwrap();
        assert_eq!((examined, promoted, demoted), (1, 1, 0));
        assert!(!failed, "the engagement fast path must never touch the LLM");
        assert!(store::get(&conn, id).unwrap().unwrap().reviewed);
    }

    #[test]
    fn run_curation_pass_promote_verdict_sets_reviewed() {
        let conn = mem_conn();
        let id = insert_settled_unreviewed(&conn, "durable fact", 5);
        let llm = FixedReflectLlm { reply: Ok("PROMOTE") };
        let (examined, promoted, demoted, failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!((examined, promoted, demoted), (1, 1, 0));
        assert!(!failed);
        assert!(store::get(&conn, id).unwrap().unwrap().reviewed);
    }

    #[test]
    fn run_curation_pass_demote_verdict_pins_importance_and_halves_stability_never_deletes() {
        let conn = mem_conn();
        let id = insert_settled_unreviewed(&conn, "transient noise", 5); // stability starts at 5*7=35.0
        let llm = FixedReflectLlm { reply: Ok("DEMOTE") };
        let (examined, promoted, demoted, failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!((examined, promoted, demoted), (1, 0, 1));
        assert!(!failed);
        let m = store::get(&conn, id).unwrap().unwrap();
        assert_eq!(m.importance, reflect::CURATION_DEMOTE_IMPORTANCE);
        assert_eq!(m.stability, Some(17.5), "stability must be halved, not reset");
        assert!(!m.reviewed, "a DEMOTE must never itself count as review");
    }

    #[test]
    fn run_curation_pass_leave_verdict_and_failed_call_both_change_nothing_and_are_not_recorded() {
        let conn = mem_conn();
        let leave_id = insert_settled_unreviewed(&conn, "can't tell yet", 5);
        let fail_id = insert_settled_unreviewed(&conn, "offline this run", 5);

        let leave_llm = FixedReflectLlm { reply: Ok("LEAVE") };
        let (examined, promoted, demoted, failed) =
            run_curation_pass(&conn, &leave_llm, &store::now_rfc3339()).unwrap();
        assert_eq!((examined, promoted, demoted), (2, 0, 0));
        assert!(!failed, "a genuine LEAVE reply is not itself a failure");

        let m = store::get(&conn, leave_id).unwrap().unwrap();
        assert!(!m.reviewed && m.importance == 5, "LEAVE must change nothing");
        let _ = fail_id;

        let fail_llm = FixedReflectLlm { reply: Err("offline") };
        let (examined2, promoted2, demoted2, failed2) =
            run_curation_pass(&conn, &fail_llm, &store::now_rfc3339()).unwrap();
        assert_eq!((examined2, promoted2, demoted2), (2, 0, 0));
        assert!(failed2, "a failed judge call must freeze the watermark");

        // Both rows must still be candidates on a later run -- nothing was
        // recorded as "seen" for either the LEAVE or the failed call.
        let still_pending = store::curation_candidates(&conn, &store::now_rfc3339(), reflect::CURATION_MIN_AGE_DAYS, 12).unwrap();
        let ids: std::collections::HashSet<i64> = still_pending.iter().map(|m| m.id).collect();
        assert!(ids.contains(&leave_id));
        assert!(ids.contains(&fail_id));
    }

    #[test]
    fn run_curation_pass_respects_the_per_run_cap() {
        let conn = mem_conn();
        for i in 0..(reflect::CURATION_MAX_PER_RUN + 3) {
            insert_settled_unreviewed(&conn, &format!("candidate {}", i), 5);
        }
        let llm = FixedReflectLlm { reply: Ok("LEAVE") };
        let (examined, _promoted, _demoted, _failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, reflect::CURATION_MAX_PER_RUN);
    }

    #[test]
    fn a_row_demoted_this_run_can_never_be_swept_dormant_in_the_same_run() {
        // Direct proof of the pass-ordering invariant: curation's own
        // settling floor (3 days) is far below dormancy's own age floor
        // (store::DORMANCY_MIN_AGE_DAYS, 90 days), so a row young enough to
        // still be a curation candidate can never simultaneously qualify
        // for dormancy, regardless of which pass runs first this run.
        let conn = mem_conn();
        let id = insert_settled_unreviewed(&conn, "freshly demoted", 5);
        let now = store::now_rfc3339();
        let llm = FixedReflectLlm { reply: Ok("DEMOTE") };
        let (_examined, _promoted, demoted, _failed) = run_curation_pass(&conn, &llm, &now).unwrap();
        assert_eq!(demoted, 1);

        let m = store::get(&conn, id).unwrap().unwrap();
        assert!(
            !store::memory_qualifies_for_dormancy(&m, false, &now),
            "a ~10-day-old row must never qualify for dormancy regardless of its freshly-lowered importance"
        );
    }

    // --- run_strength_review_pass: verdict application ---

    #[test]
    fn run_strength_review_pass_stands_bumps_last_verified_at() {
        let conn = mem_conn();
        let id = store::insert(&conn, "the user prefers dark mode", None, None, true, Some(&[1.0f32, 0.0]), 7).unwrap();
        store::insert(&conn, "a related neighbor memory", None, None, true, Some(&[1.0f32, 0.1]), STRENGTH_MIN_IMPORTANCE - 1).unwrap();

        let llm = FixedReflectLlm { reply: Ok("STANDS") };
        let (stands, stale, routed, failed) = run_strength_review_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(stands, 1);
        assert_eq!(stale, 0);
        assert_eq!(routed, 0);
        assert!(!failed);
        assert!(store::get(&conn, id).unwrap().unwrap().last_verified_at.is_some());
    }

    #[test]
    fn run_strength_review_pass_stale_halves_stability_and_bumps_verified() {
        let conn = mem_conn();
        let id = store::insert(&conn, "the user prefers dark mode", None, None, true, Some(&[1.0f32, 0.0]), 7).unwrap();
        store::insert(&conn, "a related neighbor memory", None, None, true, Some(&[1.0f32, 0.1]), STRENGTH_MIN_IMPORTANCE - 1).unwrap();
        let before = store::get(&conn, id).unwrap().unwrap().effective_stability();

        let llm = FixedReflectLlm { reply: Ok("STALE") };
        let (stands, stale, _routed, failed) = run_strength_review_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(stands, 0);
        assert_eq!(stale, 1);
        assert!(!failed);
        let after = store::get(&conn, id).unwrap().unwrap();
        assert!((after.effective_stability() - before / 2.0).abs() < 1e-9);
        assert!(after.last_verified_at.is_some(), "a STALE check still counts as a review — must not monopolize future runs");
        assert!(!after.is_superseded(), "STALE never tombstones — that stays the contradiction patrol's job");
    }

    #[test]
    fn run_strength_review_pass_conflict_routes_through_the_contradiction_judge() {
        let conn = mem_conn();
        let id = store::insert(&conn, "TESTBOSS: Moses is the user's boss", None, None, true, Some(&[1.0f32, 0.0]), 7).unwrap();
        let neighbor =
            store::insert(&conn, "TESTBOSS: the user's boss Ivar approved the RHI redesign", None, None, true, Some(&[1.0f32, 0.1]), 7)
                .unwrap();

        // First call (the sampler's own STANDS/STALE/CONFLICT check) returns
        // `CONFLICT <id>` naming the neighbor; the follow-up contradiction
        // judge uses a different, id-free `CONFLICT`/`BOTH_HOLD`/`UNCLEAR`
        // grammar (see `reflect::ContradictionVerdict`), so a routing double
        // keyed on prompt content still gives each call the reply shape it
        // actually expects.
        struct RoutingLlm {
            neighbor: i64,
        }
        impl ReflectLlm for RoutingLlm {
            fn call(&self, _model: &str, prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                if prompt.contains("periodic strength check") {
                    Ok(format!("CONFLICT {}", self.neighbor))
                } else {
                    Ok("CONFLICT".to_string())
                }
            }
        }
        let llm = RoutingLlm { neighbor };

        let now = store::now_rfc3339();
        let (stands, stale, routed, failed) = run_strength_review_pass(&conn, &llm, &now).unwrap();
        assert_eq!(stands, 0);
        assert_eq!(stale, 0);
        assert_eq!(routed, 1);
        assert!(!failed);

        let sampled = store::get(&conn, id).unwrap().unwrap();
        assert!(sampled.last_verified_at.is_some(), "the sampler's own successful call still counts as a review");
        assert!(sampled.is_superseded(), "newer_wins picked the neighbor (inserted after it) as the winner");
        assert_eq!(sampled.superseded_by, Some(neighbor));
    }

    #[test]
    fn run_strength_review_pass_malformed_reply_changes_nothing() {
        let conn = mem_conn();
        let id = store::insert(&conn, "the user prefers dark mode", None, None, true, Some(&[1.0f32, 0.0]), 7).unwrap();
        store::insert(&conn, "a related neighbor memory", None, None, true, Some(&[1.0f32, 0.1]), STRENGTH_MIN_IMPORTANCE - 1).unwrap();

        let llm = FixedReflectLlm { reply: Ok("I'm not sure about this one.") };
        let (stands, stale, routed, failed) = run_strength_review_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(stands, 0);
        assert_eq!(stale, 0);
        assert_eq!(routed, 0);
        assert!(!failed, "the call itself succeeded — only its content was malformed");
        assert!(store::get(&conn, id).unwrap().unwrap().last_verified_at.is_none(), "must stay due for a later run");
    }

    #[test]
    fn run_strength_review_pass_skips_memories_below_the_importance_floor() {
        let conn = mem_conn();
        let id =
            store::insert(&conn, "a low-importance memory", None, None, true, Some(&[1.0f32, 0.0]), STRENGTH_MIN_IMPORTANCE - 1)
                .unwrap();
        store::insert(&conn, "a related neighbor memory", None, None, true, Some(&[1.0f32, 0.1]), STRENGTH_MIN_IMPORTANCE - 1).unwrap();

        let llm = FixedReflectLlm { reply: Ok("STANDS") };
        let (stands, _stale, _routed, _failed) = run_strength_review_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(stands, 0, "importance below STRENGTH_MIN_IMPORTANCE must never be sampled");
        assert!(store::get(&conn, id).unwrap().unwrap().last_verified_at.is_none());
    }

    // --- ingest-sessions: engagement-gated reinforcement ---

    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().map(|b| b as f32).collect())
        }
    }

    /// A `TranscriptFilter` test double returning a fixed dialogue string
    /// (or `None`, simulating an unfiltered/empty transcript), never
    /// spawning a real `python3` process.
    struct FixedFilter {
        dialogue: Option<&'static str>,
    }

    impl ingest::TranscriptFilter for FixedFilter {
        fn filter(&self, _raw: &str) -> Option<String> {
            self.dialogue.map(String::from)
        }
    }

    /// A scratch directory under the system temp dir, unique per test run
    /// (mirrors `note.rs`'s own `read_from_editor` test-adjacent pattern of
    /// using `std::env::temp_dir()` directly rather than a `tempfile` crate
    /// dependency this workspace doesn't otherwise need). Callers create
    /// `<root>/projects/<proj>/<session>.jsonl` and `<root>/recall-log/...`
    /// under this the same way the real `$HOME` layout does.
    struct ScratchDir {
        path: PathBuf,
    }

    impl ScratchDir {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mach-kb-ingest-test-{}-{}-{}", std::process::id(), tag, n));
            std::fs::create_dir_all(&path).unwrap();
            ScratchDir { path }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Writes `<root>/projects/proj/<session_id>.jsonl` with `raw_lines`
    /// raw lines of content, then backdates its mtime by `age_secs` (or
    /// leaves it at "now" when `age_secs` is `None`, simulating a session
    /// still being written to) — the opportunistic sweep's staleness gate
    /// is mtime-driven, so tests need real file mtimes, not a fake clock.
    fn write_transcript(root: &Path, session_id: &str, raw_lines: usize, age_secs: Option<u64>) -> PathBuf {
        let dir = root.join("projects").join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jsonl", session_id));
        let content = (0..raw_lines).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        std::fs::write(&path, content).unwrap();
        if let Some(age) = age_secs {
            let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            set_mtime(&path, mtime);
        }
        path
    }

    /// Sets a file's mtime directly via `utimensat`-equivalent — the
    /// standard library has no portable setter, so this shells out to the
    /// `touch` coreutil (present on every Linux/macOS test runner this
    /// workspace targets) rather than adding a filetime crate dependency
    /// for one test helper.
    fn set_mtime(path: &Path, mtime: std::time::SystemTime) {
        let secs = mtime.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let ts = format!("@{}", secs);
        let status = std::process::Command::new("touch").arg("-d").arg(&ts).arg(path).status();
        assert!(status.map(|s| s.success()).unwrap_or(false), "test setup: `touch -d {} {:?}` must succeed", ts, path);
    }

    fn write_recall_log(root: &Path, session_id: &str, lines: &[&str]) {
        let dir = root.join("recall-log");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.jsonl", session_id)), lines.join("\n")).unwrap();
    }

    #[test]
    fn run_ingest_sessions_stale_session_with_no_recall_log_just_gets_digested() {
        let scratch = ScratchDir::new("digest-only");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        write_transcript(&scratch.path, "sess-a", ingest::DIGEST_MIN_TRANSCRIPT_LINES + 10, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));

        let conn = mem_conn();
        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("The user prefers dark mode.\nThe user works on project Zenith.") };
        let filter = FixedFilter { dialogue: Some("USER: I use dark mode\nASSISTANT: noted") };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.scanned, 1);
        assert_eq!(summary.processed, 1);
        assert_eq!(summary.engaged, 0);
        assert_eq!(summary.facts_added, 2);
        assert!(store::is_session_ingested(&conn, "sess-a").unwrap());
        let stored = store::unreviewed(&conn).unwrap();
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|m| m.source.as_deref() == Some("session-digest")));
    }

    #[test]
    fn run_ingest_sessions_touches_only_the_engaged_id() {
        let scratch = ScratchDir::new("engaged-vs-shown");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        write_transcript(&scratch.path, "sess-b", 5, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        write_recall_log(
            &scratch.path,
            "sess-b",
            &[r#"{"ts":"2026-01-01T00:00:00Z","ids":[1,2]}"#],
        );

        let conn = mem_conn();
        let engaged_id = store::insert(&conn, "the user prefers dark mode", None, None, true, Some(&[1.0]), 5).unwrap();
        let shown_id = store::insert(&conn, "the user once mentioned rust", None, None, true, Some(&[2.0]), 5).unwrap();
        assert_eq!((engaged_id, shown_id), (1, 2), "test fixture assumes fresh in-memory ids starting at 1");

        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("1 ENGAGED\n2 SHOWN") };
        let filter = FixedFilter { dialogue: Some("USER: yeah I always use dark mode, thanks for remembering\nASSISTANT: got it") };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.engaged, 1);
        assert_eq!(summary.shown, 1);
        assert_eq!(summary.processed, 1);

        let engaged = store::get(&conn, engaged_id).unwrap().unwrap();
        assert_eq!(engaged.access_count, 1, "the engaged memory must be touched");
        assert!(engaged.last_accessed_at.is_some());

        let shown = store::get(&conn, shown_id).unwrap().unwrap();
        assert_eq!(shown.access_count, 0, "the merely-shown memory must NOT be touched");
        assert!(shown.last_accessed_at.is_none());

        assert!(store::is_session_ingested(&conn, "sess-b").unwrap());
    }

    #[test]
    fn run_ingest_sessions_malformed_verdict_reply_touches_nothing() {
        let scratch = ScratchDir::new("malformed-verdict");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        write_transcript(&scratch.path, "sess-c", 5, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        write_recall_log(&scratch.path, "sess-c", &[r#"{"ts":"2026-01-01T00:00:00Z","ids":[1]}"#]);

        let conn = mem_conn();
        let id = store::insert(&conn, "the user likes tea", None, None, true, Some(&[1.0]), 5).unwrap();

        let embedder = FakeEmbedder;
        // Reply omits the only known id entirely -- parse_engagement_verdicts
        // must fail closed to all-SHOWN.
        let llm = FixedReflectLlm { reply: Ok("I'm not sure about this one.") };
        let filter = FixedFilter { dialogue: Some("USER: hello\nASSISTANT: hi") };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.engaged, 0);
        assert_eq!(summary.shown, 1);
        assert_eq!(store::get(&conn, id).unwrap().unwrap().access_count, 0, "never reinforce on doubt");
        assert!(store::is_session_ingested(&conn, "sess-c").unwrap());
    }

    #[test]
    fn run_ingest_sessions_failed_llm_call_leaves_session_unprocessed_for_retry() {
        let scratch = ScratchDir::new("offline");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        write_transcript(&scratch.path, "sess-d", 5, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        write_recall_log(&scratch.path, "sess-d", &[r#"{"ts":"2026-01-01T00:00:00Z","ids":[1]}"#]);

        let conn = mem_conn();
        let id = store::insert(&conn, "the user likes tea", None, None, true, Some(&[1.0]), 5).unwrap();

        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Err("offline") };
        let filter = FixedFilter { dialogue: Some("USER: hello\nASSISTANT: hi") };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.deferred_offline, 1);
        assert_eq!(summary.processed, 0);
        assert!(!store::is_session_ingested(&conn, "sess-d").unwrap(), "must stay unprocessed so it's retried");
        assert_eq!(store::get(&conn, id).unwrap().unwrap().access_count, 0);
    }

    #[test]
    fn run_ingest_sessions_skips_sessions_still_too_fresh_to_be_stale() {
        let scratch = ScratchDir::new("too-fresh");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        // No age override -- mtime stays at "now", well under SWEEP_MIN_IDLE_SECS.
        write_transcript(&scratch.path, "sess-e", 5, None);

        let conn = mem_conn();
        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("STANDS") };
        let filter = FixedFilter { dialogue: Some("USER: hi\nASSISTANT: hello") };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.scanned, 0, "a fresh transcript must never enter the opportunistic sweep's candidate set");
        assert!(!store::is_session_ingested(&conn, "sess-e").unwrap());
    }

    #[test]
    fn run_ingest_sessions_session_id_bypasses_the_staleness_gate() {
        let scratch = ScratchDir::new("session-id-bypass");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        // Fresh mtime -- would never qualify for the bare sweep.
        write_transcript(&scratch.path, "sess-f", 5, None);

        let conn = mem_conn();
        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("") };
        let filter = FixedFilter { dialogue: Some("USER: hi\nASSISTANT: hello") };

        let summary = run_ingest_sessions(
            &conn,
            &embedder,
            &llm,
            &filter,
            &projects_root,
            &recall_log_root,
            Some("sess-f"),
            &store::now_rfc3339(),
        )
        .unwrap();

        assert_eq!(summary.scanned, 1, "the SessionEnd fast trigger must bypass the idle-time gate");
        assert!(store::is_session_ingested(&conn, "sess-f").unwrap());
    }

    #[test]
    fn run_ingest_sessions_processed_set_prevents_double_touch_on_rerun() {
        let scratch = ScratchDir::new("idempotent");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        write_transcript(&scratch.path, "sess-g", 5, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        write_recall_log(&scratch.path, "sess-g", &[r#"{"ts":"2026-01-01T00:00:00Z","ids":[1]}"#]);

        let conn = mem_conn();
        let id = store::insert(&conn, "the user prefers dark mode", None, None, true, Some(&[1.0]), 5).unwrap();

        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("1 ENGAGED") };
        let filter = FixedFilter { dialogue: Some("USER: yes I always use dark mode\nASSISTANT: got it") };

        let now = store::now_rfc3339();
        let first = run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &now).unwrap();
        assert_eq!(first.engaged, 1);
        assert_eq!(store::get(&conn, id).unwrap().unwrap().access_count, 1);

        // Re-run against the exact same transcript/recall-log with the same
        // "ENGAGED" reply available -- the processed-set must skip it
        // entirely rather than touching it a second time.
        let second = run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &now).unwrap();
        assert_eq!(second.scanned, 1, "still enumerated as a candidate...");
        assert_eq!(second.processed, 0, "...but skipped before doing any work");
        assert_eq!(second.engaged, 0);
        assert_eq!(store::get(&conn, id).unwrap().unwrap().access_count, 1, "must not be touched a second time");
    }

    #[test]
    fn list_transcript_files_finds_nested_jsonl_and_ignores_other_files() {
        let scratch = ScratchDir::new("listing");
        let projects_root = scratch.path.join("projects");
        write_transcript(&scratch.path, "sess-h", 3, None);
        std::fs::write(projects_root.join("proj").join("notes.txt"), "not a transcript").unwrap();

        let files = list_transcript_files(&projects_root);
        assert_eq!(files.len(), 1);
        assert_eq!(ingest::session_id_from_path(&files[0]).as_deref(), Some("sess-h"));
    }

    #[test]
    fn list_transcript_files_on_missing_root_is_empty() {
        let missing = std::env::temp_dir().join("mach-kb-ingest-test-definitely-missing-root");
        assert_eq!(list_transcript_files(&missing), Vec::<PathBuf>::new());
    }
}
