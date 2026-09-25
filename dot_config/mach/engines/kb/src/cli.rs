
//! kb — subcommand dispatch for `mach kb ...`, matching the hand-rolled
//! arg-parsing style the sweep engine's `cli` module already uses (no
//! clap).
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::classify::{self, Classifier, Verdict};
use crate::code_index::ask as code_ask;
use crate::code_index::job;
use crate::embed::{Embedder, OllamaEmbedder};
use crate::eval;
use crate::export;
use crate::health;
use crate::improve::{self, ImproveLlm, Vcs};
use crate::ingest;
use crate::ask;
use crate::projects;
use crate::transcripts;
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
    println!("                          ranked top-N search: hybrid cosine + FTS5 exact-token");
    println!("                          (max, never sum) blended with recency/strength;");
    println!("                          unreviewed rows are included by default at a small");
    println!("                          confidence penalty — --reviewed-only excludes them;");
    println!("                          --touch reinforces the rows actually returned");
    println!("  supersede <old_id> <new_id>");
    println!("                          tombstone <old_id> in favor of <new_id>");
    println!("  review                  interactive review of unreviewed candidates — optional");
    println!("                          human override; `mach kb reflect`'s curation pass");
    println!("                          already promotes/demotes this queue on its own");
    println!("  list [--limit N] [--superseded] [--dormant] [--pinned]");
    println!("                          most recent memories (--superseded/--dormant/--pinned: audit views)");
    println!("  forget <id>             permanently delete a memory");
    println!("  restore <id>            undo a supersession: make a tombstoned memory active again;");
    println!("                          pins it so dedupe/contradiction/verdict passes never");
    println!("                          re-tombstone it automatically (manual supersede still can)");
    println!("  unpin <id>              clear a pin set by `restore`, so automatic passes may");
    println!("                          tombstone the row again");
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
    println!("  model [--json] [--max-chars N]");
    println!("                          compact mental-model view (active themes/insights only,");
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
    println!("  improve [--dry-run] [--force]");
    println!("                          self-improvement pass: when enough new signal (memories +");
    println!("                          prefers/rejects/values edges) has accrued since its last run,");
    println!("                          hands the evidence to one agentic claude call allowed to edit");
    println!("                          ~/.claude/skills, CLAUDE.md, settings.json and claude-hooks;");
    println!("                          verifies, commits via chezmoi, records the outcome as a memory.");
    println!("                          --dry-run prints the prompt; --force ignores the threshold");
    println!("  entity <name>           one entity: its reflection-built CARD (a consolidated");
    println!("                          profile of a person, project, practice or tool) plus its");
    println!("                          association-graph edges both directions, with evidence");
    println!("                          memory + date (exact name match first, else embedding");
    println!("                          similarity)");
    println!("  why <id> | why insight <id>");
    println!("                          provenance trace for one memory (or insight/theme):");
    println!("                          source + basis, supersession chain, likely origin");
    println!("                          session/meeting transcript, insights that cite it,");
    println!("                          graph edges it evidences, Hebbian associates,");
    println!("                          engagement + how often recall has shown it. Read-only.");
    println!("  eval-ask [--file F] [--json]");
    println!("                          score `ask` against questions whose answers live in transcripts");
    println!("  transcripts \"<query>\" [--limit N]");
    println!("                          search raw session passages directly (what ask's TRANSCRIPT: step sees)");
    println!("  index-transcripts [--all]");
    println!("                          index raw session transcripts for `ask` (incremental by mtime)");
    println!("  index [--project P] [--budget N] [--path-prefix P] [--history-only] [--dry-run]");
    println!("                          incremental code index: scope pass, tree-sitter chunks,");
    println!("                          per-file LLM context headers, embeddings, for every");
    println!("                          registered git project (others reported skipped).");
    println!("                          --budget caps `claude -p` calls (default 300), stopping");
    println!("                          cleanly and resuming next run; --dry-run prints the plan");
    println!("                          and changes nothing");
    println!("  index status [--project P]");
    println!("                          per project: head vs indexed head, files by status,");
    println!("                          chunk count, embedded %");
    println!("  index scope <project> [--set <dir>=<category>]...");
    println!("                          no --set: current per-directory scope categories;");
    println!("                          --set: a manual (\"user\") override the scope pass never");
    println!("                          clobbers");
    println!("  eval-code [--file F] [--project X] [--json]");
    println!("                          score the code-aware `ask` loop: a question passes when one");
    println!("                          of its expect_paths appears in the answer's citations");
    println!("  projects [list|refresh|mark-indexed <name>|forget <name>]");
    println!("                          project registry: rename detection, cards, index drift");
    println!("  ask \"<question>\" [--rounds N] [--json]");
    println!("                          iterative recall: search, judge, reword or hop, then answer");
    println!("  cards [--all] [--limit N]");
    println!("                          build the entity cards that are due right now, without");
    println!("                          running the rest of reflect. --all keeps going until none");
    println!("                          are due (one haiku call per entity)");
    println!("  eval [--file F] [--json] [--verbose]");
    println!("                          score retrieval against a fixed question set: did recall");
    println!("                          surface the memory that answers each question? Reports");
    println!("                          pass rate per category (extraction, multi_session,");
    println!("                          temporal, knowledge_update, abstention), stale-fact");
    println!("                          serves, and mean injected characters. Read-only, no LLM.");
    println!("  graph --stats           entity/edge/mention/card counts by kind — a compact view");
    println!("                          of the graph `mach kb reflect` has derived so far");
    println!("  graph audit             batched KEEP/POISONED/GENERIC judgment over every active");
    println!("                          edge; POISONED/GENERIC edges are invalidated (never deleted)");
    println!("  health [--notify]       operational self-check (ollama, kb.db, kb socket, reflect");
    println!("                          cadence, disk headroom, recall-log dir, telegram-state");
    println!("                          staleness); --notify sends one desktop alert on failure");
    println!("  recall-stats [--days N]");
    println!("                          the recall-precision tuning metric: per session judged in");
    println!("                          the window (default 14 days), how many injected memories");
    println!("                          were shown vs actually engaged (`mach kb ingest-sessions`'s");
    println!("                          verdicts, persisted past the recall-log's own 14-day prune),");
    println!("                          then an overall precision line");
    println!("  audit-supersessions [--limit N] [--apply] [--json] [--reaudit]");
    println!("                          LLM-judged audit of every tombstoned memory against its");
    println!("                          direct successor: did the tombstone lose a fact (often a");
    println!("                          dated one) the successor doesn't carry? Dry run by default");
    println!("                          (prints LOSSY pairs + a summary, changes nothing); --apply");
    println!("                          restores and pins each LOSSY row; --reaudit re-examines");
    println!("                          rows already recorded (skipped by default)");
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
        Some("restore") => cmd_restore(args),
        Some("unpin") => cmd_unpin(args),
        Some("wake") => cmd_wake(args),
        Some("reflect") => cmd_reflect(args),
        Some("insights") => cmd_insights(args),
        Some("insight-forget") => cmd_insight_forget(args),
        Some("tree") => cmd_tree(args),
        Some("model") => cmd_model(args),
        Some("export") => cmd_export(args),
        Some("import") => cmd_import(args),
        Some("ingest-sessions") => cmd_ingest_sessions(args),
        Some("improve") => cmd_improve(args),
        Some("entity") => cmd_entity(args),
        Some("why") => cmd_why(args),
        Some("eval") => cmd_eval(args),
        Some("cards") => cmd_cards(args),
        Some("ask") => cmd_ask(args),
        Some("eval-ask") => cmd_eval_ask(args),
        Some("eval-code") => cmd_eval_code(args),
        Some("index-transcripts") => cmd_index_transcripts(args),
        Some("index") => cmd_index(args),
        Some("projects") => cmd_projects(args),
        Some("transcripts") => cmd_transcripts(args),
        Some("graph") => cmd_graph(args),
        Some("health") => cmd_health(args),
        Some("recall-stats") => cmd_recall_stats(args),
        Some("audit-supersessions") => cmd_audit_supersessions(args),
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
        AddOutcome::Added { id } => {
            // `mach kb add` is deliberate, human-authored input: basis `stated`.
            let _ = store::set_basis(&conn, id, store::BASIS_STATED);
            println!("stored memory #{}", id)
        }
        AddOutcome::AddedRefining { new_id, old_id } => {
            let _ = store::set_basis(&conn, new_id, store::BASIS_STATED);
            println!("stored memory #{} (refines memory #{})", new_id, old_id);
        }
        AddOutcome::AddedAndTombstoned { new_id, old_id, verb } => {
            let _ = store::set_basis(&conn, new_id, store::BASIS_STATED);
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
    // Set on a memory hit that did NOT match the query itself but was
    // pulled in by spreading activation over `memory_assoc` from the hit
    // whose id this is (see `spread_assoc`). Omitted from JSON otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_assoc: Option<i64>,
    // With `via_assoc`: which link first reached this memory in spreading
    // activation -- "entity:<name>", "assoc" (Hebbian co-engagement), or
    // "temporal". Omitted on direct hits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_edge: Option<String>,
    // `Memory::basis` ("stated" | "inferred") when the row recorded one;
    // omitted otherwise (legacy rows, insights, channels that don't
    // classify). Lets kb-recall.py say "you told me" vs "I inferred".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub basis: Option<String>,
    // IDF-weighted term coverage from the FTS5 lexical channel
    // (`store::search_hybrid` / `store::lexical_scores`): 1.0 when the hit
    // matched every query term's IDF mass, not normalized against the best
    // hit in the result set; omitted when 0 (no token matched, or the hit
    // came from insights/association where the channel does not apply).
    // `sim` stays the raw cosine; the blended `score` already reflects
    // whichever of the two was higher.
    #[serde(skip_serializing_if = "f32_is_zero")]
    pub lexical: f32,
}

fn f32_is_zero(v: &f32) -> bool {
    *v == 0.0
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
        via_assoc: None,
        via_edge: None,
        basis: h.memory.basis,
        lexical: h.lexical,
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
        via_assoc: None,
        via_edge: None,
        basis: None,
        lexical: 0.0,
    }
}

/// One association-graph edge (or, for a 2-hop connection, a two-edge
/// chain) surfaced alongside a search's ordinary hits — never logged/shown
/// as ids (no reinforcement semantics for the graph in this first version,
/// unlike a memory hit's own `--touch`), just a plain "here's something
/// connected to what you asked about" note.
///
/// `hops` is `1` for a direct edge touching the query-matched entity
/// (`src_name`/`predicate`/`dst_name` describe it exactly as stored, same
/// shape this struct always had) or `2` for a spreading-activation hop
/// through one of that edge's own neighbors — in which case `predicate2`/
/// `src_name2`/`dst_name2` carry the second edge. EVERY edge is reported in
/// its own stored `src`/`dst` order, never re-oriented to read as a chain
/// from the matched entity: hop 1 is `src_name --predicate--> dst_name`
/// exactly as stored, hop 2 is `src_name2 --predicate2--> dst_name2`
/// exactly as stored, and the pivot entity (the one the two edges share)
/// appears in both. When `src_name2 == dst_name` the two read as one chain;
/// otherwise a renderer shows them as two edges. (Before this, hop 2 was
/// always rendered `matched --p1--> pivot --p2--> far`, which silently
/// flipped any edge stored the other way round -- "user deploys-to
/// remosspace.com" came out as "remosspace.com deploys-to user".) All three
/// are `None` for a 1-hop connection (and omitted from JSON entirely via
/// `skip_serializing_if`, so an old consumer reading only `src_name`/
/// `predicate`/`dst_name`/`evidence_date` sees no shape change).
#[derive(Serialize, Clone)]
pub struct ConnectionHit {
    pub src_name: String,
    pub predicate: String,
    pub dst_name: String,
    pub evidence_date: String,
    pub hops: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub predicate2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_name2: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_name2: Option<String>,
}

/// `mach kb search`'s full response shape: the ordinary ranked hits plus, if
/// the query itself named something close enough to a known entity, a
/// handful of that entity's active edges. A wrapping object rather than a
/// bare array (the shape `mach kb search --json`/the kb socket used to
/// return) precisely because there are now two independent things to carry
/// — `connections` has no natural home inside one `SearchHit` row, since it
/// belongs to the *query*, not to any single hit.
#[derive(Serialize)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    pub connections: Vec<ConnectionHit>,
    /// The card of the entity this query named, when it has one -- a
    /// consolidated profile injected instead of leaving recall to
    /// reassemble the same picture from scattered memories. At most
    /// `SEARCH_MAX_CARDS`; empty (and omitted from JSON) otherwise.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cards: Vec<CardHit>,
    /// Derivable structure for the session's project (see `projects`).
    /// Absent when the session is not in a registered project.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_card: Option<String>,
}

/// One entity card surfaced alongside a search's hits.
#[derive(Serialize, Clone)]
pub struct CardHit {
    pub entity: String,
    pub kind: Option<String>,
    pub text: String,
    /// When the card was last rebuilt (date only).
    pub updated: String,
    /// How many active memories it was distilled from.
    pub evidence: usize,
}

/// Cards surfaced per search. One: the query's own named entity. A second
/// would already be as long as the hits it is meant to summarize.
pub const SEARCH_MAX_CARDS: usize = 1;

/// At or above this cosine similarity between the query embedding and an
/// entity's own name embedding, that entity's connections are surfaced
/// alongside search results — deliberately lower than
/// `reflect::ENTITY_RESOLUTION_SIM_THRESHOLD` (which governs whether two
/// *candidate entity names* are treated as the same thing): a query is
/// ordinary prose, not a short canonical name, so it will never be as close
/// to an entity's embedding as another well-formed name would be, and this
/// path is additive/informational rather than a dedup decision.
pub const SEARCH_ENTITY_CONNECTION_SIM_THRESHOLD: f32 = 0.75;

/// Overall cap on how many connections (1-hop and 2-hop combined) are
/// surfaced per search — a handful of "you might also want to know"
/// connections, not a full `mach kb entity <name>` dump. Hop-1 connections
/// fill this first ("full weight" — no confidence gate of their own, same
/// as before this cap covered 2 hops), so a matched entity with 5 or more
/// direct edges alone already exhausts the cap and no 2-hop walk ever runs.
pub const SEARCH_MAX_CONNECTIONS: usize = 5;

/// A hop-1 edge only seeds a hop-2 walk through its own far entity when its
/// own confidence is at or above this floor — a low-confidence direct edge
/// is shaky enough evidence on its own without compounding it with a second
/// hop's worth of uncertainty.
pub const SEARCH_HOP2_MIN_CONFIDENCE: f64 = 0.6;

/// At most this many hop-2 edges are taken through any single hop-1
/// neighbor — bounds how much one well-connected neighbor can dominate the
/// overall cap.
pub const SEARCH_HOP2_MAX_PER_NEIGHBOR: usize = 2;

/// Builds one hop's `ConnectionHit` from a relation row, in the relation's
/// own stored `src`/`dst` order (never swapped — direction is never
/// something this enrichment corrects for, same as before this function
/// existed in its own right).
fn connection_hit_for_edge(conn: &Connection, e: &store::Relation, hops: u8) -> Option<ConnectionHit> {
    let src_name = store::get_entity(conn, e.src).ok().flatten()?.name;
    let dst_name = store::get_entity(conn, e.dst).ok().flatten()?.name;
    let evidence_date = e
        .evidence_memory_id
        .and_then(|id| store::get(conn, id).ok().flatten())
        .map(|m| m.created_at.get(..10).unwrap_or(&m.created_at).to_string())
        .unwrap_or_default();
    Some(ConnectionHit { src_name, predicate: e.predicate.clone(), dst_name, evidence_date, hops, predicate2: None, src_name2: None, dst_name2: None })
}

/// If `q_emb` is close enough (`SEARCH_ENTITY_CONNECTION_SIM_THRESHOLD`) to
/// a known entity's own name embedding, returns up to
/// `SEARCH_MAX_CONNECTIONS` connections as `ConnectionHit`s — a bounded
/// 2-hop spreading-activation walk from that entity: every active edge
/// touching it directly (hop 1, "full weight" — no confidence gate, exactly
/// like this enrichment behaved before hop 2 existed), and, for each hop-1
/// edge whose own confidence clears `SEARCH_HOP2_MIN_CONFIDENCE`, up to
/// `SEARCH_HOP2_MAX_PER_NEIGHBOR` of that edge's far entity's own active
/// edges (hop 2) — deduped by edge id (an edge already surfaced, at either
/// hop, is never surfaced twice) and never walking straight back to the
/// original matched entity (that's not new information). Hop-1 connections
/// fill the overall cap first, so they're never displaced by hop-2 ones.
///
/// Evidence date only — never a bare memory id, per this feature's
/// "connections are never logged/shown as ids" contract. Any lookup
/// failure along the way (no match, a vanished entity/memory row) simply
/// yields fewer or zero connections, never an error — this is enrichment,
/// not a required part of a search response.
/// `store::active_relations_for_entity`, filtered to exclude any edge whose
/// evidence memory is currently dormant — the graph hygiene pass's "respect
/// at query time" half of evidence-death propagation: a dormant fact can
/// wake (`mach kb wake`), so its edge must not be tombstoned, only left out
/// of *this* recall-connections view until it does. Used only here (search
/// enrichment); `mach kb entity <name>` (an audit view, not recall) keeps
/// showing these edges via the unfiltered `active_relations_for_entity`.
fn active_relations_for_recall(conn: &Connection, entity_id: i64) -> Vec<store::Relation> {
    store::active_relations_for_entity(conn, entity_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !store::relation_evidence_is_dormant(conn, r).unwrap_or(false))
        .collect()
}

/// An entity literally named in the query always matches, regardless of
/// embedding similarity: whole-sentence embeddings vs a short entity name
/// are threshold-fragile ("tell me about Moses and what he wants" cleared
/// 0.75 while "what does moses want in umoja" did not, live). Name match is
/// case-insensitive on word boundaries; the longest matching name wins
/// (most specific). Embedding similarity remains the fallback for prompts
/// that only paraphrase an entity.
fn entity_named_in_query(conn: &Connection, query: &str) -> Option<store::Entity> {
    let q = query.to_lowercase();
    let mut best: Option<store::Entity> = None;
    for e in store::all_entities(conn).unwrap_or_default() {
        let name = e.name.to_lowercase();
        if name.len() < 3 || name == "user" {
            continue; // too short to be a reliable mention; "user" matches everything
        }
        let found = store::name_mentioned(&q, &name);
        if found && best.as_ref().map_or(true, |b| name.len() > b.name.len()) {
            best = Some(e);
        }
    }
    best
}

/// The card of the entity this query names (or, failing a literal name, the
/// closest entity by name embedding -- the same resolution
/// `entity_connections_for_query` uses). Enrichment: any lookup failure
/// yields no cards, never an error.
fn cards_for_query(conn: &Connection, query: &str, q_emb: &[f32]) -> Vec<CardHit> {
    let entity = match entity_named_in_query(conn, query) {
        Some(e) => Some(e),
        None => match store::find_entity_by_similarity(conn, q_emb, SEARCH_ENTITY_CONNECTION_SIM_THRESHOLD) {
            Ok(Some((e, _))) => Some(e),
            _ => None,
        },
    };
    let Some(entity) = entity else { return Vec::new() };
    match store::get_entity_card(conn, entity.id) {
        Ok(Some(card)) => vec![CardHit {
            entity: entity.name,
            kind: entity.kind,
            text: card.text,
            updated: card.updated_at.get(..10).unwrap_or(&card.updated_at).to_string(),
            evidence: card.source_ids.len(),
        }],
        _ => Vec::new(),
    }
}

fn entity_connections_for_query(conn: &Connection, query: &str, q_emb: &[f32]) -> Vec<ConnectionHit> {
    let entity = match entity_named_in_query(conn, query) {
        Some(e) => e,
        None => match store::find_entity_by_similarity(conn, q_emb, SEARCH_ENTITY_CONNECTION_SIM_THRESHOLD) {
            Ok(Some((e, _sim))) => e,
            _ => return Vec::new(),
        },
    };
    let hop1_edges = active_relations_for_recall(conn, entity.id);
    let mut out: Vec<ConnectionHit> = Vec::new();
    let mut used_edge_ids: HashSet<i64> = HashSet::new();
    // Distinct STATEMENTS, not distinct rows: the same claim extracted from
    // two different memories is two active relation rows with identical
    // endpoints and predicate, and rendering both said one thing twice and
    // spent two lines of injection on it ("user -deploys-to-> remosspace.com"
    // appeared twice; "user -works-on-> Helios" exists five times).
    let mut seen_claims: HashSet<(i64, String, i64)> = HashSet::new();

    for e in &hop1_edges {
        if out.len() >= SEARCH_MAX_CONNECTIONS {
            return out;
        }
        if !seen_claims.insert((e.src, e.predicate.to_lowercase(), e.dst)) {
            used_edge_ids.insert(e.id); // same claim: never re-surface it at hop 2 either
            continue;
        }
        if let Some(hit) = connection_hit_for_edge(conn, e, 1) {
            used_edge_ids.insert(e.id);
            out.push(hit);
        }
    }

    for e1 in &hop1_edges {
        if out.len() >= SEARCH_MAX_CONNECTIONS {
            break;
        }
        if e1.confidence.unwrap_or(0.0) < SEARCH_HOP2_MIN_CONFIDENCE {
            continue;
        }
        let pivot_id = if e1.src == entity.id { e1.dst } else { e1.src };
        let pivot = match store::get_entity(conn, pivot_id) {
            Ok(Some(p)) => p,
            _ => continue,
        };
        let hop2_edges = active_relations_for_recall(conn, pivot_id);
        let mut taken_for_this_neighbor = 0usize;
        for e2 in &hop2_edges {
            if out.len() >= SEARCH_MAX_CONNECTIONS || taken_for_this_neighbor >= SEARCH_HOP2_MAX_PER_NEIGHBOR {
                break;
            }
            if used_edge_ids.contains(&e2.id) {
                continue; // dedup: this exact edge already surfaced (e.g. as hop 1)
            }
            let far_id = if e2.src == pivot_id { e2.dst } else { e2.src };
            if far_id == entity.id {
                continue; // a trivial walk straight back to the matched entity -- not new information
            }
            if !seen_claims.insert((e2.src, e2.predicate.to_lowercase(), e2.dst)) {
                continue; // a claim already rendered at either hop
            }
            let far_name = match store::get_entity(conn, far_id) {
                Ok(Some(f)) => f.name,
                _ => continue,
            };
            let evidence_date = e2
                .evidence_memory_id
                .and_then(|id| store::get(conn, id).ok().flatten())
                .map(|m| m.created_at.get(..10).unwrap_or(&m.created_at).to_string())
                .unwrap_or_default();
            // Each hop in its own STORED direction (see `ConnectionHit`):
            // the matched entity may be e1's dst, and the pivot may be
            // e2's dst -- re-orienting either to read "matched -> pivot ->
            // far" is exactly the direction flip this used to produce.
            let (h1_src, h1_dst) = if e1.src == entity.id {
                (entity.name.clone(), pivot.name.clone())
            } else {
                (pivot.name.clone(), entity.name.clone())
            };
            let (h2_src, h2_dst) = if e2.src == pivot_id {
                (pivot.name.clone(), far_name)
            } else {
                (far_name, pivot.name.clone())
            };
            out.push(ConnectionHit {
                src_name: h1_src,
                predicate: e1.predicate.clone(),
                dst_name: h1_dst,
                evidence_date,
                hops: 2,
                predicate2: Some(e2.predicate.clone()),
                src_name2: Some(h2_src),
                dst_name2: Some(h2_dst),
            });
            used_edge_ids.insert(e2.id);
            taken_for_this_neighbor += 1;
        }
    }
    out
}

/// The derivable card for the session's project, if it has one.
///
/// Looked up by name rather than retrieved by similarity: when the session
/// is in a project, that project's card is the right one by construction,
/// and a similarity search could return a different project's card.
pub fn project_card_for(conn: &Connection, project: Option<&str>) -> Result<Option<String>, KbError> {
    let Some(project) = project else {
        return Ok(None);
    };
    let name = project.to_lowercase();
    Ok(store::get_project_by_name(conn, &name)?.and_then(|p| p.card))
}

/// Ranked top-N search + insight blend, embedding `query` itself — the same
/// merge `cmd_search` performs for `mach kb search --json` (mem hits and
/// insight hits fetched independently, combined, sorted by score, truncated
/// to `limit`, then `min_score`-filtered). `limit` bounds the QUERY MATCHES;
/// graph hops (`spread_graph`) are appended after that cut as enrichment,
/// so a response holds at most `limit + store::SPREAD_MAX_OUT` hits. Extracted as its own `pub`
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
///
/// Also computes the query's own entity-connections enrichment
/// (`entity_connections_for_query`) from the same query embedding — one
/// embed call serves both the ordinary hit ranking and this additive
/// "here's something connected to what you asked about" field, so this is
/// the one shared path both the kb socket and `mach kb search` build on.
///
/// `project` is the caller's best guess at the session project (today,
/// `basename(cwd)` -- see `kb-recall.py`'s `project_name`), used only as a
/// fallback label. `cwd`, when given, is resolved against the `projects`
/// registry (`store::project_for_path`) and its match -- an actual claim
/// about identity, not a guess -- wins as the SESSION PROJECT: it decides
/// which project's card rides along (`project_card_for`). A caller with no
/// `cwd` (every existing one: `cmd_eval`, `ask`, every test) falls back to
/// `project` exactly as before this parameter existed.
///
/// The other-project down-weight (`apply_project_penalty`) is narrower than
/// the card lookup: it fires only when `cwd` itself resolved to a
/// registered project (never for the `project` basename fallback, which is
/// a guess, not a claim), and only against hits tagged with another name
/// that is ALSO registered in the `projects` table -- an unregistered tag
/// like `claude-config` is left alone. See `apply_project_penalty`'s doc
/// comment for why both restrictions matter.
///
/// `date_anchor`, when given, overrides `now` as the "today" that relative-
/// date query terms ("yesterday", "last week", "in august") resolve
/// against (see `store::query_date_range`) -- `now` itself keeps driving
/// recency and strength scoring unchanged. This exists solely for `mach kb
/// eval`'s `Question.as_of`, so a relative-date question written on one day
/// keeps scoring correctly on a later day; every other caller passes
/// `None` and gets exactly today's behaviour.
#[allow(clippy::too_many_arguments)]
pub fn search_hits<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    query: &str,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
    project: Option<&str>,
    cwd: Option<&str>,
    date_anchor: Option<&str>,
) -> Result<SearchResponse, KbError> {
    let cwd_project = cwd.and_then(|c| store::project_for_path(conn, c).ok().flatten());
    let resolved_from_cwd = cwd_project.is_some();
    let session_project: Option<String> = match cwd_project {
        Some(row) => Some(row.name),
        None => project.map(|p| p.to_string()),
    };
    // Only a `cwd` match against the `projects` registry is an actual claim
    // about identity (see the doc comment above); the `project` fallback is
    // just a basename guess and must never trigger the penalty below, or an
    // unrelated same-named directory could down-weight real memories.
    // Loaded once per call (not per hit) and only when it can matter.
    let registered_projects: HashSet<String> = if resolved_from_cwd {
        store::list_projects(conn)?.iter().map(|p| p.name.to_lowercase()).collect()
    } else {
        HashSet::new()
    };
    let q_emb = embedder.embed(query)?;
    // Hybrid: cosine over embeddings plus the FTS5 exact-token channel
    // (`store::search_hybrid`), so a NORAD number, hostname, or ticket name
    // the embedding blurs still ranks.
    //
    // Fetch double the candidates when a session project is known: rows
    // from OTHER projects are about to be scored down
    // (`OTHER_PROJECT_PENALTY`, below) before the truncate to `limit`, and
    // without the extra headroom a page of other-project hits could crowd
    // the session project's own hits out of contention entirely rather
    // than merely rank behind them.
    let fetch_limit = if session_project.is_some() { limit * 2 } else { limit };
    let mem_hits =
        store::search_hybrid(conn, query, &q_emb, fetch_limit, reviewed_only, include_superseded, 0.0, now, date_anchor)?;
    let insight_hits = store::search_insights_ranked(conn, &q_emb, limit, now)?;
    let mut mem_hits: Vec<SearchHit> = mem_hits.into_iter().map(to_hit).collect();
    // Down-weight memories explicitly tagged to a DIFFERENT project than
    // the session's own, so memories tagged with other projects no longer
    // compete equally against ones relevant to where the session actually
    // is. A memory with `project = None` (most of them) is left alone --
    // there is nothing to compare against, and it was never a competing
    // claim to begin with.
    if resolved_from_cwd {
        if let Some(sp) = session_project.as_deref() {
            apply_project_penalty(&mut mem_hits, sp, &registered_projects);
        }
    }
    // Threshold the query matches before spreading: a memory that only
    // scraped in under the floor by embedding must still be reachable as a
    // graph neighbour of a real hit (and then carries the neighbour score).
    // Equivalent to the post-limit threshold below for the matches
    // themselves -- both are "top-N among rows clearing the floor".
    if min_score > 0.0 {
        mem_hits.retain(|h| h.score >= min_score);
    }
    // `limit` is the budget for QUERY MATCHES. Graph hops are enrichment on
    // top of it, not competitors for its slots: a hop always scores below
    // the hit it was reached through (by construction), so letting them into
    // the same sort meant they were always the first thing truncated away --
    // with the recall hook's limit of 4 a hop could never be seen at all.
    let mut combined: Vec<SearchHit> =
        mem_hits.into_iter().chain(insight_hits.into_iter().map(insight_to_hit)).collect();
    combined.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    combined.truncate(limit);
    if min_score > 0.0 {
        combined.retain(|h| h.score >= min_score);
    }
    // Spread from the matches that survived, and append what it reaches
    // (already capped at `store::SPREAD_MAX_OUT`, and held to the same score
    // floor so a faint hop is never injected).
    let mut hops = spread_graph(conn, &combined, now, Some(&q_emb))?;
    // Same down-weighting as the direct hits, applied before the same
    // score-floor filter below -- a hop into another project's memory is no
    // more entitled to a slot than a direct hit on one.
    if resolved_from_cwd {
        if let Some(sp) = session_project.as_deref() {
            apply_project_penalty(&mut hops, sp, &registered_projects);
        }
    }
    if min_score > 0.0 {
        hops.retain(|h| h.score >= min_score);
    }
    combined.extend(hops);
    let connections = entity_connections_for_query(conn, query, &q_emb);
    let cards = cards_for_query(conn, query, &q_emb);
    let project_card = project_card_for(conn, session_project.as_deref())?;
    Ok(SearchResponse { hits: combined, connections, cards, project_card })
}

/// How much an other-project memory's score is scaled by in `search_hits`
/// when the session's own project is known -- see `apply_project_penalty`.
/// 0.5 rather than something harsher: an other-project memory is still a
/// real memory of this same user and may be the best (or only) answer to a
/// cross-project question; it should rank behind an equally-relevant
/// same-project or unlabeled memory, not be silenced outright.
pub const OTHER_PROJECT_PENALTY: f32 = 0.5;

/// Scales the score of every hit tagged to a REGISTERED project other than
/// `session_project` (case-insensitive) by `OTHER_PROJECT_PENALTY`, in
/// place. `registered_projects` is the lowercased set of names in the
/// `projects` table, loaded once by the caller. A hit with `project = None`
/// -- most memories, and every insight hit (`insight_to_hit` always sets it
/// to `None`) -- is left untouched, and so is a hit tagged with a project
/// name that was never registered (`claude-config`, `fastapi-hub` as a raw
/// tag, ad-hoc labels, ...): those are not a competing claim about *this*
/// user's *other* indexed project, just a label, and penalizing them made
/// ordinary same-project memories rank behind their own kind. The caller
/// must only invoke this when the session project itself was resolved from
/// `cwd` via `store::project_for_path` -- a basename guess is not a real
/// identity claim either, so it must never be the basis for penalizing
/// other memories.
fn apply_project_penalty(hits: &mut [SearchHit], session_project: &str, registered_projects: &HashSet<String>) {
    for h in hits.iter_mut() {
        if let Some(p) = h.project.as_deref() {
            if registered_projects.contains(&p.to_lowercase()) && !p.eq_ignore_ascii_case(session_project) {
                h.score *= OTHER_PROJECT_PENALTY;
            }
        }
    }
}

/// Spreading activation over the memory graph (`store::spread_activation`):
/// from the top `SPREAD_SEED_HITS` query matches, walk shared-entity,
/// Hebbian-association, and time-proximity links for two steps and pull in
/// up to `store::SPREAD_MAX_OUT` memories the query itself missed. A reached
/// memory's score is its activation (always below the seed it came through)
/// and it is tagged with that seed (`via_assoc`) and the first link that
/// reached it (`via_edge`). Hubs (entities mentioned everywhere) carry no
/// signal and are skipped inside the store.
pub const SPREAD_SEED_HITS: usize = 4;

fn spread_graph(
    conn: &Connection,
    hits: &[SearchHit],
    now: &str,
    q_emb: Option<&[f32]>,
) -> Result<Vec<SearchHit>, KbError> {
    let seeds: Vec<(i64, f32)> =
        hits.iter().filter(|h| !h.superseded && !h.derived).take(SPREAD_SEED_HITS).map(|h| (h.id, h.score)).collect();
    let present: HashSet<i64> = hits.iter().map(|h| h.id).collect();
    let mut out = Vec::new();
    for g in store::spread_activation(conn, &seeds, &present, now, q_emb)? {
        let Some(m) = store::get(conn, g.id)? else { continue };
        out.push(SearchHit {
            id: m.id,
            content: m.content,
            source: m.source,
            project: m.project,
            created_at: m.created_at,
            score: g.activation,
            sim: 0.0,
            recency: 0.0,
            strength: 0.0,
            importance: m.importance,
            superseded: false,
            derived: false,
            confidence: None,
            level: None,
            via_assoc: Some(g.via_seed),
            via_edge: Some(g.via_edge),
            basis: m.basis,
            lexical: 0.0,
        });
    }
    Ok(out)
}

fn cmd_search(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut query: Option<String> = None;
    let mut limit: usize = 10;
    let mut json = false;
    let mut reviewed_only = false;
    let mut include_superseded = false;
    let mut touch = false;
    let mut min_score: f32 = 0.0;
    let mut budget: Option<usize> = None;
    let mut project: Option<String> = None;
    let mut cwd: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(10),
            "--json" => json = true,
            "--reviewed-only" => reviewed_only = true,
            "--include-superseded" => include_superseded = true,
            "--touch" => touch = true,
            "--min-score" => min_score = args.next().and_then(|v| v.parse().ok()).unwrap_or(0.0),
            "--budget" => budget = args.next().and_then(|v| v.parse().ok()),
            // The kb socket has carried `project` since the project card
            // shipped; this subprocess path is the recall hook's fallback
            // for when machd is down, and without the flag that fallback
            // exits 1 and the hook injects NOTHING -- strictly worse than
            // the missing card the flag was added to fix.
            "--project" => project = args.next(),
            // Resolved against the `projects` registry (`store::
            // project_for_path`) and, when it matches, wins over
            // `--project` as the session project for both the card and
            // the other-project down-weighting in `search_hits` -- a
            // registered root is an actual claim of identity, `--project`
            // today is just a basename guess.
            "--cwd" => cwd = args.next(),
            "-h" | "--help" => {
                println!(
                    "usage: mach kb search \"<query>\" [--limit N] [--budget N] [--json] [--reviewed-only] [--touch] \
                     [--include-superseded] [--min-score F] [--project NAME] [--cwd PATH]"
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
    // The shared merge (mem hits + insight hits + entity-connections
    // enrichment) lives in `search_hits`, so this is the exact same path
    // the kb socket daemon uses for `kb-recall.sh`'s fast path — only the
    // substring-fallback branch below is unique to this subprocess entry
    // point.
    let mut response = match search_hits(&conn, &embedder, &query, limit, reviewed_only, include_superseded, 0.0, &now, project.as_deref(), cwd.as_deref(), None) {
        Ok(r) => {
            if touch {
                let ids: Vec<i64> = r.hits.iter().filter(|h| !h.derived).map(|h| h.id).collect();
                store::touch(&conn, &ids, &now).map_err(to_io)?;
            }
            r
        }
        Err(e) => {
            eprintln!(
                "mach kb search: warning: {} — falling back to substring match",
                e
            );
            // No insight fallback here: substring match forces every hit to
            // score 1.0 (not a meaningful ranking to begin with), and
            // insights have no independent text-substring path worth adding
            // for what's already a degraded mode. No connections either —
            // an embed failure means there's no query embedding to match an
            // entity against.
            let subs = store::search_substring(&conn, &query, limit, reviewed_only, include_superseded).map_err(to_io)?;
            let ids: Vec<i64> = subs.iter().map(|(m, _)| m.id).collect();
            if touch {
                store::touch(&conn, &ids, &now).map_err(to_io)?;
            }
            let hits = subs
                .into_iter()
                .map(|(m, score)| {
                    let superseded = m.is_superseded();
                    to_hit(RankedHit { memory: m, score, sim: score, recency: 0.0, strength: 0.0, superseded, lexical: 0.0 })
                })
                .collect();
            SearchResponse { hits, connections: Vec::new(), cards: Vec::new(), project_card: None }
        }
    };

    if min_score > 0.0 {
        response.hits.retain(|h| h.score >= min_score);
    }
    // Budget last: packing decides what FITS, and only rows that cleared
    // the floor are worth spending budget on.
    let response = match budget {
        Some(b) => pack_to_budget(response, b),
        None => response,
    };

    if json {
        println!("{}", serde_json::to_string(&response)?);
    } else if response.hits.is_empty() {
        println!("no matches");
    } else {
        let mut any_superseded = false;
        for h in &response.hits {
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
        if response.hits.iter().any(|h| h.derived) {
            println!("\n(D = derived insight from `mach kb reflect` — see `mach kb insights`)");
        }
        if any_superseded {
            println!("(! = superseded — scored ×0.1, shown via --include-superseded)");
        }
    }
    // Human-readable only: with --json the connections are already inside
    // the JSON object above, and trailing text would break every consumer
    // that json-parses stdout (kb-recall.py's subprocess fallback did).
    if !json {
        for c in &response.cards {
            println!(
                "\ncard: {} ({}) — rebuilt {} from {} memories",
                c.entity,
                c.kind.as_deref().unwrap_or("kind unspecified"),
                c.updated,
                c.evidence
            );
            for line in c.text.lines() {
                println!("  {}", line);
            }
        }
    }
    if !json && !response.connections.is_empty() {
        println!("\nconnections:");
        for c in &response.connections {
            match (&c.predicate2, &c.src_name2, &c.dst_name2) {
                (Some(p2), Some(s2), Some(d2)) if s2 == &c.dst_name => println!(
                    "  [connection, 2 hops] {} —{}→ {} —{}→ {}",
                    c.src_name, c.predicate, c.dst_name, p2, d2
                ),
                (Some(p2), Some(s2), Some(d2)) => println!(
                    "  [connection, 2 hops] {} —{}→ {}; {} —{}→ {}",
                    c.src_name, c.predicate, c.dst_name, s2, p2, d2
                ),
                _ => println!("  [connection] {} —{}→ {} (learned {})", c.src_name, c.predicate, c.dst_name, c.evidence_date),
            }
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
    let mut pinned_only = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(20),
            "--superseded" => superseded_only = true,
            "--dormant" => dormant_only = true,
            "--pinned" => pinned_only = true,
            "-h" | "--help" => {
                println!("usage: mach kb list [--limit N] [--superseded] [--dormant] [--pinned]");
                return Ok(());
            }
            other => {
                eprintln!("mach kb list: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let conn: Connection = store::open().map_err(to_io)?;
    let rows = if pinned_only {
        store::list_pinned(&conn, Some(limit)).map_err(to_io)?
    } else if dormant_only {
        store::list_dormant(&conn, Some(limit)).map_err(to_io)?
    } else {
        store::list(&conn, Some(limit), superseded_only).map_err(to_io)?
    };
    if rows.is_empty() {
        println!(
            "{}",
            if pinned_only {
                "no pinned memories"
            } else if dormant_only {
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
        let pin = if m.pinned_at.is_some() { 'p' } else { ' ' };
        println!(
            "{}{}{}{}{:>5}  {}  {:<10} {:<12}  {}",
            flag,
            sup,
            dor,
            pin,
            m.id,
            m.created_at,
            m.source.as_deref().unwrap_or("-"),
            m.project.as_deref().unwrap_or("-"),
            truncate(&m.content, 70)
        );
    }
    println!(
        "\n(* = awaiting review — run `mach kb review`; ! = superseded — run `mach kb list --superseded`; \
         z = dormant — run `mach kb list --dormant`, wake with `mach kb wake <id>`; \
         p = pinned — run `mach kb list --pinned`, clear with `mach kb unpin <id>`)"
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

fn cmd_restore(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let id_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("mach kb restore: missing <id> argument");
            std::process::exit(1);
        }
    };
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mach kb restore: '{}' is not a valid id", id_str);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    if store::restore(&conn, id, &now).map_err(to_io)? {
        println!("restored memory #{} (supersession undone, pinned so reflect won't re-judge it)", id);
        Ok(())
    } else {
        eprintln!(
            "mach kb restore: memory {} is not superseded (see `mach kb list --superseded`)",
            id
        );
        std::process::exit(1);
    }
}

fn cmd_unpin(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let id_str = match args.next() {
        Some(s) => s,
        None => {
            eprintln!("mach kb unpin: missing <id> argument");
            std::process::exit(1);
        }
    };
    let id: i64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mach kb unpin: '{}' is not a valid id", id_str);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    if store::unpin(&conn, id).map_err(to_io)? {
        println!("unpinned memory #{} (automatic passes may tombstone it again)", id);
        Ok(())
    } else {
        eprintln!("mach kb unpin: memory {} is not pinned", id);
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
// Up to this many of the NEWEST unreflected rows are always included in the
// working set, so a fact added this week is never stuck behind a
// four-figure backlog; whatever cap remains is filled with the OLDEST
// unreflected rows -- see `reflect_working_set`'s own doc comment for the
// full split and why it replaces the old watermark-gated "newest 60"
// truncation.
const REFLECT_NEWEST_SLICE: usize = 20;
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
    // Captured before this run's own completion overwrites `last_run_at`
    // below -- the cutoff `store::ids_new_since_reflect` uses to decide
    // which already-reflected rows still count as "new" for the dedupe and
    // contradiction passes (see that function's own doc comment).
    let previous_run_start = state.last_run_at.clone();

    // Step 1: input selection. Replaces the old single-watermark
    // `memories_since(last_id)` -- see `SCHEMA_VERSION`'s v29->v30
    // migration and `reflect::should_mark_reflected` for why: a database-
    // wide watermark that only moved on a zero-failure run froze for two
    // weeks in the live bank while the backlog grew past 1300 unexamined
    // rows. `unreflected_active_count` is the per-memory replacement: cheap
    // (a single COUNT), so it costs nothing extra on this pre-check path.
    let backlog_before = store::unreflected_active_count(&conn).map_err(to_io)?;
    let has_new = backlog_before > 0;

    // Cheap, DB-only pre-check — no network, no `claude` spawn — so an
    // early exit costs nothing on a laptop merely waking up opportunistically
    // (see the timer's OnUnitInactiveSec re-arm). `insights_due_for_verification`
    // returns up to REFLECT_VERIFICATION_SAMPLE active insights
    // unconditionally (it has no staleness filter of its own — see its own
    // doc comment), so "empty" here precisely means zero active insights
    // exist yet, not merely that none happen to be old enough to re-check.
    // Note this early exit is conservative, not exact, for the nightly
    // dedupe/contradiction passes: their own "new" set
    // (`store::ids_new_since_reflect`) can be non-empty even when
    // `has_new` is false here, if the immediately preceding run reflected
    // rows at or after its own `last_run_at` (see that function's doc
    // comment) -- a database with an empty backlog but a just-finished
    // prior run legitimately still has dedupe/contradiction work, so it
    // must not early-exit on `has_new` alone; `verification_queue`/
    // `graph_backlog` below already keep that door open. A --meta-only
    // invocation (e.g. for manual testing once enough insights already
    // exist) must still be able to run even when
    // there's nothing new or due — this is the only early exit before the
    // connectivity guard below.
    let verification_queue = store::insights_due_for_verification(&conn, REFLECT_VERIFICATION_SAMPLE).map_err(to_io)?;
    // Same "must still be able to run" reasoning extends to the graph-
    // extraction pass's own backlog (`store::has_graph_extraction_candidates`)
    // — a database with nothing new, no insight due for verification, and no
    // unprocessed memory must not early-exit before that pass ever gets a
    // look, since (like curation/dormancy) it runs unconditionally below.
    let graph_backlog = store::has_graph_extraction_candidates(&conn).map_err(to_io)?;
    // Hebbian edges forget on their own curve; dropping the ones that have
    // faded is DB-only and belongs before every exit, including "nothing new".
    let pruned_assoc = store::prune_assoc(&conn, &now).map_err(to_io)?;
    if pruned_assoc > 0 {
        println!("mach kb reflect: pruned {} faded memory associations", pruned_assoc);
    }

    // Retention: age out `judge_log` (raw LLM-call audit trail) and
    // `recall_engagement` (per-session shown/engaged verdicts) rows past
    // their retention window. DB-only, unconditional (like `prune_assoc`
    // just above) — this runs before BOTH early exits below ("nothing
    // new" and "offline, deferring"), so a quiet stretch with nothing new
    // to examine still gets pruned rather than silently skipping
    // retention indefinitely. Never touches `supersession_audit`, which
    // has no retention and is never pruned. The counts are folded into
    // the full run's summary line at the end of this function; an early
    // exit below prints its own short message and never reaches that
    // summary, so nothing extra is printed for these counts on that path.
    let judge_log_cutoff =
        store::now_rfc3339_from_secs(store::now_secs().saturating_sub(store::JUDGE_LOG_RETENTION_DAYS as u64 * 86400));
    let judge_log_pruned = store::prune_judge_log(&conn, &judge_log_cutoff).map_err(to_io)?;
    let recall_engagement_cutoff = store::now_rfc3339_from_secs(
        store::now_secs().saturating_sub(store::RECALL_ENGAGEMENT_RETENTION_DAYS as u64 * 86400),
    );
    let recall_engagement_pruned = store::prune_recall_engagement(&conn, &recall_engagement_cutoff).map_err(to_io)?;

    if !has_new && !force_meta && verification_queue.is_empty() && !graph_backlog {
        // Still a genuine completion for `mach kb health`'s purposes — the
        // process ran and had nothing to do, which is different from never
        // running at all (see `store::ReflectState::last_completed_at`'s
        // own doc comment). The offline-defer exit just below this one is
        // the one case that deliberately does NOT call this.
        store::mark_reflect_completed(&conn, &now).map_err(to_io)?;
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
    // `mach kb reflect` is the one caller that opts into the transient-
    // failure retry (`ProcessReflectLlm::with_retry`'s own doc comment) —
    // it runs opportunistically every ~3h, not on an interactive or
    // tighter-timer cadence, so the extra `retry_delay` per failed call is
    // cheap here and expensive everywhere else `ProcessReflectLlm::new()`
    // is used (`ask`, `cards`, `graph audit`, `audit-supersessions`,
    // `ingest-sessions`, `eval-ask` all stay off by default).
    let llm = ProcessReflectLlm::new().with_retry(true);
    // Set on any `claude` call failing mid-run (a network drop after the
    // connectivity guard above passed, a spawn error, a timeout) — reported
    // in the summary line's "(degraded: ...)" suffix. No longer gates
    // anything: each pass isolates and retries its own failures on its own
    // progress mechanism, and only stage-1/stage-2's OWN failure (folded in
    // just below via `stage.stage_failed`, NOT `stage.any_failed` — see
    // `InsightStageOutcome::stage_failed`'s own doc comment) decides
    // whether the insight stage's working set gets marked reflected — see
    // `reflect::should_mark_reflected`.
    let mut llm_failed = false;

    // Steps 2-4 (stage-1 questions, stage-2 insight synthesis, insight
    // re-verification) plus marking the working set reflected all live in
    // `run_insight_stage` now, so the exact same logic is exercised by unit
    // tests without spawning `store::open()`'s real db or a real `claude`
    // process — see that function's own doc comment for why only stage-1
    // and stage-2 (not re-verification) gate `reflected_at`.
    let insight_llm = reflect::LoggedLlm::new(&conn, "insights", &llm);
    let stage = run_insight_stage(&conn, &embedder, &insight_llm, &verification_queue, &now).map_err(to_io)?;
    let examined = stage.examined;
    let questions_count = stage.questions;
    let insights_added = stage.insights_added;
    let reinforced = stage.reinforced;
    let flagged = stage.flagged;
    let weakened = stage.weakened;
    let verified = stage.verified;
    let revised = stage.revised;
    let reflected = stage.reflected;
    let max_reflected_id = stage.max_reflected_id;
    // `any_failed`, not `stage_failed`: the summary line's "(degraded: ...)"
    // suffix should reflect a verification failure too, even though that
    // failure never blocked marking the working set reflected.
    llm_failed = llm_failed || stage.any_failed;

    // Step 4.5: nightly dedupe — fast capture paths (`mach note`, digests)
    // deliberately skip save-time dedupe classification, so near-duplicate
    // raw memories accumulate; this catches them here, where LLM time is
    // free. Only pairs touching a memory "new" by `store::ids_new_since_reflect`
    // are considered, capped at reflect::DEDUPE_MAX_PAIRS_PER_RUN. That set
    // replaces the old watermark-derived `new_memories.map(|m| m.id)`: it's
    // now unreflected backlog (`reflected_at IS NULL`) unioned with rows
    // reflected at or after `previous_run_start` (this run's own working
    // set, marked moments ago by `run_insight_stage`, plus anything the
    // immediately preceding run reflected) — see that function's own doc
    // comment. Deliberately runs BEFORE dormancy (below): a memory
    // old/stale/uncited enough to qualify for dormancy this very run is
    // exactly the kind of long-neglected fact a dedupe or contradiction
    // pair most needs to catch — dormancy would otherwise drop it out of
    // `active_memories_for_dormancy`'s pool (dormant rows are excluded from
    // it) before either judge ever got a look at it.
    let new_ids = store::ids_new_since_reflect(&conn, previous_run_start.as_deref()).map_err(to_io)?;
    let (deduped, dedupe_llm_failed) =
        run_dedupe_pass(&conn, &reflect::LoggedLlm::new(&conn, "dedupe", &llm), &new_ids, &now).map_err(to_io)?;
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
    let (contradictions, contradiction_llm_failed) =
        run_contradiction_pass(&conn, &reflect::LoggedLlm::new(&conn, "contradiction", &llm), &new_ids, &now)
            .map_err(to_io)?;
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
    let (curated, promoted, demoted, curation_llm_failed) =
        run_curation_pass(&conn, &reflect::LoggedLlm::new(&conn, "curation", &llm), &now).map_err(to_io)?;
    llm_failed = llm_failed || curation_llm_failed;

    // Step 4.6: strength review — extends re-verification (step 4, above)
    // from insights to raw memories themselves: samples up to
    // REFLECT_STRENGTH_SAMPLE oldest-verified ACTIVE memories at importance
    // >= STRENGTH_MIN_IMPORTANCE and checks each against its own top-3
    // semantic neighbors, routing any genuine conflict through the same
    // judge the contradiction patrol above uses. Also runs before dormancy
    // for the same reason.
    let (mem_verified, mem_stale, mem_routed, strength_llm_failed) =
        run_strength_review_pass(&conn, &reflect::LoggedLlm::new(&conn, "strength_review", &llm), &now).map_err(to_io)?;
    llm_failed = llm_failed || strength_llm_failed;

    // Step 4.63: graph extraction — entities/relations derived from facts
    // that have now been through this run's own truth-maintenance (the
    // contradiction patrol and strength review above), so extraction reads
    // already-resolved facts rather than a stale one a later pass this same
    // run might still have tombstoned. Runs every invocation regardless of
    // has_new, same as curation/dormancy — it drains the not-yet-extracted
    // backlog (`store::graph_extraction_candidates`), not just this run's
    // new material. Deliberately before dormancy: a memory dormancy is about
    // to put to sleep this very run is still exactly the kind of fact worth
    // extracting a durable edge from before it drops out of active view.
    let (graph_examined, graph_edges, graph_entities, graph_llm_failed) =
        run_graph_extraction_pass(&conn, &embedder, &reflect::LoggedLlm::new(&conn, "graph_extraction", &llm), &now)
            .map_err(to_io)?;
    llm_failed = llm_failed || graph_llm_failed;

    // Step 4.64: graph hygiene — runs right after edge extraction and before
    // dormancy: (a) evidence-death propagation (deterministic, no LLM) —
    // an active edge whose evidence memory has since died (invalidated or
    // hard-deleted) is invalidated too, `superseded_by` left NULL (it died
    // with its evidence, it wasn't beaten by a rival edge); a dormant
    // evidence memory is deliberately left alone here (it can wake) and is
    // instead excluded only at recall-query time (see
    // `active_relations_for_recall`). (b) entity merge (batched LLM
    // confirm) — candidate pairs found by name-embedding similarity or a
    // case/punctuation-insensitive exact match are batch-judged SAME/
    // DIFFERENT; a SAME verdict keeps the older entity, repoints every edge
    // off the newer one (deduping any resulting identical edges), and
    // deletes the newer entity row. Runs every invocation regardless of
    // has_new, same as curation/dormancy/graph-extraction — both halves are
    // sweeps over the whole graph, not gated on this run's new material.
    let (evidence_dead, entities_merged, hygiene_llm_failed) =
        run_graph_hygiene_pass(&conn, &reflect::LoggedLlm::new(&conn, "graph_hygiene", &llm), &now).map_err(to_io)?;
    llm_failed = llm_failed || hygiene_llm_failed;

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
    let (dormant, consolidated, dormancy_llm_failed) =
        run_dormancy_pass(&conn, &embedder, &reflect::LoggedLlm::new(&conn, "dormancy", &llm), &now).map_err(to_io)?;
    llm_failed = llm_failed || dormancy_llm_failed;

    // Step 4.655: insight dedupe — fold beliefs that say the same thing into
    // one row before the card and theme passes read them, so a theme is
    // never derived from four rephrasings of one insight.
    let (insight_pairs_judged, insights_merged, insight_dedupe_failed) =
        run_insight_dedupe_pass(&conn, &reflect::LoggedLlm::new(&conn, "insight_dedupe", &llm), &now).map_err(to_io)?;
    llm_failed = llm_failed || insight_dedupe_failed;

    // Step 4.66: entity cards — rebuild the consolidated profile of every
    // entity whose evidence has moved since its card was written. Runs after
    // merges (so a card is never built for an entity about to be merged
    // away) and after dormancy (so it reflects what actually survived), and
    // like those, every invocation regardless of has_new: staleness is
    // measured against the mention watermark, not this run's new material.
    let (cards_examined, cards_built, cards_error) =
        run_entity_card_pass(&conn, &embedder, &reflect::LoggedLlm::new(&conn, "entity_cards", &llm), &now)
            .map_err(to_io)?;
    if let Some(e) = &cards_error {
        eprintln!("mach kb reflect: entity card call failed: {}", e);
    }
    llm_failed = llm_failed || cards_error.is_some();

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
        let (added, meta_llm_failed) =
            run_meta_pass(&conn, &embedder, &reflect::LoggedLlm::new(&conn, "meta", &llm)).map_err(to_io)?;
        themes_added = added;
        llm_failed = llm_failed || meta_llm_failed;
    }

    // Step 6: `reflect_state.last_run_at` now updates every run
    // unconditionally (it's `store::ids_new_since_reflect`'s own cutoff for
    // the *next* run, not a gate on anything) and `last_memory_id` simply
    // tracks the highest id `run_insight_stage` actually marked reflected
    // this run — status/display only (`mach kb health`), nothing reads it
    // to decide what to examine any more. Monotonic: never regresses below
    // whatever it already was, since `run_insight_stage` returning
    // `max_reflected_id: None` (nothing marked, whether because there was
    // no backlog or the insight stage itself failed) simply leaves it alone.
    let new_last_memory_id = match (state.last_memory_id, max_reflected_id) {
        (Some(old), Some(new)) => Some(old.max(new)),
        (None, Some(new)) => Some(new),
        (old, None) => old,
    };
    store::update_reflect_state(&conn, &now, new_last_memory_id).map_err(to_io)?;

    // Records this run's completion for `mach kb health`'s own "is reflect
    // still running" check — unconditional, same as the `last_run_at`
    // update just above (see `store::ReflectState::last_completed_at`'s own
    // doc comment).
    store::mark_reflect_completed(&conn, &now).map_err(to_io)?;

    let backlog_after = store::unreflected_active_count(&conn).map_err(to_io)?;

    println!(
        "mach kb reflect: examined={} questions={} insights_added={} reinforced={} \
         reflected={} backlog={} \
         themes_added={} flagged={} weakened={} verified={} revised={} curated={} promoted={} demoted={} dormant={} \
         consolidated={} deduped={} contradictions={} mem_verified={} mem_stale={} mem_routed={} \
         graph_examined={} graph_edges={} graph_entities={} evidence_dead={} entities_merged={} \
         cards_examined={} cards_built={} insight_pairs={} insights_merged={} \
         judge_log_pruned={} recall_engagement_pruned={}{}",
        examined,
        questions_count,
        insights_added,
        reinforced,
        reflected,
        backlog_after,
        themes_added,
        flagged,
        weakened,
        verified,
        revised,
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
        graph_examined,
        graph_edges,
        graph_entities,
        evidence_dead,
        entities_merged,
        cards_examined,
        cards_built,
        insight_pairs_judged,
        insights_merged,
        judge_log_pruned,
        recall_engagement_pruned,
        if llm_failed { " (degraded: some claude calls failed this run)" } else { "" }
    );
    Ok(())
}

/// Selects `mach kb reflect`'s insight-stage working set: up to
/// `REFLECT_WORKING_SET_CAP` (60) memories still due for the insight stage
/// (`reflected_at IS NULL`, active), split as up to `newest_slice` (20) of
/// the NEWEST unreflected rows plus the OLDEST unreflected rows filling
/// whatever cap remains (40 when the newest slice is full, more when the
/// backlog is smaller than `newest_slice`). Deterministic and duplicate-
/// free: the newest slice is chosen first, and the oldest slice explicitly
/// excludes those ids (`store::unreflected_active_oldest`'s own `exclude`
/// parameter) rather than deduping two independently-run queries after the
/// fact. Returned oldest-id-first, matching the ordering every downstream
/// prompt-building/evidence path already expects.
///
/// This replaces the pre-migration selection this codebase used to run:
/// every active memory created since `reflect_state.last_memory_id`,
/// bridged with each one's top-3 semantic neighbors, then truncated to the
/// newest 60 when over cap. That truncation quietly discarded anything
/// older whenever the bridged set overflowed, so the same newest-60 window
/// got re-examined run after run while a watermark that only ever advanced
/// on a zero-failure run sat frozen for two weeks — the backlog could never
/// shrink even in principle. The newest slice here preserves the one
/// property worth keeping (a fact added this week is never stuck behind a
/// four-figure backlog); the oldest slice is the actual fix, draining the
/// backlog by a bounded amount every single run regardless of what any
/// other pass in that run does.
///
/// Excludes index-owned rows (`source` starting with
/// `store::INDEX_OWNED_SOURCE_PREFIX` or `store::CODE_HISTORY_SOURCE_PREFIX`,
/// i.e. `code-index:...`/`code-history:...`) even though `upsert_index_memory`
/// already sets `reflected_at` at insert time, which should keep them out of
/// `unreflected_active_newest`/`_oldest` on its own -- this is a second,
/// source-based line of defense so the insight stage never re-examines one
/// of these, even if `reflected_at` ever ended up NULL some other way (a
/// pre-phase-2 row, a bug, a manual edit). They are derived summaries, not
/// session facts to reason about.
fn reflect_working_set(conn: &Connection, cap: usize, newest_slice: usize) -> Result<Vec<Memory>, KbError> {
    let not_index_owned = |m: &Memory| !store::is_index_owned(m);
    let newest: Vec<Memory> =
        store::unreflected_active_newest(conn, newest_slice.min(cap))?.into_iter().filter(not_index_owned).collect();
    let newest_ids: HashSet<i64> = newest.iter().map(|m| m.id).collect();
    let remaining = cap.saturating_sub(newest.len());
    let oldest: Vec<Memory> =
        store::unreflected_active_oldest(conn, remaining, &newest_ids)?.into_iter().filter(not_index_owned).collect();
    let mut out = oldest;
    out.extend(newest);
    out.sort_by_key(|m| m.id);
    Ok(out)
}

/// What `run_insight_stage` did this run — everything `cmd_reflect`'s
/// summary line reports about the insight stage, plus what it needs to
/// update `reflect_state.last_memory_id` and decide whether to fold this
/// stage's own failure into the run-wide `llm_failed` flag.
struct InsightStageOutcome {
    examined: usize,
    questions: usize,
    insights_added: usize,
    reinforced: usize,
    flagged: usize,
    weakened: usize,
    verified: usize,
    /// Contradicted insights the revise/drop/keep judge corrected in place
    /// (`store::revise_insight`) instead of flagging — see that call site's
    /// own comment for the three-way verdict.
    revised: usize,
    /// Rows actually marked `reflected_at = now` this run — 0 whenever
    /// `stage_failed` is true or the working set was empty.
    reflected: usize,
    /// `MAX(id)` among the rows marked reflected this run, or `None` when
    /// nothing was marked.
    max_reflected_id: Option<i64>,
    /// Whether stage-1 (questions) or stage-2 (insight synthesis) itself
    /// failed this run — the ONLY thing `reflect::should_mark_reflected`
    /// looks at. Re-verification's own failures never set this: it keeps
    /// its OWN progress mechanism (`Insight::last_verified_at`, untouched
    /// on a failed check — see the verification loop below), so a
    /// verification call failing has no bearing on whether the memory
    /// working set gets marked reflected. A failure in any pass this
    /// function doesn't run at all (dedupe, contradiction, curation,
    /// strength review, graph extraction, hygiene, dormancy, insight
    /// dedupe, entity cards, meta) obviously has no bearing on it either.
    /// The gating decision is already made inside this function (see
    /// "Step 6a" below) before this struct is even built, so `cmd_reflect`
    /// itself never needs to read this field back out — it exists on the
    /// struct purely so the failure-isolation behavior is directly
    /// assertable from a unit test rather than only inferable from
    /// `reflected`/`unreflected_active_count`.
    #[allow(dead_code)]
    stage_failed: bool,
    /// `stage_failed` OR a re-verification call failing this run — folded
    /// into `cmd_reflect`'s run-wide `llm_failed` flag for the summary
    /// line's "(degraded: ...)" reporting only. Never used to gate
    /// anything; `reflect::should_mark_reflected` never sees this field.
    any_failed: bool,
}

/// Hard cap on the stage-1 questions prompt (`reflect::build_questions_prompt`
/// over the working set), in `char`s. A deterministic slab of unusually
/// large memories occupying the oldest slice could otherwise make stage-1's
/// own prompt too large for `claude` to answer reliably within its own
/// timeout, every single run — re-freezing exactly the insight stage this
/// whole per-memory-progress change exists to unstick, just via prompt
/// size instead of the old watermark. Enforced by `cap_stage1_prompt`.
const REFLECT_STAGE1_PROMPT_CHAR_CAP: usize = 20_000;

/// Shrinks `working_vec` (already sorted ascending by id, as
/// `reflect_working_set` returns it) so `reflect::build_questions_prompt`
/// over it fits `char_cap` chars — by dropping rows from the OLDEST
/// slice's own end (the highest ids within that slice, i.e. the
/// "least-old" members of it) until it fits, or the oldest slice is empty.
/// The up-to-`newest_slice` newest rows (the tail of `working_vec`, by
/// construction — see `reflect_working_set`'s doc comment for why every
/// oldest-slice id is guaranteed lower than every newest-slice id) are
/// ALWAYS kept in full and never trimmed, even if the prompt is still over
/// cap afterward. A row dropped this way is simply not part of this run's
/// working set at all — it's never marked reflected, and stays backlog for
/// a later run (as either an oldest or, eventually, a newest candidate
/// again).
fn cap_stage1_prompt(mut working_vec: Vec<Memory>, newest_slice: usize, char_cap: usize) -> Vec<Memory> {
    let newest_count = newest_slice.min(working_vec.len());
    let split_at = working_vec.len() - newest_count;
    if split_at == 0 {
        return working_vec; // nothing but the newest slice -- never trimmed
    }
    let newest_part: Vec<Memory> = working_vec.split_off(split_at);
    let mut oldest_part = working_vec; // now just the first split_at elements
    let prompt_chars = |oldest: &[Memory], newest: &[Memory]| -> usize {
        let lines: Vec<(i64, String)> =
            oldest.iter().chain(newest.iter()).map(|m| (m.id, m.content.clone())).collect();
        reflect::build_questions_prompt(&lines).chars().count()
    };
    while prompt_chars(&oldest_part, &newest_part) > char_cap && !oldest_part.is_empty() {
        oldest_part.pop();
    }
    oldest_part.extend(newest_part);
    oldest_part
}

/// The insight stage: `mach kb reflect`'s steps 2-4 (stage-1 salient
/// questions, stage-2 durable-insight synthesis, and insight
/// re-verification), plus marking its own working set reflected
/// (`store::mark_memories_reflected`) when — and only when — stage-1 and
/// stage-2 came through this run with no LLM failure
/// (`reflect::should_mark_reflected`; see `InsightStageOutcome::stage_failed`'s
/// own doc comment for why re-verification is deliberately excluded from
/// that gate despite living in this same function). Extracted from
/// `cmd_reflect` into its own generic-over-`ReflectLlm`/`Embedder`
/// function, same pattern as `run_dedupe_pass`/`run_graph_extraction_pass`/
/// etc. below, so it's exercised by unit tests against a fake LLM and an
/// in-memory store instead of only being reachable through a real
/// `claude -p` run.
///
/// `verification_queue` is the caller's already-fetched
/// `insights_due_for_verification` sample (re-verification's own working
/// set is insights, not memories, and is independent of the memory working
/// set `reflect_working_set` selects here — see `cli::cmd_reflect`'s "Step
/// 1" for why both need to be computed before the early-exit check, which
/// is why the caller still fetches it rather than this function).
fn run_insight_stage<E: Embedder, L: ReflectLlm>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    verification_queue: &[Insight],
    now: &str,
) -> Result<InsightStageOutcome, KbError> {
    let mut stage_failed = false;
    let mut questions_count = 0usize;
    let mut insights_added = 0usize;
    let mut reinforced = 0usize;

    let working_vec = reflect_working_set(conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE)?;
    // Cap the stage-1 prompt size before anything else touches `working_vec`
    // — `examined`, the ids eventually marked reflected, and the prompt
    // itself must all agree on exactly the same (possibly shrunk) set. See
    // `cap_stage1_prompt`'s own doc comment for the freeze risk this
    // guards against.
    let working_vec = cap_stage1_prompt(working_vec, REFLECT_NEWEST_SLICE, REFLECT_STAGE1_PROMPT_CHAR_CAP);
    let examined = working_vec.len();

    if !working_vec.is_empty() {
        // Step 2 (stage 1): salient questions, one haiku call over the
        // whole working set. TIMEOUT_HAIKU_BATCH (60s), not the shorter
        // single-fact TIMEOUT_HAIKU (30s): this call already covers up to
        // REFLECT_WORKING_SET_CAP (60) memories at once (the same scale
        // `TIMEOUT_HAIKU_BATCH` was raised for elsewhere), and the 20,000-
        // char prompt cap above still bounds it even so.
        let question_lines: Vec<(i64, String)> = working_vec.iter().map(|m| (m.id, m.content.clone())).collect();
        let q_prompt = reflect::build_questions_prompt(&question_lines);
        let questions: Vec<String> = match llm.call("haiku", &q_prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => reflect::parse_questions(&out),
            Err(_) => {
                stage_failed = true;
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
                Err(_) => {
                    // An embedding-provider failure here is a real failure
                    // of this stage's own machinery, not "insufficient
                    // evidence" — must set stage_failed (unlike the
                    // evidence.len() < 2 case just below, which is a
                    // legitimate, non-failing outcome).
                    stage_failed = true;
                    continue;
                }
            };
            let evidence = match store::top_similar_active(conn, &q_emb, REFLECT_EVIDENCE_PER_QUESTION) {
                Ok(v) => v,
                Err(_) => {
                    stage_failed = true;
                    continue;
                }
            };
            if evidence.len() < 2 {
                // Can never clear the 2-citation floor regardless of what
                // the model says — skip the sonnet call entirely. Not a
                // failure: the query succeeded and legitimately found too
                // little evidence.
                continue;
            }
            let existing_near =
                store::top_similar_insights(conn, &q_emb, REFLECT_EXISTING_INSIGHTS_CONTEXT).unwrap_or_default();

            let evidence_pairs: Vec<(i64, String)> = evidence.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
            let existing_pairs: Vec<(i64, String)> =
                existing_near.iter().map(|(ins, _)| (ins.id, ins.text.clone())).collect();
            let prompt = reflect::build_insight_prompt(question, &evidence_pairs, &existing_pairs);

            let raw = match llm.call("sonnet", &prompt, TIMEOUT_SONNET) {
                Ok(out) => out,
                Err(_) => {
                    stage_failed = true;
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
                    if store::insert_insight(conn, &text, confidence, &source_ids, text_embedding.as_deref()).is_ok() {
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
                    match store::get_insight(conn, insight_id) {
                        Ok(Some(target)) if target.is_active() => {
                            let existing_raw: HashSet<i64> =
                                target.source_ids.iter().filter_map(|s| s.parse::<i64>().ok()).collect();
                            let new_ids: Vec<i64> =
                                valid_ids.into_iter().filter(|id| !existing_raw.contains(id)).collect();
                            if new_ids.is_empty() {
                                continue; // only already-cited ids — reject
                            }
                            if store::reinforce_insight(conn, insight_id, &new_ids, now).is_ok() {
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

    // Step 4: re-verification, same run — up to REFLECT_VERIFICATION_SAMPLE
    // oldest insights by last_verified_at (never-verified first; flagging
    // now also bumps last_verified_at -- see store::flag_insight's own doc
    // comment -- so a contradicted insight leaves the head of this queue
    // instead of crowding it out run after run). Never deletes -- flags,
    // revises, or (broken citation) weakens. Runs across BOTH levels
    // (level-2 themes share this table and this query), so a theme is just
    // as due for a check as a plain insight. Independent of the memory
    // working set above (it can run, and fail, even when that working set
    // was empty). Deliberately does NOT set `stage_failed`: verification
    // tracks its own progress via `Insight::last_verified_at`, exactly like
    // every other pass below this function (dedupe, contradiction,
    // curation, ...) tracks its own — a verification call failing here
    // only means that specific insight stays due and gets retried, same as
    // any of those; it must never also hold the memory working set
    // hostage. Its failure still feeds `any_failed` (this function's own
    // aggregate, folded by the caller into `llm_failed` for the summary
    // line) so it's visible in the "(degraded: ...)" report.
    let mut any_failed = stage_failed;
    let mut flagged = 0usize;
    let mut weakened = 0usize;
    let mut verified = 0usize;
    let mut revised = 0usize;
    for insight in verification_queue {
        let mut broken_citation = false;
        for sid in &insight.source_ids {
            if let Some(rest) = sid.strip_prefix('i').or_else(|| sid.strip_prefix('I')) {
                // An `i<id>` citation — most commonly a theme citing one of
                // its level-1 insights. Now actually resolved and checked:
                // if the cited insight is gone, invalidated, or flagged,
                // the citing row (the theme) is flagged too — a theme is
                // only as sound as the insights it names.
                let ok = match rest.parse::<i64>() {
                    Ok(cid) => {
                        matches!(store::get_insight(conn, cid), Ok(Some(ci)) if ci.is_active() && !ci.is_flagged())
                    }
                    Err(_) => false,
                };
                if !ok {
                    broken_citation = true;
                    break;
                }
                continue;
            }
            let still_active = match sid.parse::<i64>() {
                Ok(mid) => matches!(store::get(conn, mid), Ok(Some(m)) if m.invalidated_at.is_none()),
                Err(_) => false,
            };
            if !still_active {
                broken_citation = true;
                break;
            }
        }

        if broken_citation {
            // A citation that died under it is thinning evidence, not a
            // contradiction: weaken by one step and let the next pass judge
            // the claim itself, rather than flagging it as doubted outright.
            store::weaken_insight(conn, insight.id, now)?;
            weakened += 1;
            continue;
        }

        let mut call_failed = false;
        let mut evidence_pairs: Vec<(i64, String)> = Vec::new();
        let contradicted = match embedder.embed(&insight.text) {
            Ok(emb) => match store::top_similar_active(conn, &emb, REFLECT_VERIFICATION_CANDIDATES) {
                Ok(candidates) if !candidates.is_empty() => {
                    let pairs: Vec<(i64, String)> =
                        candidates.iter().map(|(m, _)| (m.id, m.content.clone())).collect();
                    let prompt = reflect::build_contradiction_prompt(&insight.text, &pairs);
                    let verdict = match llm.call("haiku", &prompt, TIMEOUT_HAIKU) {
                        Ok(out) => reflect::parse_contradiction(&out).is_some(),
                        Err(_) => {
                            call_failed = true;
                            false
                        }
                    };
                    evidence_pairs = pairs;
                    verdict
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
            // Feeds `any_failed` only, never `stage_failed` -- see this
            // function's own "Step 4" comment above.
            any_failed = true;
            continue;
        }

        if contradicted && insight.level == 2 {
            // Themes never get revised (fix-list item 2): a level-2 theme
            // is a meta-reflection over other insights' TEXT, not a claim
            // with its own raw-memory evidence to re-word against, so the
            // revise/drop/keep judge is never even called for one -- same
            // pre-existing flag behavior a contradicted theme always had.
            store::flag_insight(conn, insight.id, now)?;
            flagged += 1;
        } else if contradicted {
            // Evidence has turned against this insight -- rather than
            // unconditionally flagging it (the old behavior), ask a second,
            // sonnet-tier judge whether the CURRENT evidence supports a
            // corrected wording instead of just doubting the original one.
            // Same `llm` (already tagged "insights" by the caller) as
            // stage-1/stage-2/the contradiction check above -- see
            // `reflect::build_revise_prompt`/`parse_revise`'s own doc
            // comments for the three-way verdict this drives.
            let evidence_ids: Vec<i64> = evidence_pairs.iter().map(|(id, _)| *id).collect();
            let revise_prompt = reflect::build_revise_prompt(&insight.text, &evidence_pairs);
            match llm.call("sonnet", &revise_prompt, TIMEOUT_SONNET) {
                Ok(out) => match reflect::parse_revise(&out, &evidence_ids) {
                    Some(reflect::ReviseVerdict::Revise(new_text, cited_ids)) => {
                        // Anti flip-flop (fix-list item 3): a REVISE this
                        // soon after the insight's own last revision is
                        // applied as DROP instead -- two judges disagreeing
                        // about the wording inside two weeks is a sign the
                        // belief itself is unstable, not that the newest
                        // wording is right.
                        let recently_revised = insight
                            .revised_at
                            .as_deref()
                            .and_then(store::parse_rfc3339)
                            .zip(store::parse_rfc3339(now))
                            .map(|(revised_secs, now_secs)| store::days_between(now_secs, revised_secs) < 14.0)
                            .unwrap_or(false);
                        if recently_revised {
                            store::flag_insight(conn, insight.id, now)?;
                            flagged += 1;
                        } else {
                            // Fix-list item 5: an embed failure here skips
                            // the revise entirely (the insight is left
                            // completely unchanged, so it stays due for a
                            // fresh check next run) rather than applying a
                            // text change with a stale or missing
                            // embedding -- only visible via `any_failed`.
                            match embedder.embed(&new_text) {
                                Ok(new_embedding) => {
                                    store::revise_insight(
                                        conn,
                                        insight.id,
                                        &new_text,
                                        Some(&new_embedding),
                                        &cited_ids,
                                        now,
                                    )?;
                                    revised += 1;
                                }
                                Err(_) => {
                                    any_failed = true;
                                }
                            }
                        }
                    }
                    Some(reflect::ReviseVerdict::Drop) => {
                        // No revision the evidence supports (or a REVISE
                        // that failed the citation floor -- see
                        // `reflect::parse_revise`) -- same handling as the
                        // pre-existing "contradicted" outcome: flags AND
                        // lowers confidence by two steps (see
                        // `store::flag_insight`).
                        store::flag_insight(conn, insight.id, now)?;
                        flagged += 1;
                    }
                    Some(reflect::ReviseVerdict::Keep) => {
                        // The second judge disagrees with the first: the
                        // original wording still holds. Treated as a clean
                        // verification, not a flag.
                        store::mark_insight_verified(conn, insight.id, now)?;
                        verified += 1;
                    }
                    None => {
                        // Unparseable reply -- never silently revise text
                        // the model didn't actually provide. Falls back to
                        // the pre-existing contradicted-insight behavior.
                        store::flag_insight(conn, insight.id, now)?;
                        flagged += 1;
                        any_failed = true;
                    }
                },
                Err(_) => {
                    // The revise/drop/keep call itself failed -- same
                    // fallback as an unparseable reply: flag rather than
                    // leave the insight's already-established contradiction
                    // unresolved.
                    store::flag_insight(conn, insight.id, now)?;
                    flagged += 1;
                    any_failed = true;
                }
            }
        } else {
            store::mark_insight_verified(conn, insight.id, now)?;
            verified += 1;
        }
    }

    // Step "6a": mark the working set reflected -- only when stage-1 and
    // stage-2 themselves (NOT re-verification -- see `stage_failed`'s own
    // doc comment) came through clean. A failure in any later pass (dedupe
    // onward) never reaches this decision at all; the caller folds
    // `any_failed` into the run-wide `llm_failed` flag separately, for
    // reporting only.
    let working_ids: Vec<i64> = working_vec.iter().map(|m| m.id).collect();
    let (reflected, max_reflected_id) = if reflect::should_mark_reflected(!working_vec.is_empty(), stage_failed) {
        let n = store::mark_memories_reflected(conn, &working_ids, now)?;
        (n, working_ids.iter().max().copied())
    } else {
        (0, None)
    };

    Ok(InsightStageOutcome {
        examined,
        questions: questions_count,
        insights_added,
        reinforced,
        flagged,
        weakened,
        verified,
        revised,
        reflected,
        max_reflected_id,
        stage_failed,
        any_failed,
    })
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
/// (as before) and the caller is told via the third return value, folded
/// into the run's summary line's "(degraded: ...)" reporting; it has no
/// bearing on whether the insight stage's own working set gets marked
/// reflected (see `reflect::should_mark_reflected`).
fn run_dormancy_pass<L: ReflectLlm>(
    conn: &Connection,
    embedder: &OllamaEmbedder,
    llm: &L,
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
fn run_meta_pass<L: ReflectLlm>(conn: &Connection, embedder: &OllamaEmbedder, llm: &L) -> Result<(usize, bool), KbError> {
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
///   re-points any insight citing the loser to the winner — unless the
///   loser is pinned (`mach kb restore` set that) or `store::supersession_guard`
///   blocks it (a dated loser, or one the winner doesn't lexically carry —
///   see that function's own doc comment), in which case the merge is
///   skipped and the pair is recorded in `dedupe_seen` instead, exactly
///   like `Distinct`.
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
    // Index-owned rows are regenerated and replaced only by the indexer:
    // never a candidate on either side of an automatic supersession.
    let items: Vec<(i64, Vec<f32>)> = pool
        .iter()
        .filter(|m| !store::is_index_owned(m))
        .filter_map(|m| m.embedding.clone().map(|e| (m.id, e)))
        .collect();
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
                let (winner_mem, loser_mem) = if winner_id == mem_a.id { (&mem_a, &mem_b) } else { (&mem_b, &mem_a) };
                if store::is_pinned(conn, loser_id)? {
                    // Fail safe: a human restored the loser, which pins
                    // it, so an automatic merge must never tombstone it —
                    // record the pair as judged instead, same as Distinct,
                    // so it isn't re-asked every run.
                    eprintln!("mach kb: supersession skipped (pinned) #{} -> #{}", loser_id, winner_id);
                    store::mark_dedupe_seen(conn, id_a, id_b)?;
                } else if let Some(reason) = store::supersession_guard(conn, loser_mem, winner_mem, store::GuardKind::Dedupe) {
                    // Deterministic backstop: a dated loser, or one the
                    // winner doesn't lexically carry, is never merged away
                    // on the judge's KEEP alone — recorded as judged, same
                    // as Distinct, so it isn't re-asked every run.
                    eprintln!("mach kb: supersession blocked ({}) #{} -> #{}", reason, loser_id, winner_id);
                    store::mark_dedupe_seen(conn, id_a, id_b)?;
                } else if store::merge_and_supersede(conn, loser_id, winner_id, now)? {
                    store::repoint_insight_citations(conn, loser_id, winner_id, 1)?;
                    deduped += 1;
                }
                // Never recorded in dedupe_seen for an actual merge (or a
                // no-op merge where the loser was already gone): a
                // successful merge drops the loser out of the active pool
                // (it can't resurface as a pair), so no record is needed.
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
    // Index-owned rows are regenerated and replaced only by the indexer:
    // never a candidate on either side of an automatic supersession.
    let items: Vec<(i64, Vec<f32>)> = pool
        .iter()
        .filter(|m| !store::is_index_owned(m))
        .filter_map(|m| m.embedding.clone().map(|e| (m.id, e)))
        .collect();
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
///   human review, not a silent citation swap. If the loser is pinned
///   (`mach kb restore` set that) or `store::supersession_guard` blocks it
///   (a dated loser — see that function's own doc comment; the coverage
///   check does not run for contradictions), the supersede is skipped
///   entirely and the pair is recorded in `contradiction_seen` instead,
///   exactly like `BothHold`.
/// - `ConflictRetro` applies the exact same `supersede`/flag/pin-skip
///   mechanics but with `newer_wins`'s output consumed swapped: the
///   *later*-recorded memory is the retrospective one and loses, the
///   *earlier*-recorded memory is the one that's actually current and
///   wins. Recording time and event time aren't the same thing — see
///   `ContradictionVerdict`'s own doc comment.
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
        if store::is_pinned(conn, loser_id)? {
            // Fail safe: a human restored the loser, which pins it, so an
            // automatic supersession must never tombstone it — record the
            // pair as judged instead, same as BothHold, so it isn't
            // re-asked every run.
            eprintln!("mach kb: supersession skipped (pinned) #{} -> #{}", loser_id, winner_id);
            store::mark_contradiction_seen(conn, id_a, id_b)?;
            return Ok(false);
        }
        if let (Some(loser_mem), Some(winner_mem)) = (store::get(conn, loser_id)?, store::get(conn, winner_id)?) {
            if let Some(reason) = store::supersession_guard(conn, &loser_mem, &winner_mem, store::GuardKind::Contradiction) {
                // Deterministic backstop: a dated loser is never tombstoned
                // on the judge's CONFLICT/CONFLICT_RETRO alone — recorded
                // as judged, same as BothHold, so it isn't re-asked every
                // run.
                eprintln!("mach kb: supersession blocked ({}) #{} -> #{}", reason, loser_id, winner_id);
                store::mark_contradiction_seen(conn, id_a, id_b)?;
                return Ok(false);
            }
        }
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
///   recorded) but does set the caller's `llm_failed` flag, folded into the
///   run's summary line's "(degraded: ...)" reporting — it has no bearing
///   on whether the insight stage's own working set gets marked reflected.
///
/// Returns `(examined, promoted, demoted, any_judge_call_failed)`. Generic
/// over `ReflectLlm`, same as `run_dedupe_pass`/`run_contradiction_pass`,
/// so it's exercised in tests against a fake that can fail on demand.
/// Tse-style schema-accelerated consolidation (Tse et al. 2007, 2011): a
/// candidate the curation pass just promoted — durable, coherent with what's
/// already known — gets an extra stability boost when its content sits
/// close enough (`reflect::CURATION_SCHEMA_COHERENCE_MIN_SIM`) to an
/// existing active insight or theme: the durable "schema" `mach kb reflect`
/// has already built up. Information that fits an existing schema
/// consolidates faster than an orphan fact with nothing to attach to, so it
/// gets `reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER`x its current
/// stability (capped at 365 days, same as `store::touch`); an orphan
/// promotion — nothing close enough among active insights/themes — keeps
/// its ordinary default, no multiplier applied. No extra LLM call: this
/// reuses the embedding already on the row and the insight index already in
/// the store. Called from every place this pass promotes a row (both the
/// engagement fast path and an explicit PROMOTE verdict), since either one
/// already IS "judged coherent with what's known."
fn apply_schema_fast_path(conn: &Connection, m: &Memory) -> Result<(), KbError> {
    let emb = match &m.embedding {
        Some(e) if !e.is_empty() => e,
        _ => return Ok(()),
    };
    let near = store::top_similar_insights(conn, emb, 1)?;
    let coherent = near.first().map(|(_, sim)| *sim >= reflect::CURATION_SCHEMA_COHERENCE_MIN_SIM).unwrap_or(false);
    if coherent {
        store::multiply_stability(conn, m.id, reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER)?;
    }
    Ok(())
}

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
            apply_schema_fast_path(conn, &m)?;
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
                // report it via `llm_failed`.
                llm_failed = true;
                continue;
            }
        };
        match reflect::parse_curation_verdict(&raw) {
            reflect::CurationVerdict::Promote => {
                store::set_reviewed(conn, m.id, true)?;
                apply_schema_fast_path(conn, &m)?;
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


/// Resolves `name` to an entity id: exact case-insensitive name match first
/// (`store::find_entity_by_name`), else embedding similarity reuse at or
/// above `reflect::ENTITY_RESOLUTION_SIM_THRESHOLD`
/// (`store::find_entity_by_similarity`), else a fresh entity is created
/// (embedding the name itself, best-effort -- a failed embed just means the
/// new entity can only ever be matched again by exact name, never by
/// similarity). Returns `(id, true)` when a new entity was actually
/// created, `(id, false)` when an existing one was reused, so callers can
/// report how many genuinely new entities a pass added.
fn resolve_or_create_entity<E: Embedder, L: ReflectLlm>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    name: &str,
    kind: Option<&str>,
    now: &str,
) -> Result<(i64, bool), KbError> {
    let name = name.trim();
    if let Some(existing) = store::find_entity_by_name(conn, name)? {
        return Ok((existing.id, false));
    }
    // A name already judged to BE an entity resolves with no embed and no
    // call, so the judgement is paid for once and reused forever.
    if let Some(id) = store::alias_target(conn, name)? {
        return Ok((id, false));
    }
    let embedding = embedder.embed(name).ok();
    if let Some(emb) = &embedding {
        // Search from the bottom of the judged band, then decide what the
        // score entitles us to do.
        if let Some((existing, sim)) =
            store::find_entity_by_similarity(conn, emb, reflect::ENTITY_RESOLUTION_JUDGE_MIN)?
        {
            if sim >= reflect::ENTITY_RESOLUTION_SIM_THRESHOLD {
                return Ok((existing.id, false));
            }
            // The band where similarity cannot decide: "Remos"/"Remos
            // Space" and "GitHub"/"GitHub Actions" score alike, so ask.
            let verdict = match store::alias_verdict(conn, name, existing.id)? {
                Some(v) => v,
                None => match llm.call(
                    "haiku",
                    &reflect::build_entity_alias_prompt(name, &existing.name, existing.kind.as_deref()),
                    reflect::TIMEOUT_HAIKU,
                ) {
                    Ok(out) => {
                        let same = reflect::parse_entity_alias_verdict(&out);
                        store::mark_alias_verdict(conn, name, existing.id, same, now)?;
                        same
                    }
                    // A failed judge must not be recorded as DIFFERENT --
                    // that would poison the cache with an answer nobody
                    // gave. Mint the entity and let a later run decide.
                    Err(_) => false,
                },
            };
            if verdict {
                return Ok((existing.id, false));
            }
        }
    }
    let id = store::insert_entity(conn, name, kind, embedding.as_deref())?;
    Ok((id, true))
}

/// Looks up a relation's own entity names for the id-free edge-conflict
/// prompt (`reflect::build_batch_edge_contradiction_prompt`'s description
/// tuples). `None` if either entity has since vanished (shouldn't happen —
/// entities are never deleted — but never worth a panic over).
fn describe_relation(conn: &Connection, r: &store::Relation) -> Result<Option<(String, String, String)>, KbError> {
    let src = store::get_entity(conn, r.src)?;
    let dst = store::get_entity(conn, r.dst)?;
    match (src, dst) {
        (Some(s), Some(d)) => Ok(Some((s.name, r.predicate.clone(), d.name))),
        _ => Ok(None),
    }
}

/// Batch-judges every "new edge vs. a pre-existing edge it conflicts with"
/// candidate pair collected while a whole `mach kb reflect` graph-extraction
/// run's batches were being inserted, in ONE haiku call covering every pair
/// — the batched replacement for the old one-judge-call-per-edge path. The
/// newly inserted edge always wins a `Conflict` verdict, same as before (see
/// `reflect::EdgeContradictionVerdict::Conflict`'s own doc comment for why
/// the model is never asked to name a winner). `pending` is `(new_edge_id,
/// old_edge_id)` pairs, in the order they were discovered.
///
/// Deduped and re-checked against current state before building the prompt:
/// the same pair can be collected more than once (two triples resolving to
/// the same edge names), and an old edge already tombstoned earlier in this
/// same resolution (as an even-earlier pair's loser) needs no judgment at
/// all — `store::supersede_relation`'s own `invalidated_at IS NULL` guard
/// would just no-op on it anyway, but skipping it here also means it never
/// wastes a slot in the one batched prompt.
///
/// Unlike the memory-level graph-extraction watermark, a pair the model's
/// reply never addresses is simply left alone (both edges stay active) —
/// there is no backlog/retry concept for an edge-conflict check, it's a
/// one-time judgment made at insertion time, not a queue drained across
/// runs (see `reflect::parse_batch_edge_contradiction_verdicts`'s own doc
/// comment). Returns whether the batched call itself failed outright (no
/// pairs touched at all in that case) — this still gates the caller's
/// overall `llm_failed`, same as the old per-edge path did, even though it
/// never blocks marking a memory extracted.
fn resolve_pending_conflicts<L: ReflectLlm>(
    conn: &Connection,
    llm: &L,
    pending: &[(i64, i64)],
    now: &str,
) -> Result<bool, KbError> {
    if pending.is_empty() {
        return Ok(false);
    }
    let mut seen_pairs: HashSet<(i64, i64)> = HashSet::new();
    let mut resolved: Vec<(i64, i64, (String, String, String), (String, String, String))> = Vec::new();
    for &(new_id, old_id) in pending {
        if !seen_pairs.insert((new_id, old_id)) {
            continue;
        }
        let (new_rel, old_rel) = match (store::get_relation(conn, new_id)?, store::get_relation(conn, old_id)?) {
            (Some(n), Some(o)) => (n, o),
            _ => continue,
        };
        if !old_rel.is_active() {
            continue; // already resolved earlier in this same batch
        }
        let (old_desc, new_desc) = match (describe_relation(conn, &old_rel)?, describe_relation(conn, &new_rel)?) {
            (Some(o), Some(n)) => (o, n),
            _ => continue,
        };
        resolved.push((new_id, old_id, old_desc, new_desc));
    }
    if resolved.is_empty() {
        return Ok(false);
    }

    let prompt_pairs: Vec<((&str, &str, &str), (&str, &str, &str))> = resolved
        .iter()
        .map(|(_, _, old, new)| ((old.0.as_str(), old.1.as_str(), old.2.as_str()), (new.0.as_str(), new.1.as_str(), new.2.as_str())))
        .collect();
    let prompt = reflect::build_batch_edge_contradiction_prompt(&prompt_pairs);
    // TIMEOUT_GRAPH_EXTRACTION (120s), not the shared TIMEOUT_HAIKU_BATCH
    // (60s) other batched passes still use -- see that constant's own doc
    // comment for the judge_log measurement behind this call and
    // `run_graph_extraction_pass`'s own batch-extraction call justifying it.
    let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_GRAPH_EXTRACTION) {
        Ok(out) => out,
        Err(_) => return Ok(true), // whole batch call failed -- llm_failed, no edges touched
    };
    let verdicts = reflect::parse_batch_edge_contradiction_verdicts(&raw, resolved.len());
    for (i, (new_id, old_id, _, _)) in resolved.iter().enumerate() {
        if verdicts.get(&(i + 1)) == Some(&reflect::EdgeContradictionVerdict::Conflict) {
            store::supersede_relation(conn, *old_id, *new_id, now)?;
        }
    }
    Ok(false)
}

/// The graph-extraction pass (runs inside `mach kb reflect`, right after
/// strength review and before dormancy — reading facts the contradiction
/// patrol and strength review have already truth-maintained this run,
/// before dormancy sweeps stale ones out of the active pool): offers every
/// not-yet-extracted ACTIVE memory (`store::graph_extraction_candidates`,
/// capped at `reflect::GRAPH_EXTRACTION_MAX_PER_RUN`, oldest first) to a
/// batched haiku call, `reflect::GRAPH_EXTRACTION_BATCH_SIZE` facts at a
/// time, extracting 0-4 entity/relation triples per fact
/// (`reflect::parse_batch_extraction`). Each triple's entities are resolved
/// via `resolve_or_create_entity` and the edge is inserted directly
/// (`store::insert_relation`); every "new edge vs. pre-existing edge"
/// conflict candidate this whole run turns up is collected and judged in
/// ONE batched call at the very end (`resolve_pending_conflicts`), rather
/// than one judge call per edge — the same batching philosophy the
/// extraction call itself uses, applied to the conflict check.
///
/// A memory is marked extracted (`store::mark_graph_extracted`) only when
/// its own fact number was actually addressed by its batch's reply (an
/// explicit `NONE` or at least one well-formed triple) — never when its
/// whole batch call failed outright, and never when the reply simply never
/// touched that fact number (a malformed or truncated reply) — either way
/// it stays retry-able next run (see `Memory::graph_extracted_at`'s own doc
/// comment and `reflect::parse_batch_extraction`'s). The end-of-run
/// conflict-judge call failing is tracked in the returned `llm_failed` flag
/// (folded into the run's summary line's "(degraded: ...)" reporting, same
/// as every other sub-pass — it has no bearing on whether the insight
/// stage's own working set gets marked reflected) but never blocks marking
/// a memory itself extracted — the call that row's own
/// retry-ability depends on already succeeded; a missed conflict check
/// simply leaves both edges active, same outcome as an `Unclear`/`BothHold`
/// verdict, surfaceable later via `mach kb entity`.
///
/// Runs every invocation regardless of `has_new`, same as curation/dormancy
/// — it drains the not-yet-extracted backlog, not just this run's new
/// material (this is what makes a one-time full backfill over a pre-
/// existing corpus just "run `mach kb reflect` until the backlog is empty").
///
/// Returns `(examined, edges_created, entities_created, any_llm_call_failed)`.
fn run_graph_extraction_pass<E: Embedder, L: ReflectLlm>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    now: &str,
) -> Result<(usize, usize, usize, bool), KbError> {
    // Most surprising first: the pass is capped per run, so the cap decides
    // what gets examined, and a fact that restates the bank yields nothing
    // an existing edge does not already say.
    let candidates = store::order_by_surprisal(
        conn,
        store::graph_extraction_candidates(conn, reflect::GRAPH_EXTRACTION_MAX_PER_RUN)?,
    );
    let mut examined = 0usize;
    let mut edges_created = 0usize;
    let mut entities_created = 0usize;
    let mut llm_failed = false;
    // Every "new edge vs. pre-existing edge" conflict candidate this whole
    // run turns up, judged together in ONE call at the very end (see
    // `resolve_pending_conflicts`).
    let mut pending_conflicts: Vec<(i64, i64)> = Vec::new();

    for chunk in candidates.chunks(reflect::GRAPH_EXTRACTION_BATCH_SIZE) {
        let contents: Vec<&str> = chunk.iter().map(|m| m.content.as_str()).collect();
        let prompt = reflect::build_batch_extraction_prompt(&contents);
        // TIMEOUT_GRAPH_EXTRACTION (120s), raised from the shared
        // TIMEOUT_HAIKU_BATCH (60s) -- see that constant's own doc comment
        // for the judge_log measurement (every observed timeout hit exactly
        // the old 60s ceiling; comparably-sized successful calls took
        // 18.5s-57.9s) and why a larger batch size was rejected instead.
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_GRAPH_EXTRACTION) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue; // whole batch never marked extracted -- retried next run
            }
        };
        let parsed = reflect::parse_batch_extraction(&raw, chunk.len());
        // Was the reply COMPLETE? Truncation always loses the tail, so a
        // reply that addressed the batch's last fact cannot have been cut
        // short -- and then a fact it skipped in the middle was skipped on
        // purpose, meaning no extractable relation.
        //
        // This distinction is the whole fix for a starvation bug. The old
        // rule left every unaddressed fact unmarked so it would be retried,
        // which is right for truncation and wrong for omission: 79 rows the
        // model silently declined to mention came back every run, held the
        // entire GRAPH_EXTRACTION_MAX_PER_RUN budget, and starved newly
        // added memories of extraction. The backlog sat at exactly 79
        // across repeated reflect runs while 25% of the bank had no entity
        // links at all.
        let reply_complete = parsed.contains_key(&chunk.len());

        for (i, m) in chunk.iter().enumerate() {
            let fact_num = i + 1;
            let triples = match parsed.get(&fact_num) {
                Some(t) => t,
                None => {
                    // Omitted from a complete reply: no extractable
                    // relation here, which is a stable answer. Mark it so
                    // it stops competing for next run's budget.
                    if reply_complete {
                        store::mark_graph_extracted(conn, m.id, now)?;
                        examined += 1;
                    }
                    continue;
                }
            };
            examined += 1;

            // Edges from an unreviewed source memory get the same organic
            // confidence penalty `store::UNREVIEWED_SEARCH_PENALTY` applies
            // to search — see `reflect::GRAPH_EXTRACTION_UNREVIEWED_PENALTY`.
            let source_penalty = if m.reviewed { 1.0 } else { reflect::GRAPH_EXTRACTION_UNREVIEWED_PENALTY };

            for t in triples {
                let (src_id, src_new) =
                    resolve_or_create_entity(conn, embedder, llm, &t.src_name, t.src_kind.as_deref(), now)?;
                let (dst_id, dst_new) =
                    resolve_or_create_entity(conn, embedder, llm, &t.dst_name, t.dst_kind.as_deref(), now)?;
                entities_created += src_new as usize + dst_new as usize;
                // The memory mentions both endpoints (the substrate graph
                // recall and entity cards walk); a freshly minted entity also
                // gets linked back to every older memory that already named it.
                store::link_mention(conn, m.id, src_id, store::MENTION_SOURCE_EXTRACTION, now)?;
                store::link_mention(conn, m.id, dst_id, store::MENTION_SOURCE_EXTRACTION, now)?;
                if src_new {
                    store::link_entity_mentions_by_name_scan(conn, src_id, &t.src_name, now)?;
                }
                if dst_new {
                    store::link_entity_mentions_by_name_scan(conn, dst_id, &t.dst_name, now)?;
                }

                let confidence = t.confidence * source_penalty;
                // If this exact claim is already an active edge, this memory
                // is one more piece of evidence for it -- not a second copy
                // of the claim. Re-inserting was how the graph came to hold
                // five rows for "user works-on Helios".
                let new_edge_id = match store::active_relation_for_claim(conn, src_id, &t.predicate, dst_id)? {
                    Some(existing) => {
                        store::add_relation_evidence(conn, existing, m.id, now)?;
                        existing
                    }
                    None => {
                        let id = store::insert_relation(
                            conn,
                            src_id,
                            &t.predicate,
                            dst_id,
                            Some(m.id),
                            Some(confidence),
                            now,
                        )?;
                        store::add_relation_evidence(conn, id, m.id, now)?;
                        edges_created += 1;
                        id
                    }
                };

                for old in store::relations_conflicting_with(conn, src_id, &t.predicate, dst_id)? {
                    if old.id == new_edge_id {
                        continue; // defensive: relations_conflicting_with already excludes the literal same edge
                    }
                    pending_conflicts.push((new_edge_id, old.id));
                }
            }
            store::mark_graph_extracted(conn, m.id, now)?;
        }
    }

    let conflict_llm_failed = resolve_pending_conflicts(conn, llm, &pending_conflicts, now)?;
    llm_failed = llm_failed || conflict_llm_failed;

    Ok((examined, edges_created, entities_created, llm_failed))
}

/// The insight-dedupe pass (inside `mach kb reflect`, before the card and
/// theme passes): candidate same-level insight pairs above
/// `reflect::INSIGHT_DEDUPE_MIN_SIM` are batch-judged SAME/DIFFERENT, and a
/// SAME verdict merges the newer into the older
/// (`store::merge_insights`: union of evidence, higher confidence, themes
/// repointed, loser invalidated).
///
/// Why this pass exists: memories have had dedupe from the start, insights
/// never did, so reflection kept deriving the same belief from new evidence
/// and storing it again. The bank reached 28 active level-1 insights with
/// several near-identical ("audit before build" appeared as four separately
/// worded beliefs), and insights feed the session-start mental model
/// directly -- duplication there is not just waste, it reads as four
/// independent confirmations of one idea.
///
/// Returns `(examined, merged, any_llm_call_failed)`. A pair whose verdict
/// never arrives is left unmarked and retried next run.
fn run_insight_dedupe_pass<L: ReflectLlm>(
    conn: &Connection,
    llm: &L,
    now: &str,
) -> Result<(usize, usize, bool), KbError> {
    let seen = store::insight_dedupe_seen_pairs(conn)?;
    let pairs = store::insight_dedupe_candidate_pairs(
        conn,
        reflect::INSIGHT_DEDUPE_MIN_SIM,
        &seen,
        reflect::INSIGHT_DEDUPE_MAX_PAIRS_PER_RUN,
    )?;
    let mut examined = 0usize;
    let mut merged = 0usize;
    let mut llm_failed = false;

    for chunk in pairs.chunks(reflect::INSIGHT_DEDUPE_BATCH_SIZE) {
        // Re-read inside the loop: an earlier chunk this same run may have
        // merged one side away.
        let mut resolved: Vec<(i64, i64, String, String)> = Vec::new();
        for &(a, b) in chunk {
            match (store::get_insight(conn, a)?, store::get_insight(conn, b)?) {
                (Some(ia), Some(ib)) if ia.is_active() && ib.is_active() => {
                    resolved.push((a, b, ia.text, ib.text))
                }
                _ => continue,
            }
        }
        if resolved.is_empty() {
            continue;
        }
        let prompt_pairs: Vec<(&str, &str)> =
            resolved.iter().map(|(_, _, ta, tb)| (ta.as_str(), tb.as_str())).collect();
        let prompt = reflect::build_batch_insight_dedupe_prompt(&prompt_pairs);
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue;
            }
        };
        let verdicts = reflect::parse_batch_insight_dedupe_verdicts(&raw, resolved.len());

        for (i, (a, b, _, _)) in resolved.iter().enumerate() {
            match verdicts.get(&(i + 1)) {
                Some(reflect::InsightDedupeVerdict::Same) => {
                    examined += 1;
                    // Keep the older row: its id is what any theme already cites.
                    let (keep, drop_) = if a <= b { (*a, *b) } else { (*b, *a) };
                    if store::merge_insights(conn, keep, drop_, now)? {
                        merged += 1;
                    }
                }
                Some(reflect::InsightDedupeVerdict::Different) => {
                    examined += 1;
                    store::mark_insight_dedupe_seen(conn, *a, *b)?;
                }
                None => {} // unaddressed: retried next run
            }
        }
    }

    Ok((examined, merged, llm_failed))
}

/// The entity-card pass (inside `mach kb reflect`, after graph hygiene and
/// dormancy): for every entity whose card is missing or behind its evidence
/// (`store::entity_card_candidates`), one haiku call distills the memories
/// that mention it into a short profile (`reflect::build_entity_card_prompt`).
/// This is the generic form of a "who is this person" model: the same pass
/// profiles a person, a project, a practice, or a tool, because the card is
/// keyed on the entity, not on its kind.
///
/// Returns `(examined, built, any_llm_call_failed)`. A failed or NONE reply
/// leaves the previous card untouched and the entity due again next run --
/// never a half-written card, same "never mark seen on failure" rule the
/// other passes follow.
fn run_entity_card_pass<E: Embedder, L: ReflectLlm>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    now: &str,
) -> Result<(usize, usize, Option<String>), KbError> {
    let candidates = store::entity_card_candidates(conn, store::CARD_MAX_PER_RUN)?;
    let mut examined = 0usize;
    let mut built = 0usize;
    // The first failure's message, not just a flag: `classify` now puts the
    // subprocess stderr tail in there, and a bool threw exactly that away --
    // an unattended run reported "some calls failed" and nothing else.
    let mut first_error: Option<String> = None;

    for entity in candidates {
        let mems = store::memories_mentioning(conn, entity.id, Some(store::CARD_EVIDENCE_LIMIT))?;
        if (mems.len() as i64) < store::CARD_MIN_MENTIONS {
            continue; // raced with dormancy/supersession since the candidate query
        }
        examined += 1;
        let evidence: Vec<(i64, String)> = mems.iter().map(|m| (m.id, m.content.clone())).collect();
        let existing = store::get_entity_card(conn, entity.id)?;
        let prompt = reflect::build_entity_card_prompt(
            &entity.name,
            entity.kind.as_deref(),
            &evidence,
            existing.as_ref().map(|c| c.text.as_str()),
        );
        // The batch headroom, not the single-fact one: a card prompt carries
        // up to `CARD_EVIDENCE_LIMIT` (14) memories plus the existing card and
        // asks for a whole synthesized profile back, which is the same shape
        // `TIMEOUT_HAIKU_BATCH` was raised for. At 30s the tail of this pass
        // was timing out rather than failing -- three of six entities per run
        // stayed due with "'claude' timed out after 30s".
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => out,
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
                continue;
            }
        };
        let Some(card) = reflect::parse_entity_card(&raw) else {
            continue; // NONE or malformed: keep whatever card is already there
        };
        // Only ids that are really among this entity's evidence -- a
        // hallucinated citation must never end up in a card's provenance.
        let allowed: HashSet<i64> = mems.iter().map(|m| m.id).collect();
        let cited: Vec<String> =
            card.memory_ids.iter().filter(|id| allowed.contains(id)).map(|id| id.to_string()).collect();
        if cited.is_empty() {
            continue;
        }
        let embedding = embedder.embed(&format!("{} — {}", entity.name, card.text)).ok();
        let watermark = store::max_mention_memory_id(conn, entity.id)?;
        let mention_count = store::mention_degree(conn, entity.id)?;
        store::upsert_entity_card(
            conn,
            entity.id,
            &card.text,
            &cited,
            embedding.as_deref(),
            watermark,
            mention_count,
            now,
        )?;
        built += 1;
    }

    Ok((examined, built, first_error))
}

/// The graph hygiene pass (runs inside `mach kb reflect`, right after graph
/// extraction and before dormancy): evidence-death propagation (below,
/// deterministic, no LLM) followed by batched entity-merge judging (below).
/// Returns `(evidence_dead, entities_merged, any_llm_call_failed)`.
fn run_graph_hygiene_pass<L: ReflectLlm>(conn: &Connection, llm: &L, now: &str) -> Result<(usize, usize, bool), KbError> {
    let evidence_dead = run_evidence_death_propagation(conn, now)?;
    let (entities_merged, llm_failed) = run_entity_merge_pass(conn, llm, now)?;
    Ok((evidence_dead, entities_merged, llm_failed))
}

/// Evidence-death propagation: every ACTIVE edge whose evidence memory has
/// since died (invalidated/superseded, or hard-deleted via `mach kb
/// forget`) is invalidated too, via `store::invalidate_relation` —
/// `superseded_by` stays NULL (the edge died with its evidence, it wasn't
/// beaten by a rival edge). Purely deterministic, no LLM involved; an edge
/// whose evidence is merely dormant is deliberately left alone here — see
/// `store::relation_evidence_is_dormant`'s own doc comment for why that
/// case is instead respected only at recall-query time
/// (`active_relations_for_recall`), never by tombstoning the edge outright.
/// Returns the number of edges invalidated.
fn run_evidence_death_propagation(conn: &Connection, now: &str) -> Result<usize, KbError> {
    let dying = store::relations_with_dead_evidence(conn)?;
    let mut count = 0usize;
    for r in &dying {
        if store::invalidate_relation(conn, r.id, now)? {
            count += 1;
        }
    }
    Ok(count)
}

/// Batched entity-merge judging: candidate pairs
/// (`store::entity_merge_candidate_pairs`, name-embedding similarity or a
/// case/punctuation-insensitive exact name match, capped at
/// `reflect::ENTITY_MERGE_MAX_PAIRS_PER_RUN`) are described by kind + up to
/// 2 sample edges each (id-free, `store::sample_relation_descriptions`) and
/// batch-judged SAME/DIFFERENT, `reflect::ENTITY_MERGE_BATCH_SIZE` pairs per
/// call — same batching philosophy `run_graph_extraction_pass`'s own
/// `resolve_pending_conflicts` already established for edge conflicts.
///
/// `Same` keeps the older (lower-id) entity, repoints every edge off the
/// newer one (`store::repoint_entity_relations`, which also dedupes any
/// resulting identical edges), and deletes the newer entity row
/// (`store::delete_entity`) — never watermarked in `entity_merge_seen`
/// (the merged entity is gone, so it can never resurface as a candidate,
/// mirroring the dedupe pass's own treatment of a merged memory).
/// `Different` is recorded in `entity_merge_seen` so the pair is never
/// re-asked. A pair a batch reply never addressed, or a whole batch call
/// that failed outright, is left exactly as it is — never watermarked,
/// never merged — so it gets a fresh chance on a later run (the "failed LLM
/// judgments never marked seen" house rule, same as every other batched
/// pass in this module).
///
/// Returns `(entities_merged, any_llm_call_failed)`.
fn run_entity_merge_pass<L: ReflectLlm>(conn: &Connection, llm: &L, now: &str) -> Result<(usize, bool), KbError> {
    let seen = store::entity_merge_seen_pairs(conn)?;
    let pairs = store::entity_merge_candidate_pairs(
        conn,
        reflect::ENTITY_MERGE_MIN_SIM,
        &seen,
        reflect::ENTITY_MERGE_MAX_PAIRS_PER_RUN,
    )?;

    let mut merged = 0usize;
    let mut llm_failed = false;

    for chunk in pairs.chunks(reflect::ENTITY_MERGE_BATCH_SIZE) {
        // Re-fetch descriptions fresh per batch: an earlier chunk in this
        // same run may already have merged (deleted) an entity this
        // chunk's own candidates reference.
        let mut resolved: Vec<(i64, i64, (String, String, Vec<String>), (String, String, Vec<String>))> = Vec::new();
        for &(id_a, id_b) in chunk {
            let (ea, eb) = match (store::get_entity(conn, id_a)?, store::get_entity(conn, id_b)?) {
                (Some(a), Some(b)) => (a, b),
                _ => continue, // one side already merged away earlier this run
            };
            let kind_a = ea.kind.clone().unwrap_or_else(|| "unspecified".to_string());
            let kind_b = eb.kind.clone().unwrap_or_else(|| "unspecified".to_string());
            let samples_a = store::sample_relation_descriptions(conn, id_a, 2)?;
            let samples_b = store::sample_relation_descriptions(conn, id_b, 2)?;
            resolved.push((id_a, id_b, (ea.name, kind_a, samples_a), (eb.name, kind_b, samples_b)));
        }
        if resolved.is_empty() {
            continue;
        }

        let prompt_pairs: Vec<((&str, &str, &[String]), (&str, &str, &[String]))> = resolved
            .iter()
            .map(|(_, _, a, b)| ((a.0.as_str(), a.1.as_str(), a.2.as_slice()), (b.0.as_str(), b.1.as_str(), b.2.as_slice())))
            .collect();
        let prompt = reflect::build_batch_entity_merge_prompt(&prompt_pairs);
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue;
            }
        };
        let verdicts = reflect::parse_batch_entity_merge_verdicts(&raw, resolved.len());

        for (i, (id_a, id_b, _, _)) in resolved.iter().enumerate() {
            match verdicts.get(&(i + 1)) {
                Some(reflect::EntityMergeVerdict::Same) => {
                    let (keep_id, drop_id) = if *id_a < *id_b { (*id_a, *id_b) } else { (*id_b, *id_a) };
                    store::repoint_entity_relations(conn, drop_id, keep_id, now)?;
                    store::delete_entity(conn, drop_id)?;
                    merged += 1;
                }
                Some(reflect::EntityMergeVerdict::Different) => {
                    store::mark_entity_merge_seen(conn, *id_a, *id_b)?;
                }
                None => {} // unaddressed -- left exactly as it is, retried next run
            }
        }
    }

    Ok((merged, llm_failed))
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

/// Resolves `name` to an `Entity` for `mach kb entity`/search enrichment's
/// own purposes: exact case-insensitive match first, else embedding
/// similarity at or above `reflect::ENTITY_RESOLUTION_SIM_THRESHOLD` — the
/// same resolution rule extraction itself uses to avoid minting a
/// near-duplicate, reused here so `mach kb entity <name>` finds "the same
/// thing" a slightly-off-spelled query names.
fn resolve_entity_for_lookup(conn: &Connection, embedder: &OllamaEmbedder, name: &str) -> Option<store::Entity> {
    if let Ok(Some(e)) = store::find_entity_by_name(conn, name) {
        return Some(e);
    }
    let emb = embedder.embed(name).ok()?;
    store::find_entity_by_similarity(conn, &emb, reflect::ENTITY_RESOLUTION_SIM_THRESHOLD).ok().flatten().map(|(e, _)| e)
}

/// `mach kb entity <name>` — the association-graph lookup: resolves `name`
/// to an entity (exact match first, else embedding similarity), then prints
/// every active edge touching it in either direction, each with the other
/// entity's name, the predicate, and the evidence memory's content snippet
/// and date. A trailing footnote counts invalidated (tombstoned) edges
/// without printing them — this command is about the graph's current,
/// active belief, not its audit trail (`mach kb list --superseded` is the
/// memory-level equivalent).
/// What `mach kb why` was asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyTarget {
    Memory(i64),
    Insight(i64),
}

/// Parses `why`'s positional arguments: `<id>`, `i<id>`, or `insight <id>`.
pub fn parse_why_target(args: &[String]) -> Option<WhyTarget> {
    match args {
        [one] => {
            if let Some(rest) = one.strip_prefix('i') {
                rest.parse().ok().map(WhyTarget::Insight)
            } else {
                one.parse().ok().map(WhyTarget::Memory)
            }
        }
        [kw, id] if kw == "insight" || kw == "theme" => id.parse().ok().map(WhyTarget::Insight),
        [kw, id] if kw == "memory" => id.parse().ok().map(WhyTarget::Memory),
        _ => None,
    }
}

/// Rust twin of kb-recall.py's `source_phrase`: how a memory was learned,
/// refined by its basis. Kept in step with the hook by hand (the hook is
/// the one users read; this is the audit view).
pub fn provenance_phrase(source: Option<&str>, basis: Option<&str>) -> String {
    let s = source.unwrap_or("").trim();
    let where_ = if s.starts_with("meeting:") {
        Some("a meeting")
    } else if s == "session-digest" || s.starts_with("session-digest:") || s.starts_with("transcript-backfill") {
        Some("a session")
    } else {
        None
    };
    match (basis, where_) {
        // Index-owned (`code_index::summary`'s module/repo mirrors): never
        // "you told me" -- the user never said it, an automatic pass
        // generated it from code -- regardless of what `source` looks like.
        (Some("derived"), _) => "derived by the code index".to_string(),
        (Some("experience"), w) => format!("I did this in {}", w.unwrap_or("an earlier session")),
        (Some("inferred"), w) => format!("I inferred this from {}", w.unwrap_or("context")),
        (Some("stated"), Some(w)) => format!("you said this in {}", w),
        (_, Some("a meeting")) => "from a meeting".to_string(),
        _ if s.starts_with("memory-backfill") => "from earlier project memory".to_string(),
        (_, Some("a session")) => "picked up from a session".to_string(),
        _ => "you told me".to_string(),
    }
}

/// Session ids whose recall log ever injected memory `id`, from
/// `<recall_dir>/<session>.jsonl`. Empty when the dir is missing.
fn sessions_that_showed(recall_dir: &Path, id: i64) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(recall_dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if ingest::parse_recall_log(&content).contains(&id) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push(stem.to_string());
            }
        }
    }
    out.sort();
    out
}

/// `~/.claude/projects/<any>/<session_id>.jsonl` if such a transcript exists.
fn find_transcript(session_id: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let projects = Path::new(&home).join(".claude/projects");
    for entry in std::fs::read_dir(projects).ok()?.flatten() {
        let candidate = entry.path().join(format!("{}.jsonl", session_id));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// How far a `session-digest` memory's `created_at` may sit from a digest
/// event for that session to count as its likely origin (the digest writes
/// its facts within seconds of finishing; 3 minutes covers a slow embed).
pub const WHY_SESSION_WINDOW_SECS: i64 = 180;

fn memory_line(m: &Memory) -> String {
    let date = m.created_at.get(..10).unwrap_or(&m.created_at);
    let basis = m.basis.as_deref().map(|b| format!(", {}", b)).unwrap_or_default();
    format!("#{} ({}{}) {}", m.id, date, basis, truncate(&m.content, 90))
}

/// The full provenance report for `mach kb why`, as text. `recall_dir` is
/// where kb-recall.py logs injections (`None` skips that section, e.g. in
/// tests); `home_meetings` is `~/.local/share/mach/meetings` (same). Pure
/// read: touches nothing, reinforces nothing.
pub fn why_report(
    conn: &Connection,
    target: WhyTarget,
    recall_dir: Option<&Path>,
    meetings_dir: Option<&Path>,
) -> Result<String, KbError> {
    let mut out = String::new();
    match target {
        WhyTarget::Memory(id) => {
            let Some(m) = store::get(conn, id)? else {
                return Ok(format!("no memory #{}", id));
            };
            let date = m.created_at.get(..10).unwrap_or(&m.created_at);
            out.push_str(&format!(
                "memory #{}  created {}  importance {}  {}{}\n",
                m.id,
                date,
                m.importance,
                if m.reviewed { "reviewed" } else { "unreviewed" },
                m.project.as_deref().map(|p| format!("  project {}", p)).unwrap_or_default(),
            ));
            out.push_str(&format!("  {}\n", m.content));
            out.push_str(&format!(
                "  provenance: {}  [source {}]\n",
                provenance_phrase(m.source.as_deref(), m.basis.as_deref()),
                m.source.as_deref().unwrap_or("none"),
            ));
            out.push_str(&format!(
                "  basis: {}\n",
                match m.basis.as_deref() {
                    Some("stated") => "stated (the user or a named person said it in so many words)",
                    Some("inferred") => "inferred (deduced from behavior, code, or context)",
                    Some("experience") => "experience (what Claude did here, and how the user responded)",
                    Some("derived") => "derived",
                    _ => "unknown (written before the basis column existed, or by a channel that does not classify)",
                }
            ));
            // status
            let mut status = Vec::new();
            if let Some(sb) = m.superseded_by {
                status.push(format!("superseded by #{}{}", sb, m.invalidated_at.as_deref().map(|d| format!(" on {}", &d[..10.min(d.len())])).unwrap_or_default()));
            } else if m.invalidated_at.is_some() {
                status.push("invalidated".to_string());
            }
            if let Some(d) = &m.dormant_at {
                status.push(format!("dormant since {}", &d[..10.min(d.len())]));
            }
            if status.is_empty() {
                status.push("active".to_string());
            }
            if let Some(v) = &m.last_verified_at {
                status.push(format!("last verified {}", &v[..10.min(v.len())]));
            }
            out.push_str(&format!("  status: {}\n", status.join(", ")));
            if let Some(p) = &m.pinned_at {
                out.push_str(&format!("  pinned: {}\n", p));
            }
            let preds = store::predecessors_of(conn, id)?;
            for p in &preds {
                out.push_str(&format!("  supersedes {}\n", memory_line(p)));
            }
            // engagement
            let stab = m.effective_stability();
            out.push_str(&format!(
                "  engagement: accessed {}×{}{}, stability {:.0}d\n",
                m.access_count,
                m.first_accessed_at.as_deref().map(|d| format!(" (first {}", &d[..10.min(d.len())])).unwrap_or_default(),
                m.last_accessed_at.as_deref().map(|d| format!(", last {})", &d[..10.min(d.len())])).unwrap_or_else(|| if m.first_accessed_at.is_some() { ")".to_string() } else { String::new() }),
                stab,
            ));
            if let Some(dir) = recall_dir {
                let shown = sessions_that_showed(dir, id);
                if shown.is_empty() {
                    out.push_str("  recall: never injected into a session (per recall log)\n");
                } else {
                    let list: Vec<&str> = shown.iter().take(3).map(|s| s.as_str()).collect();
                    out.push_str(&format!(
                        "  recall: injected in {} session(s){}: {}{}\n",
                        shown.len(),
                        if shown.len() > 3 { ", latest" } else { "" },
                        list.join(", "),
                        if shown.len() > 3 { ", …" } else { "" },
                    ));
                }
            }
            // origin trace by source class
            let src = m.source.clone().unwrap_or_default();
            if src == "session-digest" || src.starts_with("session-digest:") {
                let near = store::sessions_near(conn, &m.created_at, WHY_SESSION_WINDOW_SECS)?;
                if near.is_empty() {
                    out.push_str("  origin: a session digest; no ingest event within 3 min of creation (session id unknown)\n");
                } else {
                    out.push_str("  origin: a session digest; likely session (by proximity, not a stored link):\n");
                    for (sid, ts, kind) in near.iter().take(3) {
                        let transcript = find_transcript(sid)
                            .map(|p| format!("  transcript {}", p.display()))
                            .unwrap_or_else(|| "  transcript not found locally".to_string());
                        out.push_str(&format!("    {} ({} at {}){}\n", sid, kind, &ts[..19.min(ts.len())], transcript));
                    }
                }
            } else if let Some(dir_name) = src.strip_prefix("meeting:") {
                let path = meetings_dir.map(|d| d.join(dir_name).join("transcript.md"));
                match path {
                    Some(p) if p.is_file() => out.push_str(&format!("  origin: meeting transcript {}\n", p.display())),
                    Some(p) => out.push_str(&format!("  origin: meeting {} (transcript missing at {})\n", dir_name, p.display())),
                    None => out.push_str(&format!("  origin: meeting {}\n", dir_name)),
                }
            } else if let Some(ids) = src.strip_prefix("consolidation:") {
                out.push_str("  origin: consolidated by reflect from:\n");
                for part in ids.split(',') {
                    if let Ok(sid) = part.trim().parse::<i64>() {
                        match store::get(conn, sid)? {
                            Some(sm) => out.push_str(&format!("    {}\n", memory_line(&sm))),
                            None => out.push_str(&format!("    #{} (gone)\n", sid)),
                        }
                    }
                }
            } else if let Some(file) = src.strip_prefix("memory-backfill:") {
                out.push_str(&format!(
                    "  origin: Claude Code auto-memory file {} (retired 2026-09-08; backup in ~/.local/share/mach/backups/)\n",
                    file
                ));
            } else if src.starts_with("note:") || src.starts_with("telegram:") {
                out.push_str(&format!("  origin: {} (your own words)\n", src));
            } else if !src.is_empty() {
                out.push_str(&format!("  origin: {}\n", src));
            }
            // downstream
            // The full downstream cone, not just direct citers: a memory
            // feeds an insight, that insight feeds a theme, and the theme is
            // what the session-start mental model shows.
            let cites = store::downstream_of_memory(conn, id)?;
            if !cites.is_empty() {
                let direct: HashSet<i64> = store::insights_citing(conn, id)?.iter().map(|i| i.id).collect();
                out.push_str("  everything resting on it:\n");
                for iid in &cites {
                    let Some(i) = store::get_insight(conn, *iid)? else { continue };
                    out.push_str(&format!(
                        "    {} {} #{} (confidence {:.2}{}) {}\n",
                        if direct.contains(iid) { "directly:" } else { "indirectly:" },
                        if i.level >= 2 { "theme" } else { "insight" },
                        i.id,
                        i.confidence,
                        if i.is_flagged() { ", DOUBTED" } else { "" },
                        truncate(&i.text, 80)
                    ));
                }
            }
            let mentions = store::entities_of_memory(conn, id)?;
            if !mentions.is_empty() {
                let names: Vec<String> = mentions
                    .iter()
                    .map(|e| {
                        let on_card = store::get_entity_card(conn, e.id)
                            .ok()
                            .flatten()
                            .map(|c| c.source_ids.iter().any(|sid| sid.parse::<i64>() == Ok(id)))
                            .unwrap_or(false);
                        if on_card {
                            format!("{} (on its card)", e.name)
                        } else {
                            e.name.clone()
                        }
                    })
                    .collect();
                out.push_str(&format!("  mentions: {}\n", names.join(", ")));
            }
            let edges = store::relations_evidenced_by(conn, id)?;
            if !edges.is_empty() {
                out.push_str("  evidence for graph edges:\n");
                for e in &edges {
                    let name = |eid: i64| store::get_entity(conn, eid).ok().flatten().map(|x| x.name).unwrap_or_else(|| format!("#{}", eid));
                    let shared = store::relation_evidence(conn, e.id)?.len();
                    let also = if shared > 1 { format!(" (with {} other memories)", shared - 1) } else { String::new() };
                    out.push_str(&format!("    {} —{}→ {}{}\n", name(e.src), e.predicate, name(e.dst), also));
                }
            }
            let now = store::now_rfc3339();
            let assoc = store::assoc_neighbors(conn, id, &now)?;
            if !assoc.is_empty() {
                out.push_str("  associated (engaged together in past sessions):\n");
                for (other, w) in assoc.iter().take(5) {
                    if let Some(om) = store::get(conn, *other)? {
                        out.push_str(&format!("    w {:.2}  {}\n", w, memory_line(&om)));
                    }
                }
            }
        }
        WhyTarget::Insight(id) => {
            let Some(i) = store::get_insight(conn, id)? else {
                return Ok(format!("no insight #{}", id));
            };
            let date = i.created_at.get(..10).unwrap_or(&i.created_at);
            out.push_str(&format!(
                "{} #{}  created {}  confidence {:.2}{}{}\n",
                if i.level >= 2 { "theme" } else { "insight" },
                i.id,
                date,
                i.confidence,
                if i.is_flagged() { "  DOUBTED" } else { "" },
                i.last_verified_at.as_deref().map(|v| format!("  last verified {}", &v[..10.min(v.len())])).unwrap_or_default(),
            ));
            out.push_str(&format!("  {}\n", i.text));
            out.push_str(&format!("  derived from {} source(s):\n", i.source_ids.len()));
            for sid in &i.source_ids {
                let Ok(n) = sid.parse::<i64>() else { continue };
                if i.level >= 2 {
                    match store::get_insight(conn, n)? {
                        Some(sub) => out.push_str(&format!(
                            "    insight #{} (confidence {:.2}, {} memories) {}\n",
                            sub.id,
                            sub.confidence,
                            sub.source_ids.len(),
                            truncate(&sub.text, 90)
                        )),
                        None => out.push_str(&format!("    insight #{} (gone)\n", n)),
                    }
                } else {
                    match store::get(conn, n)? {
                        Some(m) => out.push_str(&format!(
                            "    {}  [{}]\n",
                            memory_line(&m),
                            provenance_phrase(m.source.as_deref(), m.basis.as_deref())
                        )),
                        None => out.push_str(&format!("    #{} (gone)\n", n)),
                    }
                }
            }
            let cites = store::insights_citing(conn, id)?;
            for t in cites.iter().filter(|t| t.level >= 2) {
                out.push_str(&format!("  cited by theme #{} (confidence {:.2}) {}\n", t.id, t.confidence, truncate(&t.text, 90)));
            }
            // What dies with it: every memory whose death would flag this
            // insight, read in the other direction.
            let evidence_alive = i
                .source_ids
                .iter()
                .filter_map(|s| s.parse::<i64>().ok())
                .filter(|mid| {
                    if i.level >= 2 {
                        store::get_insight(conn, *mid).ok().flatten().map(|x| x.is_active()).unwrap_or(false)
                    } else {
                        store::get(conn, *mid).ok().flatten().map(|m| m.invalidated_at.is_none()).unwrap_or(false)
                    }
                })
                .count();
            if evidence_alive < i.source_ids.len() {
                out.push_str(&format!(
                    "  WARNING: {} of {} cited sources are gone\n",
                    i.source_ids.len() - evidence_alive,
                    i.source_ids.len()
                ));
            }
        }
    }
    Ok(out)
}

/// `mach kb cards` — run ONLY the entity-card pass.
///
/// The card pass inside `mach kb reflect` is capped per run so a nightly
/// reflection stays bounded, which means a bank that has just gained the
/// feature (or just had 50 entities go stale at once) takes days of timer
/// runs to catch up. This drains the queue on demand.
/// What one `ask` run gathered and concluded.
pub struct AskOutcome {
    pub evidence: Vec<(i64, String)>,
    pub passages: Vec<String>,
    pub trail: Vec<String>,
}

/// The gather loop, shared by `mach kb ask` and `mach kb eval-ask`.
///
/// Extracted so the evaluator measures the real thing rather than a
/// reimplementation of it: a harness that drifts from the command it grades
/// is worse than no harness.
pub fn run_ask_gather<E: Embedder, L: ReflectLlm>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    question: &str,
    rounds: usize,
    now: &str,
    verbose: bool,
) -> io::Result<AskOutcome> {
    let question = question.to_string();
    // Insertion-ordered so the prompt reads in the order the loop found
    // things, with the first round's best matches at the top.
    let mut evidence: Vec<(i64, String)> = Vec::new();
    // Raw transcript passages ride alongside, never merged into `evidence`:
    // they carry no memory id, so nothing may cite them as a bank row.
    let mut passages: Vec<String> = Vec::new();
    let mut seen: HashSet<i64> = HashSet::new();
    let mut trail: Vec<String> = Vec::new();
    // Every gather already performed, so a judge that asks twice for the
    // same thing ends the loop instead of spending a round re-finding rows
    // that are already in the pool.
    let mut done: HashSet<String> = HashSet::new();

    let mut push = |evidence: &mut Vec<(i64, String)>, id: i64, content: String| -> bool {
        if seen.insert(id) && evidence.len() < ask::EVIDENCE_CAP {
            evidence.push((id, content));
            return true;
        }
        false
    };

    // A round gathers, then judges. The first gather is always the question
    // as asked; after that the judge chooses, and its choice IS the gather —
    // a hop is not followed by a repeat of the search that preceded it.
    let mut next = Some(ask::Step::Search(question.clone()));

    for round in 1..=rounds {
        let step = match next.take() {
            Some(s) => s,
            None => break,
        };
        match step {
            ask::Step::Enough => break,
            ask::Step::Search(q) => {
                if !done.insert(format!("search:{}", q.to_lowercase())) {
                    trail.push(format!("round {}: search {:?} -> already run, stopping", round, q));
                    break;
                }
                // Round 1 searches transcripts too, without being asked.
                //
                // Measured: with `TRANSCRIPT:` available only as a judge
                // verb, three of five eval-ask failures never invoked it --
                // the judge saw topically plausible memories and said
                // ENOUGH, never learning that the literal it needed was one
                // BM25 query away. A judge is the wrong gate for "is the
                // distilled layer missing something", because the evidence
                // it reads cannot show what is absent from it. The channel
                // costs no LLM call, so run it and let the pool speak.
                if round == 1 {
                    // Embed the query for the semantic channel; if ollama
                    // is unreachable the search degrades to lexical rather
                    // than failing the round.
                    let qe = embedder.embed(&q).ok();
                    let hits =
                        store::search_transcripts(conn, &q, qe.as_deref(), ask::TRANSCRIPT_LIMIT).map_err(to_io)?;
                    let n = hits.len();
                    for h in hits {
                        let when = h.ts.as_deref().unwrap_or("undated");
                        passages.push(format!("[{} session {}, {}]\n{}", h.project, h.session_id, when, h.text));
                    }
                    done.insert(format!("transcript:{}", q.to_lowercase()));
                    trail.push(format!("round 1: transcript {:?} -> {} passages (automatic)", q, n));
                    if verbose {
                        eprintln!("{}", trail.last().unwrap());
                    }
                }
                let resp =
                    search_hits(conn, embedder, &q, ask::ROUND_LIMIT, false, false, ask::ROUND_MIN_SCORE, now, None, None, None)
                        .map_err(to_io)?;
                let found = resp.hits.len();
                let mut added = 0usize;
                for h in resp.hits {
                    // Insights share the numeric id space with memories and
                    // cannot be cited as a source row, so only memories
                    // enter the pool.
                    if !h.derived && push(&mut evidence, h.id, h.content) {
                        added += 1;
                    }
                }
                trail.push(format!("round {}: search {:?} -> {} hits, {} new, {} total", round, q, found, added, evidence.len()));
            }
            ask::Step::Transcript(q) => {
                if !done.insert(format!("transcript:{}", q.to_lowercase())) {
                    trail.push(format!("round {}: transcript {:?} -> already searched, stopping", round, q));
                    break;
                }
                let qe = embedder.embed(&q).ok();
                let hits = store::search_transcripts(conn, &q, qe.as_deref(), ask::TRANSCRIPT_LIMIT).map_err(to_io)?;
                let found = hits.len();
                for h in hits {
                    let when = h.ts.as_deref().unwrap_or("undated");
                    passages.push(format!("[{} session {}, {}]\n{}", h.project, h.session_id, when, h.text));
                }
                trail.push(format!("round {}: transcript {:?} -> {} passages", round, q, found));
            }
            ask::Step::Hop(name) => {
                if !done.insert(format!("hop:{}", name.to_lowercase())) {
                    trail.push(format!("round {}: hop {:?} -> already taken, stopping", round, name));
                    break;
                }
                let Some(entity) = store::find_entity_by_name(conn, &name).map_err(to_io)? else {
                    trail.push(format!("round {}: hop {:?} -> no such entity, stopping", round, name));
                    break;
                };
                let mems = store::memories_mentioning(conn, entity.id, Some(ask::HOP_LIMIT)).map_err(to_io)?;
                let mut added = 0usize;
                for m in mems {
                    if push(&mut evidence, m.id, m.content) {
                        added += 1;
                    }
                }
                trail.push(format!("round {}: hop {:?} -> {} new, {} total", round, name, added, evidence.len()));
            }
        }
        if verbose {
            eprintln!("{}", trail.last().unwrap());
        }
        if round == rounds {
            break;
        }

        let probe = ask::build_probe_prompt(&question, &evidence, &passages, round);
        next = Some(match llm.call("haiku", &probe, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => ask::parse_step(&out),
            // A failed judge is not a failed answer: synthesize over what
            // the rounds so far already gathered.
            Err(e) => {
                if verbose {
                    eprintln!("round {}: judge failed ({}), answering with what we have", round, e);
                }
                ask::Step::Enough
            }
        });
    }

    Ok(AskOutcome { evidence, passages, trail })
}

/// `mach kb ask` — iterative agentic recall (see `ask` for the shape and
/// why it is a command rather than part of the hook).
///
/// One round is: search, then ask haiku whether what came back answers the
/// question. If not, haiku picks the next move — a rewording, or a hop to
/// an entity it saw named in the evidence — and the round runs again with
/// that. After `ask::MAX_ROUNDS`, or as soon as haiku says ENOUGH, one
/// sonnet call answers over everything gathered.
///
/// Evidence accumulates across rounds and is deduplicated by memory id, so
/// a reworded query that re-finds the same rows costs nothing and a hop
/// only ever adds. Every id sent is remembered so `ask::cited_ids` can
/// reject a citation to anything that was not.
/// Every `.jsonl` under `dir`, at any depth. Symlinks are not followed --
/// a loop through one would walk forever.
fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}

/// `mach kb eval-ask` — score iterative recall the way `mach kb eval`
/// scores the hook.
///
/// Same discipline: deterministic, no LLM judge, run through the real code
/// path (`run_ask_gather`), read-only against the bank. Different unit,
/// though — an ask question is graded on substrings, not memory ids,
/// because most of what it should reach is a transcript passage and those
/// carry no id on purpose.
///
/// Reports retrieval and answering separately. A run that gathers the right
/// passage and then writes around it is a synthesis problem; one that never
/// gathers it is a retrieval problem, and conflating them would send the
/// next fix to the wrong layer.
fn cmd_eval_ask(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut file: Option<String> = None;
    let mut json = false;
    let mut verbose = false;
    let mut rounds = ask::MAX_ROUNDS;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--file" => file = args.next(),
            "--rounds" => rounds = args.next().and_then(|v| v.parse().ok()).unwrap_or(rounds),
            "--json" => json = true,
            "--verbose" | "-v" => verbose = true,
            "-h" | "--help" => {
                println!("usage: mach kb eval-ask [--file F] [--rounds N] [--json] [--verbose]");
                println!("       Scores `mach kb ask` on questions whose answers live in raw");
                println!("       transcripts. Default set: ~/{}", eval::DEFAULT_ASK_QUESTIONS_PATH);
                return Ok(());
            }
            other => {
                eprintln!("mach kb eval-ask: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let path = match file {
        Some(f) => PathBuf::from(f),
        None => {
            let home = std::env::var("HOME").unwrap_or_default();
            Path::new(&home).join(eval::DEFAULT_ASK_QUESTIONS_PATH)
        }
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mach kb eval-ask: cannot read {}: {}", path.display(), e);
            std::process::exit(1);
        }
    };
    let questions = eval::parse_ask_questions(&content).map_err(to_io)?;

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    let now = store::now_rfc3339();

    let mut results = Vec::new();
    for q in &questions {
        let out = run_ask_gather(&conn, &embedder, &llm, &q.query, rounds, &now, verbose)?;
        let used_transcript = !out.passages.is_empty();
        let mut pool = String::new();
        for (id, c) in &out.evidence {
            pool.push_str(&format!("[{}] {}\n", id, c));
        }
        for p in &out.passages {
            pool.push_str(p);
            pool.push('\n');
        }
        let answer = if out.evidence.is_empty() && out.passages.is_empty() {
            String::new()
        } else {
            let prompt = ask::build_answer_prompt(&q.query, &out.evidence, &out.passages);
            llm.call("sonnet", &prompt, reflect::TIMEOUT_SONNET).unwrap_or_default()
        };
        let r = eval::score_ask(q, &pool, &answer, out.trail.len(), used_transcript);
        if verbose {
            eprintln!("{}: retrieved={} answered={} transcript={}", r.id, r.retrieved, r.answered, r.used_transcript);
        }
        results.push(r);
    }

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let retrieved = results.iter().filter(|r| r.retrieved).count();
    let with_transcript = results.iter().filter(|r| r.used_transcript).count();

    if json {
        println!("{}", serde_json::json!({"total": total, "passed": passed, "retrieved": retrieved,
            "used_transcript": with_transcript, "results": results}));
    } else {
        println!("mach kb eval-ask: {} questions, rounds<={}", total, rounds);
        let mut cats: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for r in &results {
            let e = cats.entry(r.category.clone()).or_insert((0, 0));
            e.1 += 1;
            if r.passed {
                e.0 += 1;
            }
        }
        for (cat, (p, n)) in &cats {
            println!("  {:<18} {}/{} ({}%)", cat, p, n, if *n > 0 { p * 100 / n } else { 0 });
        }
        println!(
            "  OVERALL            {}/{} ({}%)  retrieved {}/{}  used transcript {}/{}",
            passed,
            total,
            if total > 0 { passed * 100 / total } else { 0 },
            retrieved,
            total,
            with_transcript,
            total
        );
    }
    Ok(())
}

/// `mach kb eval-code` — scores the code-aware `mach kb ask` loop against a
/// fixed question set of real repo-relative paths.
///
/// Unlike `eval-ask` (substring match against the whole gathered pool),
/// this scores the loop's actual contract: a question passes when one of
/// its `expect_paths` is the path component of a citation the final
/// answer actually gave (`code_ask::score_eval_code`). A project named in
/// a question but not registered in this bank is reported and scored a
/// fail rather than aborting the whole run — one bad row in the question
/// set shouldn't hide every other result.
fn cmd_eval_code(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut file: Option<String> = None;
    let mut project_filter: Option<String> = None;
    let mut json = false;
    let mut verbose = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--file" => file = args.next(),
            "--project" => project_filter = args.next(),
            "--json" => json = true,
            "--verbose" | "-v" => verbose = true,
            "-h" | "--help" => {
                println!("usage: mach kb eval-code [--file F] [--project X] [--json] [--verbose]");
                println!("       Scores the code-aware `mach kb ask` loop: a question passes when one");
                println!("       of its expect_paths appears in the answer's citations.");
                println!("       Default question set: ~/{}", code_ask::DEFAULT_EVAL_CODE_PATH);
                return Ok(());
            }
            other => {
                eprintln!("mach kb eval-code: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let path = match file {
        Some(f) => PathBuf::from(f),
        None => {
            let home = std::env::var("HOME").unwrap_or_default();
            Path::new(&home).join(code_ask::DEFAULT_EVAL_CODE_PATH)
        }
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mach kb eval-code: cannot read {}: {}", path.display(), e);
            std::process::exit(1);
        }
    };
    let mut questions = code_ask::parse_eval_code_questions(&content).map_err(to_io)?;
    if let Some(p) = &project_filter {
        questions.retain(|q| &q.project == p);
    }

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    let now = store::now_rfc3339();

    let mut results = Vec::new();
    for q in &questions {
        let Some(p) = store::get_project_by_name(&conn, &q.project).map_err(to_io)? else {
            eprintln!("mach kb eval-code: {}: project '{}' is not registered -- scored a fail", q.id, q.project);
            results.push(code_ask::EvalCodeResult {
                id: q.id.clone(),
                project: q.project.clone(),
                passed: false,
                citations: Vec::new(),
                answer: String::new(),
                rounds: 0,
                tools: Vec::new(),
            });
            continue;
        };
        let out = code_ask::run_code_ask(&conn, &llm, &embedder, &p, &q.query, &now, verbose).map_err(to_io)?;
        let passed = code_ask::score_eval_code(&q.expect_paths, &out.citations);
        if verbose {
            eprintln!("{}: passed={} citations={:?}", q.id, passed, out.citations);
        }
        results.push(code_ask::EvalCodeResult {
            id: q.id.clone(),
            project: q.project.clone(),
            passed,
            citations: out.citations,
            answer: out.answer,
            rounds: out.rounds,
            tools: out.tools,
        });
    }

    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();

    if json {
        println!("{}", serde_json::json!({"total": total, "passed": passed, "results": results}));
    } else {
        println!("mach kb eval-code: {}/{} ({}%)", passed, total, if total > 0 { passed * 100 / total } else { 0 });
        for r in &results {
            println!("  [{}] {} {}", if r.passed { "pass" } else { "FAIL" }, r.id, r.project);
        }
    }
    Ok(())
}

/// `mach kb index-transcripts` — build/refresh the raw-transcript index
/// that `mach kb ask` searches with its `TRANSCRIPT:` step.
///
/// Walks `~/.claude/projects/<project>/<session>.jsonl`, extracting only
/// user and assistant prose (see `transcripts::extract_turn`) and storing
/// it in `CHUNK_CHARS`-sized passages. Incremental by (mtime, size): an
/// unchanged file is skipped without being read, which is what keeps a
/// 962MB / 3274-file corpus a routine command rather than an event.
///
/// Streams line by line and commits per file, so a 194MB transcript costs
/// one line of memory at a time and an interrupted run keeps what it
/// finished.
/// `mach kb transcripts` — search the raw passage index directly.
///
/// Exists as a diagnostic first: `ask`'s `TRANSCRIPT:` step is two LLM
/// calls deep, so asking "did retrieval find the passage, or did the judge
/// word the query badly" used to mean a five-minute eval run per guess.
/// Also useful on its own — it is the closest thing to grepping your own
/// conversation history with meaning rather than regex.
fn cmd_transcripts(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut query: Option<String> = None;
    let mut limit = 8usize;
    let mut json = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(limit),
            "--json" => json = true,
            "-h" | "--help" => {
                println!("usage: mach kb transcripts \"<query>\" [--limit N] [--json]");
                println!("       Hybrid search (cosine + BM25) over indexed session passages.");
                return Ok(());
            }
            other if query.is_none() => query = Some(other.to_string()),
            other => {
                eprintln!("mach kb transcripts: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let Some(query) = query else {
        eprintln!("mach kb transcripts: need a query");
        std::process::exit(1);
    };
    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let qe = embedder.embed(&query).ok();
    let hits = store::search_transcripts(&conn, &query, qe.as_deref(), limit).map_err(to_io)?;
    if json {
        println!("{}", serde_json::to_string(&hits.iter().map(|h| serde_json::json!({
            "id": h.id, "project": h.project, "session": h.session_id, "ts": h.ts, "score": h.score,
            "text": h.text })).collect::<Vec<_>>()).unwrap_or_default());
    } else {
        if hits.is_empty() {
            println!("mach kb transcripts: nothing matched");
        }
        for h in &hits {
            let when = h.ts.as_deref().unwrap_or("undated");
            println!("[{:.3}] {} {} {}", h.score, h.project, when, h.session_id);
            for line in h.text.lines().take(4) {
                println!("    {}", line.chars().take(160).collect::<String>());
            }
            println!();
        }
    }
    Ok(())
}

fn cmd_index_transcripts(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut reindex_all = false;
    let mut embed_missing = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--all" => reindex_all = true,
            "--embed-missing" => embed_missing = true,
            "-h" | "--help" => {
                println!("usage: mach kb index-transcripts [--all] [--embed-missing]");
                println!("       --embed-missing only fills in absent passage embeddings, without");
                println!("       re-reading any transcript; use it when the index predates embeddings.");
                println!("       Indexes session transcripts for `mach kb ask`. Unchanged files are");
                println!("       skipped; --all re-reads every file even when mtime and size match.");
                return Ok(());
            }
            other => {
                eprintln!("mach kb index-transcripts: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let root = Path::new(&home).join(".claude/projects");
    if !root.is_dir() {
        eprintln!("mach kb index-transcripts: no {}", root.display());
        return Ok(());
    }

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let now = store::now_rfc3339();

    if embed_missing {
        let pending = store::transcript_chunks_missing_embedding(&conn, usize::MAX).map_err(to_io)?;
        let total = pending.len();
        let (mut done, mut failed) = (0usize, 0usize);
        for (id, text) in pending {
            match embedder.embed(&text) {
                Ok(v) => {
                    store::set_transcript_chunk_embedding(&conn, id, &v).map_err(to_io)?;
                    done += 1;
                }
                // Leave it NULL and carry on: the passage stays lexically
                // searchable and the next run picks it up.
                Err(_) => failed += 1,
            }
            if done > 0 && done % 500 == 0 {
                eprintln!("mach kb index-transcripts: embedded {}/{}", done, total);
            }
        }
        println!("mach kb index-transcripts: embedded {} of {} missing ({} failed)", done, total, failed);
        return Ok(());
    }
    let (mut scanned, mut indexed, mut skipped, mut chunks_total) = (0usize, 0usize, 0usize, 0usize);
    let mut machine = 0usize;

    let projects = std::fs::read_dir(&root)?;
    for pdir in projects.flatten() {
        if !pdir.path().is_dir() {
            continue;
        }
        let dir_name = pdir.file_name().to_string_lossy().to_string();
        let project = transcripts::project_from_dir(&dir_name);
        // Recursive: subagent transcripts live one level deeper
        // (<session>/subagents/agent-*.jsonl) and are 652 of this corpus's
        // 3274 files. A subagent's findings are part of what was said.
        let mut files: Vec<PathBuf> = Vec::new();
        collect_jsonl(&pdir.path(), &mut files);
        for path in files {
            scanned += 1;
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let size = meta.len() as i64;
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let path_s = path.to_string_lossy().to_string();
            if !reindex_all && store::transcript_file_current(&conn, &path_s, mtime, size).map_err(to_io)? {
                skipped += 1;
                continue;
            }
            let session_id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let Ok(file) = std::fs::File::open(&path) else { continue };
            let mut turns = Vec::new();
            for line in io::BufRead::lines(io::BufReader::new(file)).map_while(Result::ok) {
                if let Some(t) = transcripts::extract_turn(&line) {
                    turns.push(t);
                }
            }
            // mach's own chore transcripts are machinery, not conversation.
            // Indexed anyway (rather than `continue`d before this point) so
            // that a file which WAS indexed before this filter existed gets
            // its chunks cleared by the empty replace below.
            let chunks =
                if transcripts::is_machine_session(&turns) { Vec::new() } else { transcripts::chunk_turns(&turns) };
            if chunks.is_empty() {
                machine += 1;
            }
            // One embedding per passage, so transcript search has a
            // semantic channel and not only BM25. A failure here stores
            // NULL and the passage stays lexically searchable.
            let embeddings: Vec<Option<Vec<f32>>> =
                chunks.iter().map(|c| embedder.embed(&c.text).ok()).collect();
            let n = store::replace_transcript_chunks(
                &conn,
                &path_s,
                &session_id,
                &project,
                mtime,
                size,
                &chunks,
                &embeddings,
                &now,
            )
            .map_err(to_io)?;
            indexed += 1;
            chunks_total += n;
        }
    }

    let (files, total_chunks) = store::transcript_index_stats(&conn).map_err(to_io)?;
    println!(
        "mach kb index-transcripts: scanned={} indexed={} skipped={} machine={} new_chunks={} (index now {} files, {} chunks)",
        scanned, indexed, skipped, machine, chunks_total, files, total_chunks
    );
    Ok(())
}

/// `mach kb index [--project P] [--budget N] [--path-prefix P]
/// [--history-only] [--dry-run]` -- the incremental code index (scope
/// pass, tree-sitter chunks, per-file LLM context headers, embeddings,
/// symbol graph, monthly commit-history summaries). `--history-only`
/// (requires `--project`, rejected together with `--path-prefix`) skips
/// straight to the commit-history stage for that one project.
/// Orchestration lives in `code_index::job`; this function only parses
/// args, wires up the real `ProcessReflectLlm`/`OllamaEmbedder`, and
/// prints. `index status` and `index scope` are separate sub-subcommands
/// (same nesting style as `mach kb graph audit`).
fn cmd_index(args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut args = args.peekable();
    match args.peek().map(|s| s.as_str()) {
        Some("status") => {
            args.next();
            return cmd_index_status(args);
        }
        Some("scope") => {
            args.next();
            return cmd_index_scope(args);
        }
        _ => {}
    }

    let opts = match parse_index_args(args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            print_index_help();
            return Ok(());
        }
        Err(msg) => {
            eprintln!("mach kb index: {}", msg);
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;

    if opts.dry_run {
        let plans = match job::plan_index(&conn, &opts) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("mach kb index: {}", e);
                std::process::exit(1);
            }
        };
        let mut total_calls = 0usize;
        for p in &plans {
            if let Some(reason) = &p.skipped_reason {
                println!("{:<20} skipped ({})", p.project, reason);
                continue;
            }
            if opts.history_only {
                println!("{:<20} months={} calls={}", p.project, p.months_to_summarize, p.calls_needed);
            } else {
                println!(
                    "{:<20} files={} delete={} move={} modules-stale={} months={} calls={}{}",
                    p.project,
                    p.files_to_index,
                    p.files_to_delete,
                    p.files_moved,
                    p.modules_stale,
                    p.months_to_summarize,
                    p.calls_needed,
                    if p.needs_scope { " [scope pass needed]" } else { "" }
                );
            }
            total_calls += p.calls_needed;
        }
        println!("mach kb index --dry-run: {} project(s), {} call(s) estimated, nothing changed", plans.len(), total_calls);
        return Ok(());
    }

    // Same lock the systemd units take around every kb-writing command, so
    // a manual run never overlaps the nightly index or reflect. Held until
    // this function returns.
    let _lock = match acquire_kb_job_lock() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("mach kb index: {}", e);
            std::process::exit(1);
        }
    };
    let llm = ProcessReflectLlm::new();
    let embedder = OllamaEmbedder::new();
    let report = match job::run_index(&conn, &llm, &embedder, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mach kb index: {}", e);
            std::process::exit(1);
        }
    };
    let mut errored = 0usize;
    for p in &report.projects {
        if let Some(err) = &p.error {
            errored += 1;
            println!("{:<20} error: {} (run continued with the next project)", p.project, err);
            continue;
        }
        if let Some(reason) = &p.skipped_reason {
            println!("{:<20} skipped ({})", p.project, reason);
            continue;
        }
        println!(
            "{:<20} chunked={} deleted={} moved={} secret-skipped={} headers={} headers-failed={} months={} months-failed={} symbols={} edges={}{}{}{}",
            p.project,
            p.files_chunked,
            p.files_deleted,
            p.files_moved,
            p.files_skipped_secret,
            p.headers_attempted,
            p.headers_failed,
            p.months_summarized,
            p.months_failed,
            p.symbols_total,
            p.edges_total,
            match p.summaries_failed + p.modules_failed {
                0 => String::new(),
                n => format!(" summaries-failed={}", n),
            },
            if p.scope_ran { " scope-pass" } else { "" },
            if p.head_advanced { " head-advanced" } else { "" }
        );
    }
    if !report.embedder_up {
        println!("index: embedder unreachable — no header calls made this run (chunking still ran)");
    } else if report.backfill_embedded > 0 {
        println!("index: backfilled {} missing embedding(s)", report.backfill_embedded);
    }
    if report.calls_failed > 0 {
        println!(
            "index: {} of {} call(s) failed (last: {})",
            report.calls_failed,
            report.calls_used,
            report.last_error.as_deref().unwrap_or("unknown")
        );
    }
    if report.stopped_on_failures {
        println!(
            "index: stopped after {} failed calls in a row — claude -p looks down or throttled; resumes next run",
            job::MAX_CONSECUTIVE_FAILURES
        );
    } else if report.budget_reached {
        println!("index: budget reached ({} calls) — resumes next run", report.calls_used);
    } else {
        println!("mach kb index: done ({} call(s) used of {})", report.calls_used, report.budget);
    }
    // A targeted run (--project) that failed should fail loudly; the
    // nightly all-projects run stays exit 0 (errors are in its output).
    if errored > 0 && opts.project.is_some() {
        std::process::exit(1);
    }
    Ok(())
}

/// Takes `~/.local/share/mach/kb-job.lock`, the lock the systemd units
/// hold (via flock(1)) around every kb-writing command. Waits if another
/// job holds it, saying so first so a manual run isn't silently stuck.
fn acquire_kb_job_lock() -> io::Result<std::fs::File> {
    let path = store::db_path().map_err(to_io)?.with_file_name("kb-job.lock");
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            eprintln!("mach kb index: another kb job holds {} — waiting for it to finish", path.display());
            file.lock()?;
        }
        Err(std::fs::TryLockError::Error(e)) => return Err(e),
    }
    Ok(file)
}

fn print_index_help() {
    println!("usage: mach kb index [--project P] [--budget N] [--path-prefix P] [--history-only] [--dry-run]");
    println!("       mach kb index status [--project P]");
    println!("       mach kb index scope <project> [--set <dir>=<category>]...");
    println!();
    println!("       Incrementally indexes every registered git project (others are");
    println!("       reported skipped, never touched): a scope pass classifies");
    println!("       directories, changed in-scope files are chunked (tree-sitter),");
    println!("       each dirty file gets one LLM context-header call plus embeddings,");
    println!("       a symbol graph (definitions/calls/includes/imports/inherits) is");
    println!("       extracted for free, and each project's monthly commit-history is");
    println!("       summarized (last, budget permitting).");
    println!("       --budget caps `claude -p` calls for the whole invocation (default");
    println!("       {}); the run stops cleanly once spent and resumes next time.", job::DEFAULT_BUDGET);
    println!("       --path-prefix (requires --project) limits changed-file detection and");
    println!("       the dirty/summary backlog to that subtree, never advances the");
    println!("       indexed-head watermark, and skips");
    println!("       the commit-history stage entirely (a partial file view has no");
    println!("       meaningful month to report on).");
    println!("       --history-only (requires --project, rejected together with");
    println!("       --path-prefix) skips the scope/chunk/header/summary stages entirely");
    println!("       and runs only the commit-history stage for that one project --");
    println!("       useful to backfill every month's history without paying for a full");
    println!("       index first.");
    println!("       --dry-run prints the plan (files, stale modules and changed history");
    println!("       months per project, calls needed; with --history-only just the");
    println!("       months) and changes nothing.");
    println!("       Files matching secret patterns (.env, *.pem, *.key, id_rsa*, tokens,");
    println!("       private keys, ...) are recorded skipped and never chunked or sent.");
    println!("       One project's error is reported and the run moves on to the next.");
    println!("       Run output per project: headers=/months= are calls attempted this");
    println!("       run, headers-failed=/months-failed= how many of those failed");
    println!("       (timeout, error; nothing written, retried next run); symbols=/edges=");
    println!("       are the project's current code_symbols/code_edges row totals (not");
    println!("       just this run's delta).");
    println!("       The run stops early after {} failed calls in a row.", job::MAX_CONSECUTIVE_FAILURES);
    println!("       Takes ~/.local/share/mach/kb-job.lock (waits if another kb job");
    println!("       holds it), so it never overlaps the nightly index or reflect.");
}

/// Parses `mach kb index [--project P] [--budget N] [--path-prefix P]
/// [--history-only] [--dry-run]`. `Ok(None)` for `--help`. Errors (as a
/// message) on an unknown flag, a missing or non-numeric `--budget`, a
/// flag missing its value, `--path-prefix` without `--project` (a prefix
/// only means something inside one repo), `--history-only` without
/// `--project`, or `--history-only` combined with `--path-prefix`.
fn parse_index_args(mut args: impl Iterator<Item = String>) -> Result<Option<job::IndexOptions>, String> {
    let mut opts = job::IndexOptions::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--project" => opts.project = Some(args.next().ok_or("--project requires a project name")?),
            "--budget" => {
                let v = args.next().ok_or("--budget requires a number")?;
                opts.budget = v.parse::<usize>().map_err(|_| format!("--budget must be a non-negative integer, got '{}'", v))?;
            }
            "--path-prefix" => opts.path_prefix = Some(args.next().ok_or("--path-prefix requires a path")?),
            "--dry-run" => opts.dry_run = true,
            "--history-only" => opts.history_only = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("unexpected argument '{}'", other)),
        }
    }
    if opts.path_prefix.is_some() && opts.project.is_none() {
        return Err("--path-prefix requires --project".to_string());
    }
    if opts.history_only && opts.project.is_none() {
        return Err("--history-only requires --project".to_string());
    }
    if opts.history_only && opts.path_prefix.is_some() {
        return Err("--history-only cannot be combined with --path-prefix".to_string());
    }
    Ok(Some(opts))
}

fn cmd_index_status(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut project: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--project" => {
                let Some(p) = args.next() else {
                    eprintln!("mach kb index status: --project requires a project name");
                    std::process::exit(1);
                };
                project = Some(p);
            }
            "-h" | "--help" => {
                println!("usage: mach kb index status [--project P]");
                println!("       per project: head vs indexed head, files by status (skipped = secret");
                println!("       path/content, never stored), chunk count, embedded %, how many files");
                println!("       gave up on ever getting a summary (summaries_given_up), and the");
                println!("       symbol graph (symbols, edges, edges_resolved %)");
                return Ok(());
            }
            other => {
                eprintln!("mach kb index status: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let conn = store::open().map_err(to_io)?;
    let statuses = match job::all_project_statuses(&conn, project.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mach kb index status: {}", e);
            std::process::exit(1);
        }
    };
    if statuses.is_empty() {
        println!("mach kb index status: no registered projects");
    }
    for s in &statuses {
        if let Some(reason) = &s.skipped_reason {
            println!("{:<20} skipped ({})", s.project, reason);
            continue;
        }
        let short = |sha: &str| sha[..sha.len().min(10)].to_string();
        let head = s.head.as_deref().map(short).unwrap_or_else(|| "(no commits)".to_string());
        let indexed_head = s.code_indexed_head.as_deref().map(short).unwrap_or_else(|| "(never indexed)".to_string());
        let behind = if s.head != s.code_indexed_head && s.code_indexed_head.is_some() { " [BEHIND]" } else { "" };
        println!(
            "{:<20} head={} indexed_head={}{} indexed={} fallback={} dirty={} skipped={} chunks={} embedded={:.0}% summaries_given_up={} symbols={} edges={} edges_resolved={:.0}%",
            s.project,
            head,
            indexed_head,
            behind,
            s.indexed,
            s.fallback,
            s.dirty,
            s.skipped,
            s.chunks,
            s.embedded_pct,
            s.summaries_given_up,
            s.symbols,
            s.edges,
            s.edges_resolved_pct
        );
        if !s.skip_reasons.is_empty() {
            let parts: Vec<String> = s.skip_reasons.iter().map(|(r, n)| format!("{}={}", r, n)).collect();
            println!("{:<20}   skipped: {}", "", parts.join(" "));
        }
    }
    Ok(())
}

fn cmd_index_scope(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut project_name: Option<String> = None;
    let mut sets: Vec<(String, String)> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--set" => {
                let Some(kv) = args.next() else {
                    eprintln!("mach kb index scope: --set requires <dir>=<category>");
                    std::process::exit(1);
                };
                let Some((dir, cat)) = kv.split_once('=') else {
                    eprintln!("mach kb index scope: --set value must be <dir>=<category>, got '{}'", kv);
                    std::process::exit(1);
                };
                sets.push((dir.to_string(), cat.to_string()));
            }
            "-h" | "--help" => {
                println!("usage: mach kb index scope <project> [--set <dir>=<category>]...");
                println!("       no --set: prints the project's current per-directory categories");
                println!("       --set: records a manual (\"user\") override the scope pass never");
                println!("       clobbers. Categories: product, tests, docs, vendored, generated,");
                println!("       assets, build-output.");
                return Ok(());
            }
            other => {
                if project_name.is_none() {
                    project_name = Some(other.to_string());
                } else {
                    eprintln!("mach kb index scope: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }
    let Some(project_name) = project_name else {
        eprintln!("mach kb index scope: missing <project>");
        std::process::exit(1);
    };
    let conn = store::open().map_err(to_io)?;
    let Some(project) = store::get_project_by_name(&conn, &project_name).map_err(to_io)? else {
        eprintln!("mach kb index scope: unknown project '{}'", project_name);
        std::process::exit(1);
    };
    if sets.is_empty() {
        let rows = store::code_scope_get(&conn, project.id).map_err(to_io)?;
        if rows.is_empty() {
            println!("mach kb index scope {}: no scope decisions yet (run `mach kb index` first)", project_name);
        }
        for r in rows {
            println!("{:<30} {:<12} ({})", r.dir, r.category, r.source);
        }
    } else {
        // Validate every pair against the committed tree before writing any.
        let root = Path::new(&project.root_path);
        if !crate::code_index::git::Repo::is_repo(root) {
            eprintln!("mach kb index scope: '{}' is not a git repo", project.root_path);
            std::process::exit(1);
        }
        let tracked = crate::code_index::git::Repo::new(root).tracked_files().map_err(to_io)?;
        let mut validated = Vec::with_capacity(sets.len());
        for (dir, cat) in &sets {
            match crate::code_index::scope::validate_user_scope_set(&tracked, dir, cat) {
                Ok(v) => validated.push(v),
                Err(e) => {
                    eprintln!("mach kb index scope: --set {}={}: {}", dir, cat, e);
                    std::process::exit(1);
                }
            }
        }
        let now = store::now_rfc3339();
        for (dir, cat) in &validated {
            store::code_scope_set(&conn, project.id, dir, cat.as_str(), "user", &now).map_err(to_io)?;
            println!("{:<30} -> {} (user)", dir, cat.as_str());
        }
    }
    Ok(())
}

/// Registers one project, re-tagging its memories when the fingerprint was
/// already known under a different name. Returns rows re-tagged.
///
/// Split out from `cmd_projects` so the rename path is testable without a
/// filesystem or a git repo.
/// Returns `(renamed, retagged)` as the two distinct numbers they are: a
/// rename either happened or it didn't (`previous.is_some()`), independent
/// of how many rows `retag_project` found to move. A project with zero
/// memories yet still counts as renamed even though nothing was retagged --
/// conflating the two used to print a rename event as "0 renamed".
fn refresh_one_project(
    conn: &Connection,
    fingerprint: &str,
    name: &str,
    root_path: &str,
    card: Option<&str>,
    now: &str,
) -> Result<(bool, usize), KbError> {
    let (id, previous) = store::upsert_project(conn, fingerprint, name, root_path, now)?;
    let mut retagged = 0;
    let renamed = previous.is_some();
    if let Some(old) = previous {
        // A rename or a case change. Automatic because the alternative is
        // an index that stays orphaned until someone notices, and because
        // it is semantically safe: same repo, same facts, new label.
        retagged = store::retag_project(conn, &old, name)?;
        eprintln!("mach kb projects: {} -> {} ({} references re-tagged)", old, name, retagged);
    }
    if let Some(card) = card {
        store::set_project_card(conn, id, card)?;
    }
    Ok((renamed, retagged))
}

/// `mach kb projects` — the registry: rename detection, card rebuilds and
/// index-drift reporting. No LLM call anywhere in here, so it is safe on
/// the reflect timer.
fn cmd_projects(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let sub = args.next().unwrap_or_else(|| "list".to_string());
    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();

    match sub.as_str() {
        "-h" | "--help" => {
            println!("usage: mach kb projects [list|refresh|mark-indexed <name>|forget <name>]");
            println!("       list          registered projects, their cards and drift");
            println!("       refresh       detect renames, rebuild cards, recompute drift (no LLM)");
            println!("       mark-indexed  reset a project's drift watermark after re-indexing");
            println!("       forget        drop a project's registry row (its project-index memories stay)");
            Ok(())
        }
        "list" => {
            for p in store::list_projects(&conn).map_err(to_io)? {
                let root = Path::new(&p.root_path);
                let drift = projects::drift_state(
                    p.indexed_commits,
                    projects::commit_count(root),
                    p.indexed_at.as_deref(),
                    &now,
                );
                let state = match drift {
                    projects::DriftState::NeverIndexed => "never indexed".to_string(),
                    projects::DriftState::Fresh => "fresh".to_string(),
                    projects::DriftState::Drifted { commits, days } => {
                        format!("DRIFTED {} commits / {:.0} days", commits, days)
                    }
                };
                let present = if root.is_dir() { "" } else { " [MISSING ON DISK]" };
                println!("{:<20} {:<28} {}{}", p.name, state, p.fingerprint, present);
            }
            Ok(())
        }
        "refresh" => {
            let home = std::env::var("HOME").unwrap_or_default();
            // Registered projects first, then discover anything under
            // ~/programming that already has index memories.
            let mut roots: Vec<PathBuf> =
                store::list_projects(&conn).map_err(to_io)?.into_iter().map(|p| PathBuf::from(p.root_path)).collect();
            let prog = Path::new(&home).join("programming");
            if let Ok(entries) = std::fs::read_dir(&prog) {
                for e in entries.flatten() {
                    if e.path().is_dir() && !roots.contains(&e.path()) {
                        roots.push(e.path());
                    }
                }
            }
            // mach is indexed but lives outside ~/programming.
            let mach = Path::new(&home).join(".config/mach");
            if mach.is_dir() && !roots.contains(&mach) {
                roots.push(mach);
            }

            let (mut seen, mut renamed, mut retagged_total, mut cards) = (0usize, 0usize, 0usize, 0usize);
            for root in roots {
                if !root.is_dir() {
                    continue;
                }
                let name = projects::normalize_name(&root);
                // Only track projects the bank actually knows something
                // about; registering every directory would fill this with
                // scratch dirs. A real query failure is surfaced loudly and
                // this project is skipped rather than silently treated as
                // having zero memories.
                let known: i64 = match store::count_project_memories(&conn, &name) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("mach kb projects: skipping '{}' at {} — could not count its memories: {}", name, root.display(), e);
                        continue;
                    }
                };
                let fingerprint = projects::fingerprint_for(&root).key();
                let registered = store::get_project_by_fingerprint(&conn, &fingerprint).map_err(to_io)?.is_some();
                if known == 0 && !registered {
                    continue;
                }
                // `name` is not a unique key in the schema: two different
                // fingerprints could otherwise both register under it, and
                // their memories would then share one ambiguous project
                // tag with no automatic way back apart. If some other
                // fingerprint already holds this name, skip rather than
                // silently create a second, colliding row.
                if let Some(existing) =
                    store::get_project_by_name(&conn, &name).map_err(to_io)?.filter(|p| p.fingerprint != fingerprint)
                {
                    eprintln!(
                        "mach kb projects: skipping '{}' at {} — name already registered for {} ({})",
                        name,
                        root.display(),
                        existing.root_path,
                        existing.fingerprint
                    );
                    continue;
                }
                seen += 1;
                // `build_card` cannot fail — an unreadable directory just
                // yields a card with no layout or manifest lines. Writing
                // that over a good card would silently lose the map, so a
                // degenerate card is skipped and the previous one stands.
                let built = projects::build_card(&root);
                let card = if built.lines().count() > 1 { Some(built) } else { None };
                let (did_rename, rows_retagged) = refresh_one_project(
                    &conn,
                    &fingerprint,
                    &name,
                    &root.to_string_lossy(),
                    card.as_deref(),
                    &now,
                )
                .map_err(to_io)?;
                if did_rename {
                    renamed += 1;
                }
                retagged_total += rows_retagged;
                if card.is_some() {
                    cards += 1;
                }
            }
            println!(
                "mach kb projects: {} tracked, {} renamed ({} references re-tagged), {} cards rebuilt",
                seen, renamed, retagged_total, cards
            );
            Ok(())
        }
        "mark-indexed" => {
            let Some(name) = args.next() else {
                eprintln!("mach kb projects mark-indexed: need a project name");
                std::process::exit(1);
            };
            let name = name.to_lowercase();
            let Some(p) = store::get_project_by_name(&conn, &name).map_err(to_io)? else {
                eprintln!("mach kb projects mark-indexed: '{}' is not registered", name);
                std::process::exit(1);
            };
            let root = PathBuf::from(&p.root_path);
            let is_git = p.fingerprint.starts_with("git:");
            let commits = projects::commit_count(&root);
            let wrote = store::mark_project_indexed_if_readable(&conn, p.id, is_git, commits, &now).map_err(to_io)?;
            if !wrote {
                eprintln!(
                    "mach kb projects mark-indexed: cannot read commit count for '{}' at {} — refusing to touch the watermark",
                    name,
                    root.display()
                );
                std::process::exit(1);
            }
            println!("mach kb projects: {} watermark reset", name);
            Ok(())
        }
        "forget" => {
            let Some(name) = args.next() else {
                eprintln!("mach kb projects forget: need a project name");
                std::process::exit(1);
            };
            let name = name.to_lowercase();
            let Some(p) = store::get_project_by_name(&conn, &name).map_err(to_io)? else {
                eprintln!("mach kb projects forget: '{}' is not registered", name);
                std::process::exit(1);
            };
            // The remaining-memory count is informational only: forgetting
            // never touches memories, so this is safe to compute either
            // before or after the delete.
            let remaining: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM memories WHERE project = ?1 OR source = ?2",
                    rusqlite::params![name, format!("project-index:{}", name)],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            store::forget_project(&conn, p.id).map_err(to_io)?;
            println!(
                "mach kb projects: {} forgotten (registry row only — {} project-index memories untouched)",
                name, remaining
            );
            Ok(())
        }
        other => {
            eprintln!("mach kb projects: unknown subcommand '{}'", other);
            std::process::exit(1);
        }
    }
}

/// Project resolution for `mach kb ask`/`--project`: an explicit flag wins,
/// otherwise the current directory is checked against the `projects`
/// registry the same way every other command with a `--cwd`-shaped
/// resolution does (`store::project_for_path`) — a registered root is an
/// actual identity claim, not a guess. `None` (no flag, cwd not inside any
/// registered root, or no readable cwd at all) means "no project", and
/// `cmd_ask` falls through to the unchanged memory-only path.
fn resolve_ask_project(conn: &Connection, explicit: Option<&str>) -> Result<Option<store::ProjectRow>, KbError> {
    if let Some(name) = explicit {
        // An explicit --project that names nothing is a typo, not "no
        // project": silently falling back to memory-only answers would hide it.
        return match store::get_project_by_name(conn, name)? {
            Some(p) => Ok(Some(p)),
            None => Err(KbError::Other(format!("unknown project '{}' (see `mach kb projects`)", name))),
        };
    }
    match std::env::current_dir() {
        Ok(cwd) => store::project_for_path(conn, &cwd.to_string_lossy()),
        Err(_) => Ok(None),
    }
}

/// Parses a `--rounds` value: must be present and a non-negative integer.
fn parse_rounds_value(v: Option<String>) -> Result<usize, String> {
    let v = v.ok_or("--rounds requires a number")?;
    v.parse::<usize>().map_err(|_| format!("--rounds must be a non-negative integer, got '{}'", v))
}

fn cmd_ask(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut question: Option<String> = None;
    let mut rounds = ask::MAX_ROUNDS;
    let mut rounds_explicit: Option<usize> = None;
    let mut json = false;
    let mut verbose = false;
    let mut project: Option<String> = None;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--rounds" => match parse_rounds_value(args.next()) {
                Ok(n) => {
                    rounds = n;
                    rounds_explicit = Some(n);
                }
                Err(msg) => {
                    eprintln!("mach kb ask: {}", msg);
                    std::process::exit(1);
                }
            },
            "--project" => {
                let Some(p) = args.next() else {
                    eprintln!("mach kb ask: --project requires a project name");
                    std::process::exit(1);
                };
                project = Some(p);
            }
            "--json" => json = true,
            "--verbose" | "-v" => verbose = true,
            "-h" | "--help" => {
                println!("usage: mach kb ask \"<question>\" [--project P] [--rounds N] [--json] [--verbose]");
                println!("       With a registered project (--project, or detected from the current");
                println!("       directory) that has a code index: an agentic loop over search/read/");
                println!(
                    "       symbol/history tools, up to {} rounds (--rounds lowers it), citing path:line@sha7.",
                    code_ask::MAX_ROUNDS
                );
                println!("       Without a project, or one with no code index yet: searches memories,");
                println!("       judges whether the result answers the question, and if not rewords the");
                println!("       query or hops to an entity — up to {} rounds — then answers with", ask::MAX_ROUNDS);
                println!("       citations. Spends LLM calls either way: this is the deliberate path,");
                println!("       not the hook.");
                return Ok(());
            }
            other if question.is_none() => question = Some(other.to_string()),
            other => {
                eprintln!("mach kb ask: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let Some(question) = question else {
        eprintln!("mach kb ask: need a question");
        std::process::exit(1);
    };
    let rounds = rounds.clamp(1, ask::MAX_ROUNDS);

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    let now = store::now_rfc3339();

    // Code-aware path: only when a project actually resolves AND its code
    // index has something in it. Anything else (no project, or a
    // registered project nobody has run `mach kb index` on yet) falls
    // through to the memory-only path below completely unchanged, byte for
    // byte, from before this branch existed.
    let resolved = match resolve_ask_project(&conn, project.as_deref()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mach kb ask: {}", e);
            std::process::exit(1);
        }
    };
    if let Some(p) = resolved {
        if code_ask::has_code_chunks(&conn, p.id).map_err(to_io)? {
            // --rounds is honored on the code path too (clamped to its own
            // cap); without it the code loop uses its full MAX_ROUNDS.
            let code_rounds = rounds_explicit.unwrap_or(code_ask::MAX_ROUNDS);
            let out = code_ask::run_code_ask_with_rounds(&conn, &llm, &embedder, &p, &question, code_rounds, &now, verbose)
                .map_err(to_io)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"question": question, "project": p.name, "answer": out.answer,
                        "citations": out.citations, "trail": out.trail})
                );
            } else {
                println!("{}", out.answer);
                if !out.citations.is_empty() {
                    println!("\ncitations: {}", out.citations.join(", "));
                }
            }
            return Ok(());
        }
    }

    let out = run_ask_gather(&conn, &embedder, &llm, &question, rounds, &now, verbose)?;
    let (evidence, passages, trail) = (out.evidence, out.passages, out.trail);

    if evidence.is_empty() && passages.is_empty() {
        if json {
            println!("{}", serde_json::json!({"question": question, "answer": null, "sources": [], "trail": trail}));
        } else {
            println!("mach kb ask: nothing in the bank clears the floor for that question");
        }
        return Ok(());
    }

    let prompt = ask::build_answer_prompt(&question, &evidence, &passages);
    let answer = match llm.call("sonnet", &prompt, reflect::TIMEOUT_SONNET) {
        Ok(a) => a.trim().to_string(),
        Err(e) => {
            eprintln!("mach kb ask: answer call failed: {}", e);
            std::process::exit(1);
        }
    };
    let allowed: Vec<i64> = evidence.iter().map(|(id, _)| *id).collect();
    let sources = ask::cited_ids(&answer, &allowed);

    if json {
        println!(
            "{}",
            serde_json::json!({"question": question, "answer": answer, "sources": sources, "trail": trail})
        );
    } else {
        println!("{}", answer);
        if !sources.is_empty() {
            let list: Vec<String> = sources.iter().map(|id| id.to_string()).collect();
            println!("\nsources: {}", list.join(", "));
        }
    }
    Ok(())
}

fn cmd_cards(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut all = false;
    let mut limit: Option<usize> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--all" => all = true,
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()),
            "-h" | "--help" => {
                println!("usage: mach kb cards [--all] [--limit N]");
                println!("       Builds the entity cards currently due. Without --all it does one");
                println!("       pass ({} entities); with --all it repeats until none are due.", store::CARD_MAX_PER_RUN);
                return Ok(());
            }
            other => {
                eprintln!("mach kb cards: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let llm = ProcessReflectLlm::new();
    let mut total_examined = 0usize;
    let mut total_built = 0usize;

    loop {
        let now = store::now_rfc3339();
        let (examined, built, error) = run_entity_card_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
        total_examined += examined;
        total_built += built;
        if let Some(e) = &error {
            eprintln!("mach kb cards: a claude call failed — those entities stay due: {}", e);
        }
        let due = store::entity_card_candidates(&conn, 1000).map_err(to_io)?.len();
        println!("mach kb cards: examined={} built={} still_due={}", total_examined, total_built, due);
        // Stop when asked to, when nothing is due, or when a pass achieved
        // nothing (a persistent failure or a NONE-verdict entity would
        // otherwise loop forever).
        let hit_limit = limit.map(|n| total_built >= n).unwrap_or(false);
        if !all || due == 0 || examined == 0 || hit_limit {
            break;
        }
    }
    Ok(())
}

/// Rough token estimate: English prose runs about four characters per
/// token, close enough to spend a context budget against without pulling
/// in a tokenizer for the model that will actually read it.
pub fn est_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// Reservation cap on how much of the budget the project card may claim in
/// `pack_to_budget`, expressed as numerator/denominator rather than a float
/// so the comparison stays in integer token counts.
///
/// The card is allowed to cost about what one ranked hit costs, and no
/// more. Before this cap it was charged FIRST against the whole budget, so
/// a long card could evict three hits — measured live, `project="umoja"`
/// turned hits `[206, 81, 207]` into `[206, 81]`. Displacing roughly one
/// hit is the intended trade, not a defect: the card exists precisely to
/// replace the derivable-structure memories that used to occupy those
/// slots, and it is re-derivable from the repo at any time while a ranked
/// memory is not.
///
/// The fraction is a third rather than a quarter because of arithmetic,
/// not taste. Real cards measure 306-469 chars, and `est_tokens` is
/// `chars/4`, so they cost 77-118 tokens against a 400-token budget. A
/// quarter (100) cut straight through that range: zenith kept its card and
/// umoja and helios silently lost theirs. A third (133) clears every card
/// `build_card` can emit, because `projects::CARD_MAX_CHARS` is capped so
/// that `CARD_MAX_CHARS / 4` stays under it — the two constants are
/// consistent by construction, not by luck. Change one and the
/// `the_card_cap_and_the_budget_reservation_stay_consistent` test fails.
pub const PROJECT_CARD_BUDGET_NUMERATOR: usize = 1;
pub const PROJECT_CARD_BUDGET_DENOMINATOR: usize = 3;

#[cfg(test)]
mod project_card_budget_tests {
    use super::*;

    /// The two constants are only safe together. `build_card` truncates to
    /// `CARD_MAX_CHARS`, and `pack_to_budget` drops — never truncates — a
    /// card that exceeds its reservation, so a cap above the reservation
    /// means the longest cards vanish entirely instead of being shortened,
    /// silently and only for the projects with the most to say.
    #[test]
    fn the_card_cap_and_the_budget_reservation_stay_consistent() {
        const RECALL_BUDGET: usize = 400; // claude-hooks/kb-recall.py TOKEN_BUDGET
        let reservation =
            RECALL_BUDGET * PROJECT_CARD_BUDGET_NUMERATOR / PROJECT_CARD_BUDGET_DENOMINATOR;
        let longest_card = "x".repeat(crate::projects::CARD_MAX_CHARS);
        assert!(
            est_tokens(&longest_card) <= reservation,
            "a maximum-length card costs {} tokens but the reservation is {}: the longest \
             cards would be dropped whole rather than truncated",
            est_tokens(&longest_card),
            reservation
        );
    }
}

/// Greedy token-budget packing (Hindsight's final recall stage): keep
/// results in rank order until the budget is spent, instead of keeping a
/// fixed COUNT. A count is the wrong unit for injection -- four one-line
/// preferences and four paragraph-long project-index facts cost the same
/// four slots and wildly different context.
///
/// Packed in this order: the project card first (a guaranteed same-project
/// match by construction, not a similarity guess), then entity cards (each
/// a consolidation of many memories, so per token they say the most among
/// what's left), then hits. Connections are left alone (they are one short
/// line each and never the bulk of an injection).
pub fn pack_to_budget(resp: SearchResponse, budget: usize) -> SearchResponse {
    let mut spent = 0usize;
    let project_card = match resp.project_card {
        Some(c) if est_tokens(&c) <= budget * PROJECT_CARD_BUDGET_NUMERATOR / PROJECT_CARD_BUDGET_DENOMINATOR => {
            spent += est_tokens(&c);
            Some(c)
        }
        _ => None,
    };
    let mut cards = Vec::new();
    for c in resp.cards {
        let cost = est_tokens(&c.text);
        if spent + cost <= budget {
            spent += cost;
            cards.push(c);
        }
    }
    let mut hits = Vec::new();
    for h in resp.hits {
        let cost = est_tokens(&h.content);
        if spent + cost > budget {
            continue; // skip, don't stop: a later shorter hit may still fit
        }
        spent += cost;
        hits.push(h);
    }
    SearchResponse { hits, connections: resp.connections, cards, project_card }
}

/// `mach kb eval` — score retrieval against a fixed question set.
///
/// Runs every question through the SAME path and settings the recall hook
/// uses (`search_hits` with the hook's limit and score floor), so the
/// number reflects what would actually be injected into a session rather
/// than what a hand-tuned query could dig out. Read-only: nothing is
/// touched, reinforced or logged.
///
/// The default limit/min-score deliberately mirror `kb-recall.py`'s
/// constants; `--limit`/`--min-score` exist to sweep them, which is the
/// whole point of having a score at all.
fn cmd_eval(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    // Mirrors kb-recall.py's SEARCH_LIMIT / SCORE_THRESHOLD.
    let mut limit: usize = 4;
    let mut min_score: f32 = 0.45;
    let mut budget: Option<usize> = None;
    let mut file: Option<String> = None;
    let mut json = false;
    let mut verbose = false;

    while let Some(a) = args.next() {
        match a.as_str() {
            "--file" => file = args.next(),
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()).unwrap_or(limit),
            "--min-score" => min_score = args.next().and_then(|v| v.parse().ok()).unwrap_or(min_score),
            "--budget" => budget = args.next().and_then(|v| v.parse().ok()),
            "--json" => json = true,
            "--verbose" | "-v" => verbose = true,
            "-h" | "--help" => {
                println!("usage: mach kb eval [--file F] [--limit N] [--min-score F] [--budget N] [--json] [--verbose]");
                println!("       Scores retrieval, not generation: a question passes when recall injected");
                println!("       one of the memory ids that answer it (and none of its superseded ones).");
                println!("       Default question set: ~/{}", eval::DEFAULT_QUESTIONS_PATH);
                return Ok(());
            }
            other => {
                eprintln!("mach kb eval: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let path = match file {
        Some(f) => PathBuf::from(f),
        None => {
            let home = std::env::var("HOME").unwrap_or_default();
            Path::new(&home).join(eval::DEFAULT_QUESTIONS_PATH)
        }
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mach kb eval: cannot read {}: {}", path.display(), e);
            std::process::exit(1);
        }
    };
    let questions = eval::parse_questions(&content).map_err(to_io)?;

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let now = store::now_rfc3339();
    let mut remaps: Vec<(String, i64, i64)> = Vec::new();
    let questions: Vec<eval::Question> = questions
        .iter()
        .map(|q| {
            let (resolved, moved) =
                eval::resolve_expect(q, |id| store::get(&conn, id).ok().flatten().and_then(|m| m.superseded_by));
            remaps.extend(moved.into_iter().map(|(orig, new)| (q.id.clone(), orig, new)));
            resolved
        })
        .collect();

    let mut results = Vec::new();
    for q in &questions {
        let resp = match search_hits(&conn, &embedder, &q.query, limit, false, false, min_score, &now, q.project.as_deref(), None, q.as_of.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("mach kb eval: {} — search failed: {}", q.id, e);
                std::process::exit(1);
            }
        };
        let resp = match budget {
            Some(b) => pack_to_budget(resp, b),
            None => resp,
        };
        // Memory hits only: an insight id lives in the same numeric space
        // and would otherwise count as a false hit on an expected id.
        let injected: Vec<i64> = resp.hits.iter().filter(|h| !h.derived).map(|h| h.id).collect();
        let chars: usize = resp.hits.iter().map(|h| h.content.chars().count()).sum::<usize>()
            + resp.cards.iter().map(|c| c.text.chars().count()).sum::<usize>();
        results.push(eval::score(q, &injected, chars));
    }

    let (by_cat, overall) = eval::summarize(&results);

    if json {
        #[derive(Serialize)]
        struct Report<'a> {
            limit: usize,
            min_score: f32,
            budget: Option<usize>,
            by_category: &'a BTreeMap<String, eval::Tally>,
            overall: &'a eval::Tally,
            results: &'a [eval::QuestionResult],
        }
        let report = Report { limit, min_score, budget, by_category: &by_cat, overall: &overall, results: &results };
        let out = serde_json::to_string_pretty(&report)
            .map_err(|e| io::Error::other(format!("mach kb eval: {}", e)))?;
        println!("{}", out);
        return Ok(());
    }

    println!("mach kb eval: {} questions, limit={} min_score={}{}", overall.total, limit, min_score,
        budget.map(|b| format!(" budget={}", b)).unwrap_or_default());
    for (cat, t) in &by_cat {
        println!(
            "  {:<18} {}/{} ({:.0}%)  mean {:.0} chars{}",
            cat,
            t.passed,
            t.total,
            t.rate() * 100.0,
            t.mean_chars(),
            if t.served_stale > 0 { format!("  STALE SERVED x{}", t.served_stale) } else { String::new() }
        );
    }
    println!(
        "  {:<18} {}/{} ({:.0}%)  mean {:.0} chars{}",
        "OVERALL",
        overall.passed,
        overall.total,
        overall.rate() * 100.0,
        overall.mean_chars(),
        if overall.served_stale > 0 { format!("  STALE SERVED x{}", overall.served_stale) } else { String::new() }
    );

    if verbose {
        for (qid, orig, resolved) in &remaps {
            let content = store::get(&conn, *resolved).ok().flatten().map(|m| truncate(&m.content, 60)).unwrap_or_default();
            println!("  REMAP {} #{} -> #{}  {}", qid, orig, resolved, content);
        }
        for r in results.iter().filter(|r| !r.passed) {
            let q = questions.iter().find(|q| q.id == r.id);
            println!(
                "\n  MISS {} [{}] {:?}\n    expected {:?}  injected {:?}",
                r.id,
                r.category,
                r.query,
                q.map(|q| q.expect.clone()).unwrap_or_default(),
                r.injected
            );
            for id in &r.injected {
                if let Ok(Some(m)) = store::get(&conn, *id) {
                    println!("      #{} {}", id, truncate(&m.content, 70));
                }
            }
        }
    }
    Ok(())
}

fn cmd_why(args: impl Iterator<Item = String>) -> io::Result<()> {
    let args: Vec<String> = args.collect();
    if args.iter().any(|a| a == "-h" || a == "--help") || args.is_empty() {
        println!("usage: mach kb why <memory-id>");
        println!("       mach kb why insight <insight-id>   (also: mach kb why i<insight-id>)");
        println!("Read-only provenance trace: where a memory came from, what it replaced, what cites it,");
        println!("which graph edges it evidences, what it is associated with, and how often recall showed it.");
        return if args.is_empty() { std::process::exit(1) } else { Ok(()) };
    }
    let Some(target) = parse_why_target(&args) else {
        eprintln!("mach kb why: expected <id> or `insight <id>`, got {:?}", args);
        std::process::exit(1);
    };
    let conn = store::open().map_err(to_io)?;
    let home = std::env::var("HOME").unwrap_or_default();
    let recall_dir = Path::new(&home).join(".local/share/mach/recall-log");
    let meetings_dir = Path::new(&home).join(".local/share/mach/meetings");
    let report = why_report(&conn, target, Some(&recall_dir), Some(&meetings_dir)).map_err(to_io)?;
    print!("{}", report);
    Ok(())
}

fn cmd_entity(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut name: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach kb entity <name>");
                return Ok(());
            }
            other => {
                if name.is_none() {
                    name = Some(other.to_string());
                } else {
                    eprintln!("mach kb entity: unexpected argument '{}'", other);
                    std::process::exit(1);
                }
            }
        }
    }
    let name = match name {
        Some(n) => n,
        None => {
            eprintln!("mach kb entity: missing <name> argument");
            std::process::exit(1);
        }
    };

    let conn = store::open().map_err(to_io)?;
    let embedder = OllamaEmbedder::new();
    let entity = match resolve_entity_for_lookup(&conn, &embedder, &name) {
        Some(e) => e,
        None => {
            println!("no entity found matching '{}'", name);
            return Ok(());
        }
    };

    println!("#{} {} ({})", entity.id, entity.name, entity.kind.as_deref().unwrap_or("unspecified"));

    let degree = store::mention_degree(&conn, entity.id).map_err(to_io)?;
    match store::get_entity_card(&conn, entity.id).map_err(to_io)? {
        Some(card) => {
            println!(
                "  card (rebuilt {} from {} of {} mentioning memories):",
                card.updated_at.get(..10).unwrap_or(&card.updated_at),
                card.source_ids.len(),
                degree
            );
            for line in card.text.lines() {
                println!("    {}", line);
            }
        }
        None if degree >= store::CARD_MIN_MENTIONS => {
            println!("  card: none yet ({} mentioning memories; due at the next `mach kb reflect`)", degree)
        }
        None => println!("  card: none ({} mentioning memories, {} needed)", degree, store::CARD_MIN_MENTIONS),
    }

    let edges = store::active_relations_for_entity(&conn, entity.id).map_err(to_io)?;
    if edges.is_empty() {
        println!("  no active connections");
    } else {
        for e in &edges {
            let other_id = if e.src == entity.id { e.dst } else { e.src };
            let other_name = match store::get_entity(&conn, other_id).map_err(to_io)? {
                Some(o) => o.name,
                None => format!("#{}", other_id),
            };
            let evidence = e
                .evidence_memory_id
                .and_then(|id| store::get(&conn, id).ok().flatten());
            let snippet = match &evidence {
                Some(m) => {
                    let date = m.created_at.get(..10).unwrap_or(&m.created_at);
                    format!("  — {} ({})", truncate(&m.content, 70), date)
                }
                None => String::new(),
            };
            if e.src == entity.id {
                println!("  {} —{}→ {}{}", entity.name, e.predicate, other_name, snippet);
            } else {
                println!("  {} —{}→ {}{}", other_name, e.predicate, entity.name, snippet);
            }
        }
    }
    let invalidated = store::invalidated_relation_count_for_entity(&conn, entity.id).map_err(to_io)?;
    if invalidated > 0 {
        println!(
            "  ({} invalidated connection{} not shown — mach kb list --superseded style audit trail)",
            invalidated,
            if invalidated == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// `mach kb graph --stats` — a compact view of the association graph `mach
/// kb reflect`'s extraction pass has derived so far: total entities (broken
/// down by `kind`) and edge counts (active vs. invalidated). `mach kb graph
/// audit` is a separate subcommand (see `cmd_graph_audit`) — dispatched on
/// here since both live under the same `graph` subcommand name.
fn cmd_graph(args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut args = args.peekable();
    if args.peek().map(|s| s.as_str()) == Some("audit") {
        args.next();
        return cmd_graph_audit(args);
    }

    let mut stats = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--stats" => stats = true,
            "-h" | "--help" => {
                println!("usage: mach kb graph --stats");
                println!("       mach kb graph audit");
                return Ok(());
            }
            other => {
                eprintln!("mach kb graph: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    if !stats {
        eprintln!("mach kb graph: missing --stats");
        std::process::exit(1);
    }

    let conn = store::open().map_err(to_io)?;
    let total_entities = store::entity_count(&conn).map_err(to_io)?;
    let by_kind = store::entity_counts_by_kind(&conn).map_err(to_io)?;
    let active_edges = store::active_relation_count(&conn).map_err(to_io)?;
    let invalidated_edges = store::invalidated_relation_count(&conn).map_err(to_io)?;

    println!("entities: {}", total_entities);
    println!("memory associations (hebbian): {}", store::count_assoc(&conn).map_err(to_io)?);
    for (kind, count) in &by_kind {
        println!("  {}: {}", kind, count);
    }
    println!("edges: {} active, {} invalidated", active_edges, invalidated_edges);
    println!(
        "memory→entity mentions: {} (the substrate graph recall hops over and cards are built from)",
        store::count_mentions(&conn).map_err(to_io)?
    );
    let cards = store::count_entity_cards(&conn).map_err(to_io)?;
    let due = store::entity_card_candidates(&conn, 1000).map_err(to_io)?.len();
    println!("entity cards: {} built, {} due for a rebuild", cards, due);
    Ok(())
}

/// `mach kb graph audit` — a one-time-or-repeatable batched pass over every
/// ACTIVE edge (`store::active_relations_all`), judging each KEEP / POISONED
/// / GENERIC (`reflect::GraphAuditVerdict`) and invalidating (never
/// deleting) anything POISONED or GENERIC. Reusable: unlike `mach kb
/// reflect`'s own drained-backlog passes, this re-examines the whole active
/// edge set every run, so running it again after a clean pass costs one
/// batch of already-KEEP verdicts, never a stale watermark.
fn cmd_graph_audit(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach kb graph audit");
                println!("       batched KEEP/POISONED/GENERIC judgment over every active edge;");
                println!("       POISONED/GENERIC edges are invalidated (never deleted).");
                return Ok(());
            }
            other => {
                eprintln!("mach kb graph audit: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    let llm = ProcessReflectLlm::new();
    let (kept, invalidated, invalidated_descs, llm_failed) =
        run_graph_audit(&conn, &reflect::LoggedLlm::new(&conn, "graph_audit", &llm), &now).map_err(to_io)?;

    for desc in &invalidated_descs {
        println!("invalidated: {}", desc);
    }
    println!(
        "mach kb graph audit: kept={} invalidated={}{}",
        kept,
        invalidated,
        if llm_failed { " (degraded: some claude calls failed — re-run to cover the rest)" } else { "" }
    );
    Ok(())
}

/// The audit pass itself: batches `store::active_relations_all` at
/// `reflect::GRAPH_AUDIT_BATCH_SIZE` edges per call, describing each edge by
/// its endpoint names/kinds, predicate, and an evidence snippet (id-free,
/// per this feature's own house rule). An edge whose batch reply never
/// addressed it, or whose whole batch call failed, is left exactly as it is
/// (treated like an explicit KEEP for this run's counts) — never
/// destructive on doubt, and simply re-examined on the next `mach kb graph
/// audit` invocation. Returns `(kept, invalidated, invalidated_descriptions,
/// any_llm_call_failed)`. Generic over `ReflectLlm` so it's exercised in
/// tests against a fake that can fail on demand.
fn run_graph_audit<L: ReflectLlm>(conn: &Connection, llm: &L, now: &str) -> Result<(usize, usize, Vec<String>, bool), KbError> {
    let edges = store::active_relations_all(conn)?;
    let mut kept = 0usize;
    let mut invalidated = 0usize;
    let mut invalidated_descs: Vec<String> = Vec::new();
    let mut llm_failed = false;

    for chunk in edges.chunks(reflect::GRAPH_AUDIT_BATCH_SIZE) {
        let mut descs: Vec<(String, String, String, String, String, String)> = Vec::new();
        for r in chunk {
            let src = store::get_entity(conn, r.src)?;
            let dst = store::get_entity(conn, r.dst)?;
            let (src_name, src_kind) = match src {
                Some(e) => (e.name, e.kind.unwrap_or_else(|| "unspecified".to_string())),
                None => ("?".to_string(), "unspecified".to_string()),
            };
            let (dst_name, dst_kind) = match dst {
                Some(e) => (e.name, e.kind.unwrap_or_else(|| "unspecified".to_string())),
                None => ("?".to_string(), "unspecified".to_string()),
            };
            let evidence = r
                .evidence_memory_id
                .and_then(|id| store::get(conn, id).ok().flatten())
                .map(|m| truncate(&m.content, 100))
                .unwrap_or_else(|| "(no evidence recorded)".to_string());
            descs.push((src_name, src_kind, r.predicate.clone(), dst_name, dst_kind, evidence));
        }
        let prompt_rows: Vec<(&str, &str, &str, &str, &str, &str)> = descs
            .iter()
            .map(|(a, b, c, d, e, f)| (a.as_str(), b.as_str(), c.as_str(), d.as_str(), e.as_str(), f.as_str()))
            .collect();
        let prompt = reflect::build_batch_graph_audit_prompt(&prompt_rows);
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                kept += chunk.len(); // left untouched -- counted as kept for this run's report
                continue;
            }
        };
        let verdicts = reflect::parse_batch_graph_audit_verdicts(&raw, chunk.len());
        for (i, r) in chunk.iter().enumerate() {
            match verdicts.get(&(i + 1)) {
                Some(reflect::GraphAuditVerdict::Poisoned) | Some(reflect::GraphAuditVerdict::Generic) => {
                    let verdict_label = if verdicts.get(&(i + 1)) == Some(&reflect::GraphAuditVerdict::Poisoned) {
                        "POISONED"
                    } else {
                        "GENERIC"
                    };
                    if store::invalidate_relation(conn, r.id, now)? {
                        invalidated += 1;
                        let (src_name, src_kind, predicate, dst_name, dst_kind, _) = &descs[i];
                        invalidated_descs.push(format!(
                            "#{} {} ({}) --{}--> {} ({}) [{}]",
                            r.id, src_name, src_kind, predicate, dst_name, dst_kind, verdict_label
                        ));
                    }
                }
                _ => kept += 1, // explicit KEEP, or unaddressed -- never destructive on doubt
            }
        }
    }
    Ok((kept, invalidated, invalidated_descs, llm_failed))
}

// --- audit-supersessions: find and repair lossy tombstones ---

/// One LOSSY verdict from a `mach kb audit-supersessions` run, ready to
/// print or serialize. `restored` is only ever true under `--apply`, and
/// only when `store::restore` actually flipped the row (it stays false on
/// a dry run, and would stay false for a row some earlier step already
/// restored — the PK on `old_id` means that can't happen within one run,
/// but the caller doesn't rely on that to report honestly).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct LossyRow {
    old_id: i64,
    new_id: i64,
    reason: String,
    restored: bool,
}

/// Summary + detail of one `mach kb audit-supersessions` run.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct SupersessionAuditRunResult {
    audited: usize,
    lossy: usize,
    ok: usize,
    no_verdict: usize,
    llm_failed: bool,
    lossy_rows: Vec<LossyRow>,
}

/// `mach kb audit-supersessions [--limit N] [--apply] [--json] [--reaudit]`
/// — an LLM-judged second look at automatic supersession. Candidates are
/// every tombstoned memory joined to its direct (one-hop) successor
/// (`store::supersession_candidates`), oldest tombstoned row first, minus
/// whatever `supersession_audit` already has a verdict for unless
/// `--reaudit`. Batches of `reflect::SUPERSESSION_AUDIT_BATCH_SIZE` go to
/// one sonnet call each (`reflect::build_supersession_audit_prompt` /
/// `parse_supersession_audit_verdicts`), tagged `supersession_audit` in
/// `judge_log`.
///
/// Dry run by default: every audited pair (OK or LOSSY) is still recorded
/// in `supersession_audit` so it isn't re-asked next run, but nothing about
/// the memories themselves changes. `--apply` additionally restores (and,
/// via `store::restore`, pins) every LOSSY row still tombstoned — not only
/// ones freshly judged this run, but every LOSSY verdict on record a
/// previous run (a dry run, or an `--apply` run that didn't reach every
/// row) hasn't acted on yet; since an already-audited pair is never
/// re-sent to the judge, restoring only this run's fresh verdicts would
/// otherwise leave every earlier finding tombstoned forever. An
/// unaddressed pair — the judge's reply never named it, or its whole batch
/// call failed — gets no verdict recorded at all and is simply retried
/// next run; it is never restored, matching the "fail safe: when unsure,
/// keep both rows" rule every other automatic tombstone path in this
/// codebase follows.
fn cmd_audit_supersessions(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut limit: Option<usize> = None;
    let mut apply = false;
    let mut json = false;
    let mut reaudit = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--limit" => limit = args.next().and_then(|v| v.parse().ok()),
            "--apply" => apply = true,
            "--json" => json = true,
            "--reaudit" => reaudit = true,
            "-h" | "--help" => {
                println!("usage: mach kb audit-supersessions [--limit N] [--apply] [--json] [--reaudit]");
                println!(
                    "       LLM-judged audit of every tombstoned memory against its direct \
                     successor: did the tombstone lose a fact (often a dated one) that the \
                     successor doesn't carry? Dry run by default -- prints LOSSY pairs plus a \
                     summary and changes nothing. --apply restores and pins each LOSSY old row. \
                     --reaudit re-examines rows already recorded in supersession_audit (skipped \
                     by default)."
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb audit-supersessions: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    let llm = ProcessReflectLlm::new();
    let result = run_supersession_audit(&conn, &reflect::LoggedLlm::new(&conn, "supersession_audit", &llm), &now, limit, apply, reaudit)
        .map_err(to_io)?;

    if json {
        println!("{}", serde_json::to_string(&result)?);
    } else {
        for row in &result.lossy_rows {
            println!(
                "LOSSY #{} -> #{}  {}{}",
                row.old_id,
                row.new_id,
                row.reason,
                if row.restored { "  restored" } else { "" }
            );
        }
        println!(
            "mach kb audit-supersessions: audited {}, lossy {}, ok {}, no-verdict {}{}",
            result.audited,
            result.lossy,
            result.ok,
            result.no_verdict,
            if result.llm_failed { " (degraded: some claude calls failed — re-run to cover the rest)" } else { "" }
        );
    }
    Ok(())
}

/// The audit pass itself, generic over `ReflectLlm` for testing against a
/// fixed-reply fake. `filtered` candidates (oldest first, already-audited
/// ones dropped unless `reaudit`, then capped by `limit`) are chunked at
/// `reflect::SUPERSESSION_AUDIT_BATCH_SIZE` and sent one sonnet call per
/// chunk. A failed call counts its whole chunk as no-verdict and sets
/// `llm_failed`, same as `run_graph_audit`'s failure handling, except
/// nothing here is ever "kept" implicitly — OK and LOSSY are both explicit
/// verdicts written to `supersession_audit`; only a genuinely unaddressed
/// pair (or a failed call) is left unrecorded and retried. `audited`/
/// `lossy`/`ok`/`no_verdict` describe only THIS run's fresh judging.
///
/// The reported/restored LOSSY rows are a separate, deliberately wider
/// query (`store::supersession_audit_lossy_rows`): every LOSSY verdict on
/// record that is still tombstoned, not only ones freshly judged this
/// call. A prior dry run may have found a LOSSY pair that no `--apply` run
/// has acted on yet — since an already-audited pair is never re-sent to
/// the judge, an `--apply` run that only restored this run's fresh
/// verdicts would silently leave every previously-identified LOSSY row
/// tombstoned forever. `--apply` restores (and pins) each one here; a dry
/// run reports the same rows with `restored` left false.
///
/// A recorded LOSSY row is only ever restored when the memory's CURRENT
/// `superseded_by` still equals that row's recorded `new_id` — never on
/// bare "is this row still tombstoned". Otherwise a manual `mach kb
/// supersede <old> <new>` run after the row was restored (a human
/// deciding, of their own accord, that `<old>` really is superseded by
/// some newer `<new>`) would get silently undone by a later `--apply`
/// invocation acting on the stale earlier verdict — exactly the
/// "audit --apply can undo a manual supersede" bug this guards against.
/// The candidate filter (`filtered`, above) mirrors this: a pair is
/// treated as already-audited, and so skipped rather than re-sent to the
/// judge, only when the recorded `new_id` matches the candidate's CURRENT
/// `new_id` (from `supersession_candidates`'s live join) — so a fresh hop
/// created by that same manual supersede is judged again from scratch,
/// never silently skipped on the strength of the old hop's verdict.
fn run_supersession_audit<L: ReflectLlm>(
    conn: &Connection,
    llm: &L,
    now: &str,
    limit: Option<usize>,
    apply: bool,
    reaudit: bool,
) -> Result<SupersessionAuditRunResult, KbError> {
    let candidates = store::supersession_candidates(conn)?;
    let audited_pairs = if reaudit { HashMap::new() } else { store::supersession_audited_pairs(conn)? };
    // Skip a candidate as already-audited only when the recorded verdict's
    // `new_id` still matches this candidate's CURRENT `new_id`. If `old_id`
    // was audited before but has since been manually re-superseded to a
    // different winner, `audited_pairs.get(&c.old_id)` won't equal
    // `c.new_id` and the new hop is judged fresh, not skipped.
    let mut filtered: Vec<store::SupersessionCandidate> =
        candidates.into_iter().filter(|c| audited_pairs.get(&c.old_id) != Some(&c.new_id)).collect();
    if let Some(l) = limit {
        filtered.truncate(l);
    }

    let mut audited = 0usize;
    let mut lossy = 0usize;
    let mut ok = 0usize;
    let mut no_verdict = 0usize;
    let mut llm_failed = false;

    for chunk in filtered.chunks(reflect::SUPERSESSION_AUDIT_BATCH_SIZE) {
        let prompt_pairs: Vec<(i64, &str, i64, &str)> =
            chunk.iter().map(|c| (c.old_id, c.old_content.as_str(), c.new_id, c.new_content.as_str())).collect();
        let ids: Vec<i64> = chunk.iter().map(|c| c.old_id).collect();
        let prompt = reflect::build_supersession_audit_prompt(&prompt_pairs);
        let raw = match llm.call("sonnet", &prompt, reflect::TIMEOUT_SONNET) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                no_verdict += chunk.len();
                continue;
            }
        };
        let verdicts = reflect::parse_supersession_audit_verdicts(&raw, &ids);
        for c in chunk {
            match verdicts.get(&c.old_id) {
                Some(result) => {
                    audited += 1;
                    let verdict_str = match result.verdict {
                        reflect::SupersessionAuditVerdict::Ok => "OK",
                        reflect::SupersessionAuditVerdict::Lossy => "LOSSY",
                    };
                    store::record_supersession_audit(conn, c.old_id, c.new_id, verdict_str, &result.reason, now)?;
                    match result.verdict {
                        reflect::SupersessionAuditVerdict::Ok => ok += 1,
                        reflect::SupersessionAuditVerdict::Lossy => lossy += 1,
                    }
                }
                None => no_verdict += 1,
            }
        }
    }

    let mut lossy_rows: Vec<LossyRow> = Vec::new();
    for row in store::supersession_audit_lossy_rows(conn)? {
        let current_superseded_by = store::get(conn, row.old_id)?.and_then(|m| m.superseded_by);
        if current_superseded_by != Some(row.new_id) {
            // Either already fixed (an earlier --apply run, or `mach kb
            // restore` by hand, already cleared it — `current_superseded_by`
            // is None) or a human has since manually re-superseded this row
            // to a DIFFERENT winner (`mach kb supersede <old> <new>`) — in
            // which case restoring on the strength of this stale verdict
            // would silently undo that manual decision. Either way, this
            // recorded verdict is no longer actionable; skip it.
            continue;
        }
        let restored = apply && store::restore(conn, row.old_id, now)?;
        lossy_rows.push(LossyRow { old_id: row.old_id, new_id: row.new_id, reason: row.reason, restored });
    }

    Ok(SupersessionAuditRunResult { audited, lossy, ok, no_verdict, llm_failed, lossy_rows })
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

/// Renders text-mode `mach kb model` output, one `format_model_row` line
/// per selected row, in the same order `rows` is already in (see
/// `store::mental_model` for the ranking) — selection never reorders rows,
/// it only decides which ones make the cut. `None` renders every row,
/// unchanged from before `--max-chars` existed.
///
/// With `max_chars`, three passes over `rows` decide what's included, so
/// a handful of just-created insights can't be crowded out by old
/// high-confidence themes filling the whole budget on rank alone:
///
/// 1. the highest-ranked rows are added, skipping (never truncating) any
///    that would overflow, until the running total would exceed the
///    first 30% of the budget — the same skip-on-overflow behavior this
///    function had before this reservation existed, just capped smaller;
/// 2. rows not yet included, still in rank order, are added — against the
///    full budget, not the 30% cap — but only rows flagged `recent`
///    (`store::ModelRow::recent`: created or revised in the last 7 days),
///    until they account for at least 30% of the whole budget or there
///    are no more eligible recent rows left to add;
/// 3. whatever budget remains is filled by the rest, in rank order, same
///    skip-on-overflow behavior as pass 1.
///
/// A row is added at most once (a `selected` flag per row prevents a
/// later pass from re-adding one an earlier pass already placed). A
/// theme's nested belief rows (`row.nested`) always immediately follow it
/// in `rows` (see `mental_model`), so `parent_idx` records each nested
/// row's theme by position; a nested row is only ever eligible once its
/// own theme row has been selected, in whichever pass that happened —
/// this is what keeps an indented belief from ever appearing without its
/// parent theme above it, across all three passes, not just the first.
fn render_model_text(rows: &[store::ModelRow], max_chars: Option<usize>) -> String {
    let max = match max_chars {
        None => {
            let mut out = String::new();
            for row in rows {
                out.push_str(&format_model_row(row));
                out.push('\n');
            }
            return out;
        }
        Some(m) => m,
    };

    let n = rows.len();
    let lines: Vec<String> = rows.iter().map(format_model_row).collect();
    let lens: Vec<usize> = lines.iter().map(|l| l.chars().count() + 1).collect();

    // Each nested row's parent theme, by position -- `None` for a theme
    // row itself or an unthemed belief, neither of which is gated by
    // anything else being selected first.
    let mut parent_idx: Vec<Option<usize>> = vec![None; n];
    {
        let mut current_theme: Option<usize> = None;
        for (i, row) in rows.iter().enumerate() {
            if !row.nested {
                current_theme = if matches!(row.kind, store::ModelKind::Theme) { Some(i) } else { None };
            } else {
                parent_idx[i] = current_theme;
            }
        }
    }
    let eligible = |i: usize, selected: &[bool]| match parent_idx[i] {
        Some(p) => selected[p],
        None => true,
    };

    let mut selected = vec![false; n];
    let mut total = 0usize;
    let mut recent_total = 0usize;
    let reserved = max.saturating_mul(3) / 10; // first 30% of the budget

    // Pass 1: highest-ranked rows, capped to the first 30% of the budget.
    for i in 0..n {
        if selected[i] || !eligible(i, &selected) || total + lens[i] > reserved {
            continue;
        }
        selected[i] = true;
        total += lens[i];
        if rows[i].recent {
            recent_total += lens[i];
        }
    }

    // Pass 2: top up recent rows, in rank order, against the full budget,
    // until they account for at least 30% of it (or none are left to add).
    // Parent themes selected earlier in this same forward pass make their
    // own recent children eligible later in the same pass, since a theme's
    // index always precedes its nested rows.
    for i in 0..n {
        if recent_total >= reserved {
            break;
        }
        if selected[i] || !rows[i].recent || !eligible(i, &selected) || total + lens[i] > max {
            continue;
        }
        selected[i] = true;
        total += lens[i];
        recent_total += lens[i];
    }

    // Pass 3: fill whatever budget remains with the rest, in rank order.
    for i in 0..n {
        if selected[i] || !eligible(i, &selected) || total + lens[i] > max {
            continue;
        }
        selected[i] = true;
        total += lens[i];
    }

    let mut out = String::new();
    for i in 0..n {
        if selected[i] {
            out.push_str(&lines[i]);
            out.push('\n');
        }
    }
    out
}

fn cmd_model(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut json = false;
    let mut max_chars: Option<usize> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--json" => json = true,
            "--max-chars" => max_chars = args.next().and_then(|v| v.parse().ok()),
            "-h" | "--help" => {
                println!("usage: mach kb model [--json] [--max-chars N]");
                println!(
                    "       compact mental-model view: all active (non-invalidated) insights and \
                     themes, ranked non-doubted-and-highest-confidence first — no source ids, no \
                     memory leaves. Empty store prints nothing. Meant for cheap always-on context \
                     injection (see ~/.config/claude-hooks/kb-model.sh); use `mach kb tree` or \
                     `mach kb insights` for the full picture with citations. --max-chars N \
                     (text mode only) fits the output to a size budget by skipping rows that \
                     would overflow it, favoring earlier (higher-ranked) rows."
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
        print!("{}", render_model_text(&rows, max_chars));
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
    pruned_recall_logs: usize,
    /// `--partial` only: sessions whose checkpoint mark advanced this run.
    checkpointed: usize,
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

/// Deletes every recall-log file (`<session_id>.jsonl` under
/// `recall_log_root`) whose session has already been fully ingested
/// (`store::is_session_ingested`) and which has sat untouched for at least
/// `ingest::RECALL_LOG_PRUNE_MIN_AGE_SECS` — these otherwise accumulate
/// forever, one per session, with nothing else ever cleaning them up.
/// Tolerant of a missing `recall_log_root` (nothing to prune yet, same
/// posture as `list_transcript_files`) and of any single file's metadata
/// read, DB lookup, or delete failing — that one file is left alone and
/// retried on a later run rather than aborting the rest of the sweep. A
/// file whose session was never ingested is never touched, no matter how
/// old it is — pruning only ever removes logs whose data already did its
/// job.
fn prune_recall_logs(conn: &Connection, recall_log_root: &Path) -> usize {
    let mut pruned = 0usize;
    let Ok(entries) = std::fs::read_dir(recall_log_root) else {
        return pruned;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(session_id) = ingest::session_id_from_path(&path) else {
            continue;
        };
        let ingested = match store::is_session_ingested(conn, &session_id) {
            Ok(v) => v,
            Err(_) => continue, // DB hiccup this run -- leave the file, retried next time
        };
        let age_secs = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|mtime| mtime.elapsed().ok())
            .map(|d| d.as_secs())
            .unwrap_or(0); // unknown age -- never prune on uncertainty
        if ingest::should_prune_recall_log(ingested, age_secs) && std::fs::remove_file(&path).is_ok() {
            pruned += 1;
        }
    }
    pruned
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
/// Test-facing wrapper: the historical whole-session entry point.
#[cfg(test)]
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
    ingest_sessions_impl(conn, embedder, llm, filter, projects_root, recall_log_root, only_session, false, now)
}

/// `run_ingest_sessions` plus the `--partial` mode behind mid-session
/// checkpoints (`kb-checkpoint.sh` on Stop/PreCompact): digest only the raw
/// transcript lines since the session's last checkpoint
/// (`store::session_progress`), record the new high-water mark, and never
/// mark the session ingested nor judge engagement -- both belong to the
/// final pass, which in turn digests only the lines after the last
/// checkpoint so no passage is ever extracted twice. A partial run needs a
/// specific session; with `only_session` None it does nothing.
#[allow(clippy::too_many_arguments)]
fn ingest_sessions_impl<E: Embedder, L: ReflectLlm, F: ingest::TranscriptFilter>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    filter: &F,
    projects_root: &Path,
    recall_log_root: &Path,
    only_session: Option<&str>,
    partial: bool,
    now: &str,
) -> Result<IngestSummary, KbError> {
    let mut summary = IngestSummary::default();

    if partial && only_session.is_none() {
        return Ok(summary);
    }

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
        // A session the idle sweep already marked finished can still be
        // alive (a laptop resumed after ten quiet minutes): its engagement
        // pass is done for good, but anything written after the mark still
        // deserves a digest -- so "ingested" no longer means "never look
        // again," only "digest the tail, no engagement, no re-marking."
        let already_ingested = store::is_session_ingested(conn, &session_id)?;
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue; // unreadable this run -- stays unprocessed, retried later
        };

        // Lines a previous checkpoint (or the final pass) already digested.
        // Every path digests only what came after them.
        let last_line = store::session_progress(conn, &session_id)?.max(0) as usize;
        let (new_raw, raw_line_count) = ingest::lines_after(&raw, last_line);
        let new_line_count = raw_line_count.saturating_sub(last_line);

        if already_ingested && !partial {
            if new_line_count < ingest::PARTIAL_MIN_NEW_LINES {
                continue;
            }
            // finished session that kept growing: tail digest only
            let Some(tail) = filter.filter(&new_raw).filter(|d| !d.trim().is_empty()) else {
                store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
                continue;
            };
            match llm.call(
                "haiku",
                &ingest::build_digest_prompt(&ingest::tail_lines(&tail, ingest::DIGEST_MAX_DIALOGUE_LINES)),
                ingest::TIMEOUT_INGEST,
            ) {
                Ok(out) => {
                    for fact in ingest::parse_digest_facts_with_basis(&out) {
                        let embedding = embedder.embed(&fact.content).ok();
                        if let Ok(id) = store::insert_with_basis(
                            conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                        ) {
                            if let Some((from, to)) = &fact.when {
                                let _ = store::set_occurrence(conn, id, from, to);
                            }
                            summary.facts_added += 1;
                        }
                    }
                    store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
                    summary.checkpointed += 1;
                }
                Err(_) => summary.deferred_offline += 1,
            }
            continue;
        }

        // --- checkpoint (partial) ---
        if partial {
            if new_line_count < ingest::PARTIAL_MIN_NEW_LINES {
                continue; // not enough new material yet; the next checkpoint or the final pass gets it
            }
            let dialogue = filter.filter(&new_raw);
            let Some(dialogue_text) = dialogue.as_deref().filter(|d| !d.trim().is_empty()) else {
                // nothing digestible in this stretch -- move the mark so the
                // final pass does not re-filter it either
                store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
                summary.checkpointed += 1;
                continue;
            };
            let window = ingest::tail_lines(dialogue_text, ingest::DIGEST_MAX_DIALOGUE_LINES);
            match llm.call("haiku", &ingest::build_digest_prompt(&window), ingest::TIMEOUT_INGEST) {
                Ok(out) => {
                    for fact in ingest::parse_digest_facts_with_basis(&out) {
                        let embedding = embedder.embed(&fact.content).ok();
                        if let Ok(id) = store::insert_with_basis(
                            conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                        ) {
                            if let Some((from, to)) = &fact.when {
                                let _ = store::set_occurrence(conn, id, from, to);
                            }
                            summary.facts_added += 1;
                        }
                    }
                    store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
                    summary.checkpointed += 1;
                }
                Err(_) => summary.deferred_offline += 1, // mark unchanged; retried next checkpoint
            }
            continue;
        }

        // --- final pass ---

        // No-LLM skill-usage counts for `mach kb improve`, recorded on every
        // path that marks this session processed.
        let usage_json = ingest::skill_usage_json(&ingest::skill_usage_from_transcript(&raw));

        let recall_log_path = recall_log_root.join(format!("{}.jsonl", session_id));
        let injected_ids: Vec<i64> =
            std::fs::read_to_string(&recall_log_path).map(|c| ingest::parse_recall_log(&c)).unwrap_or_default();

        if raw_line_count < INGEST_TRIVIAL_LINE_FLOOR && injected_ids.is_empty() {
            store::mark_session_ingested_with_usage(conn, &session_id, now, usage_json.as_deref())?;
            store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
            summary.processed += 1;
            continue;
        }

        // Engagement is judged over the whole conversation; the digest only
        // over what no checkpoint has seen.
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
                store::mark_session_ingested_with_usage(conn, &session_id, now, usage_json.as_deref())?;
                store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
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
                        let (verdicts, is_fallback) = ingest::parse_engagement_verdicts(&out, &known_ids);
                        let engaged_ids: Vec<i64> = verdicts
                            .iter()
                            .filter(|(_, v)| **v == ingest::EngagementVerdict::Engaged)
                            .map(|(id, _)| *id)
                            .collect();
                        if !engaged_ids.is_empty() {
                            store::touch(conn, &engaged_ids, now)?;
                        }
                        // fired together, wire together
                        if engaged_ids.len() >= 2 {
                            store::reinforce_assoc(conn, &engaged_ids, now)?;
                        }
                        summary.engaged += engaged_ids.len();
                        summary.shown += verdicts.len() - engaged_ids.len();
                        // Durable record for `mach kb recall-stats`, surviving the
                        // 14-day prune of the recall-log JSONL: "shown" here is
                        // exactly the ids the judge returned a verdict for. Skipped
                        // entirely when the reply was rejected and this is the
                        // all-SHOWN fallback (`is_fallback`) -- that isn't a real
                        // judgment, and recording it would make recall-stats claim
                        // the judge reviewed and passed on every one of these ids
                        // when it actually just failed to produce a usable reply.
                        if !is_fallback {
                            let shown_ids: Vec<i64> = verdicts.keys().copied().collect();
                            store::record_engagement(conn, &session_id, &shown_ids, &engaged_ids, now)?;
                        }
                    }
                    Err(_) => needs_retry = true,
                }
            }
            // every injected id has since been forgotten -- nothing left to
            // judge or touch, and no call was needed for it.
        }

        // --- fact digest (absorbed kb-capture.sh) over the undigested tail ---
        // A session with no checkpoints keeps the historical whole-transcript
        // floor; one that was checkpointed only needs the tail to be worth a
        // call.
        let digest_floor =
            if last_line == 0 { ingest::DIGEST_MIN_TRANSCRIPT_LINES } else { ingest::PARTIAL_MIN_NEW_LINES };
        if !needs_retry && new_line_count >= digest_floor {
            let tail_dialogue = if last_line == 0 { Some(dialogue_text.to_string()) } else { filter.filter(&new_raw) };
            if let Some(tail) = tail_dialogue.as_deref().filter(|d| !d.trim().is_empty()) {
                let digest_prompt = ingest::build_digest_prompt(&ingest::tail_lines(tail, ingest::DIGEST_MAX_DIALOGUE_LINES));
                match llm.call("haiku", &digest_prompt, ingest::TIMEOUT_INGEST) {
                    Ok(out) => {
                        for fact in ingest::parse_digest_facts_with_basis(&out) {
                            let embedding = embedder.embed(&fact.content).ok();
                            if let Ok(id) = store::insert_with_basis(
                                conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                            ) {
                                if let Some((from, to)) = &fact.when {
                                    let _ = store::set_occurrence(conn, id, from, to);
                                }
                                summary.facts_added += 1;
                            }
                        }
                    }
                    Err(_) => needs_retry = true,
                }
            }
        }

        if needs_retry {
            summary.deferred_offline += 1;
            continue; // leave unprocessed -- retried on a later run
        }

        store::mark_session_ingested_with_usage(conn, &session_id, now, usage_json.as_deref())?;
        store::set_session_progress(conn, &session_id, raw_line_count as i64, now)?;
        summary.processed += 1;
    }

    summary.pruned_recall_logs = prune_recall_logs(conn, recall_log_root);

    Ok(summary)
}

fn cmd_ingest_sessions(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut only_session: Option<String> = None;
    let mut partial = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--session-id" => only_session = args.next(),
            "--partial" => partial = true,
            "-h" | "--help" => {
                println!("usage: mach kb ingest-sessions [--session-id ID [--partial]]");
                println!(
                    "       --partial (with --session-id): mid-session checkpoint -- digests only the \
                     transcript lines since the last checkpoint and records the new mark; never \
                     marks the session finished (kb-checkpoint.sh's Stop/PreCompact trigger)."
                );
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

    if partial && only_session.is_none() {
        eprintln!("mach kb ingest-sessions: --partial requires --session-id");
        std::process::exit(1);
    }
    let summary = ingest_sessions_impl(
        &conn,
        &embedder,
        &reflect::LoggedLlm::new(&conn, "ingest", &llm),
        &filter,
        &projects_root,
        &recall_log_root,
        only_session.as_deref(),
        partial,
        &now,
    )
    .map_err(to_io)?;

    println!(
        "mach kb ingest-sessions: scanned={} processed={} checkpointed={} engaged={} shown={} facts_added={} deferred_offline={} pruned_recall_logs={}",
        summary.scanned,
        summary.processed,
        summary.checkpointed,
        summary.engaged,
        summary.shown,
        summary.facts_added,
        summary.deferred_offline,
        summary.pruned_recall_logs
    );
    Ok(())
}

// --- recall-stats: the injected-vs-engaged tuning metric ---

/// Formats `recall_stats_since` rows into the lines `mach kb recall-stats`
/// prints: one `session_id(8 chars)  shown  engaged  precision%` row per
/// session, then an `OVERALL` line totalling across all of them. A separate,
/// pure function (no db, no clock) so the exact text is unit-testable
/// without a connection. An empty window prints a single explanatory line
/// rather than nothing, so a fresh `recall_engagement` table doesn't read as
/// a hung or broken command.
fn format_recall_stats(rows: &[store::RecallStatsRow]) -> Vec<String> {
    if rows.is_empty() {
        return vec!["mach kb recall-stats: no judged sessions in this window".to_string()];
    }
    let pct = |engaged: i64, shown: i64| if shown > 0 { (engaged as f64 / shown as f64) * 100.0 } else { 0.0 };
    let mut lines: Vec<String> = rows
        .iter()
        .map(|r| {
            // Char-based, not a byte slice: a session id is normally an
            // ASCII UUID, but slicing raw bytes would panic if one ever
            // isn't and byte 8 lands mid-character.
            let short: String = r.session_id.chars().take(8).collect();
            format!("{:<8}  {:>5}  {:>7}  {:>5.0}%", short, r.shown, r.engaged, pct(r.engaged, r.shown))
        })
        .collect();
    let total_shown: i64 = rows.iter().map(|r| r.shown).sum();
    let total_engaged: i64 = rows.iter().map(|r| r.engaged).sum();
    lines.push(format!("{:<8}  {:>5}  {:>7}  {:>5.0}%", "OVERALL", total_shown, total_engaged, pct(total_engaged, total_shown)));
    lines
}

fn cmd_recall_stats(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut days: i64 = 14;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--days" => days = args.next().and_then(|v| v.parse().ok()).unwrap_or(days),
            "-h" | "--help" => {
                println!("usage: mach kb recall-stats [--days N]");
                println!(
                    "       the recall-precision tuning metric, read from the durable"
                );
                println!(
                    "       `recall_engagement` table `mach kb ingest-sessions` writes per"
                );
                println!(
                    "       session: shown = ids the recall log listed that the engagement"
                );
                println!(
                    "       judge actually returned a verdict for; engaged = the subset it"
                );
                println!(
                    "       marked ENGAGED. Default window: 14 days (the recall-log JSONL's"
                );
                println!("       own prune age -- this table is what survives it).");
                return Ok(());
            }
            other => {
                eprintln!("mach kb recall-stats: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn = store::open().map_err(to_io)?;
    let since = store::now_rfc3339_from_secs(store::now_secs().saturating_sub((days.max(0) as u64) * 86400));
    let rows = store::recall_stats_since(&conn, &since).map_err(to_io)?;
    for line in format_recall_stats(&rows) {
        println!("{}", line);
    }
    Ok(())
}

// --- health: operational self-check + persistent-degradation notification ---

/// `~/.local/share/mach/health-notify-streak.json` — the last failing-set
/// `mach kb health --notify` actually notified about, and when (see
/// `health::should_notify`).
fn health_streak_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mach/health-notify-streak.json"))
}

fn load_streak(path: &Path) -> Option<health::NotifyStreak> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok())
}

fn save_streak(path: &Path, streak: &health::NotifyStreak) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(streak) {
        let _ = std::fs::write(path, json);
    }
}

/// Bytes free on the filesystem containing `path`, via `df -B1
/// --output=avail` -- same subprocess-over-a-syscall-binding approach
/// `engines/meet/src/disk.rs::free_bytes` already uses for the same reason
/// (a one-off syscall wrapper isn't worth an extra crate dependency);
/// duplicated in miniature here rather than taking a cross-crate dependency
/// on `meet` for one function.
fn free_bytes(path: &Path) -> io::Result<u64> {
    let out = std::process::Command::new("df").arg("-B1").arg("--output=avail").arg(path).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("df failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .nth(1) // line 0 is the "avail" header
        .and_then(|l| l.trim().parse::<u64>().ok())
        .ok_or_else(|| io::Error::other(format!("could not parse `df` output: {:?}", text)))
}

/// Checks that `mach kb`'s ollama embed model actually responds — a tiny
/// real embed call, not just a TCP connect (mirrors why `note`/`add`/
/// `search` all treat a failed embed as the definitive "ollama unreachable"
/// signal rather than pinging the port).
fn check_ollama() -> health::Check {
    let name = "ollama".to_string();
    let embedder = OllamaEmbedder::new();
    match embedder.embed("mach kb health check") {
        Ok(_) => health::Check { name, ok: true, detail: "embed model responded".to_string() },
        Err(e) => health::Check { name, ok: false, detail: e.to_string() },
    }
}

/// Checks that `kb.db` opens, passes `PRAGMA integrity_check`, and reports
/// its `user_version` (informational — confirms migrations have actually
/// run, not a pass/fail condition on its own).
fn check_kb_db(conn_result: &Result<Connection, KbError>) -> health::Check {
    let name = "kb.db".to_string();
    match conn_result {
        Ok(conn) => {
            let integrity: String =
                conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap_or_else(|_| "error".to_string());
            let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(-1);
            health::Check {
                name,
                ok: integrity == "ok",
                detail: format!("integrity_check={} user_version={}", integrity, version),
            }
        }
        Err(e) => health::Check { name, ok: false, detail: e.to_string() },
    }
}

/// Checks the reflect-completion age (`store::ReflectState::last_completed_at`)
/// against `health::REFLECT_STALE_WARN_HOURS`.
fn check_reflect_completion(conn_result: &Result<Connection, KbError>) -> health::Check {
    let name = "reflect".to_string();
    let conn = match conn_result {
        Ok(c) => c,
        Err(_) => return health::Check { name, ok: false, detail: "kb.db unavailable".to_string() },
    };
    let state = match store::get_reflect_state(conn) {
        Ok(s) => s,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    let now = store::now_rfc3339();
    let hours = state.last_completed_at.as_deref().map(|ts| store::age_days(ts, &now) * 24.0);
    let ok = health::reflect_completion_ok(hours);
    let detail = match hours {
        Some(h) => format!("last completed {:.1}h ago", h),
        None => "never completed".to_string(),
    };
    health::Check { name, ok, detail }
}

/// `mach kb improve` liveness: completed within `improve::STALE_WARN_HOURS`
/// and not `improve::HEALTH_FAIL_STREAK` failures in a row.
fn check_improve(conn_result: &Result<Connection, KbError>) -> health::Check {
    let name = "improve".to_string();
    let conn = match conn_result {
        Ok(c) => c,
        Err(_) => return health::Check { name, ok: false, detail: "kb.db unavailable".to_string() },
    };
    let state = match store::get_improve_state(conn) {
        Ok(s) => s,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    let now = store::now_rfc3339();
    let hours = state.last_completed_at.as_deref().map(|ts| store::age_days(ts, &now) * 24.0);
    let outcomes = match store::memories_by_source_prefix_or_project(conn, improve::OUTCOME_SOURCE_PREFIX, improve::OUTCOME_PROJECT) {
        Ok(v) => v,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    // newest-first, capped at the streak window -- both the pass/fail bools
    // `health_ok` needs and the memories themselves, so a streak that is
    // entirely the uncommitted-write-targets guard can be named by path
    // instead of folded into the generic message below.
    let recent: Vec<&Memory> = outcomes
        .iter()
        .rev()
        .filter(|m| m.source.as_deref().map(|s| s.starts_with(improve::OUTCOME_SOURCE_PREFIX)).unwrap_or(false))
        .take(improve::HEALTH_FAIL_STREAK)
        .collect();
    let recent_failed: Vec<bool> = recent.iter().map(|m| improve::is_failed_outcome(m)).collect();
    let ok = improve::health_ok(hours, &recent_failed);
    let streak = improve::failure_streak(&recent_failed);
    // Name the threshold in the line. "[OK] ... 2 recent failures" reads as
    // a contradiction; the check is a three-strikes rule, so say so rather
    // than leaving the reader to guess whether OK meant the failures were
    // counted.
    let generic_detail = || match hours {
        Some(h) if streak == 0 => format!("last completed {:.1}h ago", h),
        Some(h) => format!(
            "last completed {:.1}h ago, {} consecutive failures since (alerts at {})",
            h,
            streak,
            improve::HEALTH_FAIL_STREAK
        ),
        None => "never completed".to_string(),
    };
    // The special "blocked since ... uncommitted: ..." line only replaces
    // the generic one once the streak is exactly what fails the check (a
    // shorter streak is still `ok` and reads as the same growing-warning
    // text it always has) -- and only when every failure in that streak was
    // this specific guard, never some other kind.
    let detail = if streak >= improve::HEALTH_FAIL_STREAK {
        improve::uncommitted_block_detail(&recent[..streak]).unwrap_or_else(generic_detail)
    } else {
        generic_detail()
    };
    health::Check { name, ok, detail }
}

/// Checks that `machd`'s kb socket subsystem (`socket::run`) is up and
/// actually answers a `search` op — a real one-shot round trip over
/// `$XDG_RUNTIME_DIR/mach-kb.sock`, not just a file-exists check.
/// Checks that every judged consolidation pass can actually reach work.
///
/// This exists because the same mistake happened three times in one day.
/// `INSIGHT_DEDUPE_MIN_SIM`, `ENTITY_MERGE_MIN_SIM` and `DEDUPE_MIN_SIM`
/// were each set above what this embedding space produces for real prose,
/// so three passes ran every three hours, judged nothing, reported zero
/// work and no errors, and looked perfectly healthy. Insight duplicates
/// accumulated to 34 rows and entity duplicates to 420 while the machinery
/// meant to fold them was unreachable by construction.
///
/// The signature of a dead threshold is exact and cheap to test: no
/// candidates available AND nothing ever recorded in that pass's seen
/// table, while the source table holds enough rows to form pairs at all.
/// "No candidates but plenty judged" is the healthy steady state and must
/// not alarm; "nothing judged, ever, and nothing to judge" is a threshold
/// that cannot be met.
fn check_thresholds(conn_result: &Result<Connection, KbError>) -> health::Check {
    let name = "thresholds".to_string();
    let conn = match conn_result {
        Ok(c) => c,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    // (label, rows available to pair, pairs judged so far, candidates now)
    let probes: [(&str, &str, &str); 3] = [
        ("memory dedupe", "SELECT COUNT(*) FROM memories WHERE invalidated_at IS NULL AND embedding IS NOT NULL", "SELECT COUNT(*) FROM dedupe_seen"),
        ("insight dedupe", "SELECT COUNT(*) FROM insights WHERE invalidated_at IS NULL AND embedding IS NOT NULL", "SELECT COUNT(*) FROM insight_dedupe_seen"),
        ("entity merge", "SELECT COUNT(*) FROM entities WHERE embedding IS NOT NULL", "SELECT COUNT(*) FROM entity_merge_seen"),
    ];
    let mut dead = Vec::new();
    let mut detail = Vec::new();
    for (label, rows_sql, seen_sql) in probes {
        let rows: i64 = conn.query_row(rows_sql, [], |r| r.get(0)).unwrap_or(0);
        let judged: i64 = conn.query_row(seen_sql, [], |r| r.get(0)).unwrap_or(0);
        // Fewer than two rows cannot form a pair, so silence proves nothing.
        if rows >= 2 && judged == 0 {
            dead.push(label);
        }
        detail.push(format!("{} {} rows/{} judged", label, rows, judged));
    }
    let ok = dead.is_empty();
    let detail = if ok {
        detail.join(", ")
    } else {
        format!("never judged a pair (threshold likely unreachable): {} | {}", dead.join(", "), detail.join(", "))
    };
    health::Check { name, ok, detail }
}

/// Reports drifted projects and projects whose directory has vanished.
///
/// Not a failure when a project is merely drifted — that is a prompt to
/// re-index, and the SessionStart notice already carries it. A directory
/// that no longer exists IS a failure: its memories are now unreachable
/// from any session and only a human knows whether it moved or died.
fn check_projects(conn_result: &Result<Connection, KbError>) -> health::Check {
    let name = "projects".to_string();
    let conn = match conn_result {
        Ok(c) => c,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    let now = store::now_rfc3339();
    let rows = match store::list_projects(conn) {
        Ok(r) => r,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    let mut missing = Vec::new();
    let mut drifted = 0usize;
    for p in &rows {
        let root = Path::new(&p.root_path);
        if !root.is_dir() {
            missing.push(p.name.clone());
            continue;
        }
        if matches!(
            projects::drift_state(p.indexed_commits, projects::commit_count(root), p.indexed_at.as_deref(), &now),
            projects::DriftState::Drifted { .. } | projects::DriftState::NeverIndexed
        ) {
            drifted += 1;
        }
    }
    let ok = missing.is_empty();
    let detail = if missing.is_empty() {
        format!("{} tracked, {} due a re-index", rows.len(), drifted)
    } else {
        format!(
            "{} tracked, {} due a re-index, MISSING ON DISK: {} (run `mach kb projects forget <name>` to clear a project whose directory is gone for good)",
            rows.len(),
            drifted,
            missing.join(", ")
        )
    };
    health::Check { name, ok, detail }
}

fn check_kb_socket() -> health::Check {
    let name = "kb-socket".to_string();
    let path = crate::socket::socket_path();
    if !path.exists() {
        return health::Check { name, ok: false, detail: format!("socket not found at {}", path.display()) };
    }
    let result: Result<(), String> = (|| {
        use std::os::unix::net::UnixStream;
        let mut stream = UnixStream::connect(&path).map_err(|e| e.to_string())?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let req = serde_json::json!({"op": "search", "query": "mach kb health check", "limit": 1}).to_string() + "\n";
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.contains(&b'\n') {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let line = text.lines().next().unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("bad response: {}", e))?;
        if v.get("hits").is_some() {
            Ok(())
        } else if let Some(err) = v.get("error") {
            Err(format!("socket returned an error: {}", err))
        } else {
            Err("unexpected response shape".to_string())
        }
    })();
    match result {
        Ok(()) => health::Check { name, ok: true, detail: "search responded".to_string() },
        Err(e) => health::Check { name, ok: false, detail: e },
    }
}

/// Checks that `~/.local/share/mach/recall-log` exists (creating it if
/// needed) and is actually writable — a real write-then-remove probe file,
/// not just a permissions read.
fn check_recall_log_writable() -> health::Check {
    let name = "recall-log-dir".to_string();
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return health::Check { name, ok: false, detail: "HOME is not set".to_string() },
    };
    let dir = PathBuf::from(home).join(".local/share/mach/recall-log");
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return health::Check { name, ok: false, detail: format!("cannot create {}: {}", dir.display(), e) };
    }
    let probe = dir.join(format!(".health-write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            health::Check { name, ok: true, detail: format!("{} writable", dir.display()) }
        }
        Err(e) => health::Check { name, ok: false, detail: format!("cannot write to {}: {}", dir.display(), e) },
    }
}

/// Checks free disk space at the filesystem holding `kb.db` against
/// `health::DISK_HEADROOM_WARN_BYTES`.
fn check_disk_headroom() -> health::Check {
    let name = "disk-headroom".to_string();
    let path = match store::db_path() {
        Ok(p) => p,
        Err(e) => return health::Check { name, ok: false, detail: e.to_string() },
    };
    let dir = path.parent().unwrap_or(Path::new("/"));
    match free_bytes(dir) {
        Ok(free) => health::Check {
            name,
            ok: health::disk_headroom_ok(free),
            detail: format!("{:.2} GB free at {}", free as f64 / 1e9, dir.display()),
        },
        Err(e) => health::Check { name, ok: false, detail: e.to_string() },
    }
}

/// Checks `telegram-state.json`'s own staleness (its file mtime — the only
/// timestamp it records, see `engines/telegram/src/state.rs::State`) against
/// `health::TELEGRAM_STATE_STALE_WARN_HOURS` — but ONLY when `telegram.toml`
/// exists at all; an unconfigured bridge has nothing to be stale about, so
/// this returns `None` (no check row at all) rather than a synthetic
/// failure. The bridge's own long-poll loop (`telegram::run`) persists this
/// file on every successful poll, even an empty one — not just when a real
/// update arrives — precisely so its mtime means "the poll loop is alive,"
/// not merely "a message showed up recently."
fn check_telegram_state() -> Option<health::Check> {
    let home = std::env::var("HOME").ok()?;
    let toml_path = PathBuf::from(&home).join(".local/share/mach/telegram.toml");
    if !toml_path.exists() {
        return None;
    }
    let name = "telegram-state".to_string();
    let state_path = PathBuf::from(&home).join(".local/share/mach/telegram-state.json");
    let hours = std::fs::metadata(&state_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| d.as_secs_f64() / 3600.0);
    let ok = health::telegram_state_ok(hours);
    let detail = match hours {
        Some(h) => format!("last update poll {:.1}h ago", h),
        None => format!("no {} yet", state_path.display()),
    };
    Some(health::Check { name, ok, detail })
}

/// `--notify`'s side effect: fires at most one `notify-send -u critical`
/// summarizing every failing check, suppressed for
/// `health::NOTIFY_SUPPRESS_WINDOW_SECS` when the exact same failing set was
/// already notified about — see `health::should_notify`. Clears the streak
/// file entirely once everything passes, so a *future* failure (even an
/// identical one) notifies immediately rather than staying suppressed by a
/// stale streak from a resolved incident.
fn maybe_send_health_notification(checks: &[health::Check]) {
    let failing = health::failing_names(checks);
    let path = match health_streak_path() {
        Ok(p) => p,
        Err(_) => return,
    };
    if failing.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let now_secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let last = load_streak(&path);
    if health::should_notify(&failing, last.as_ref(), now_secs) {
        let summary = format!("mach kb health: {} check(s) failing: {}", failing.len(), failing.join(", "));
        let _ = std::process::Command::new("notify-send")
            .arg("-u")
            .arg("critical")
            .arg("-a")
            .arg("mach kb health")
            .arg(&summary)
            .spawn();
        save_streak(&path, &health::NotifyStreak { key: health::streak_key(&failing), last_notified_secs: now_secs });
    }
}

// --- improve: the knowledge bank feeding back into Claude's own config ---

/// What one `mach kb improve` invocation came to (before any exit code).
#[derive(Debug)]
enum ImproveRun {
    BelowThreshold { signal: usize, min_signal: usize },
    DryRun { prompt: String },
    Done(improve::Outcome),
}

/// New signal since the improve watermark: `(new_memories, signal_relations)`.
/// This pass's own outcome memories are never "new" -- they ride along in
/// the history section instead.
fn improve_signal(conn: &Connection, state: &store::ImproveState) -> Result<(Vec<Memory>, Vec<store::Relation>), KbError> {
    let history_ids: HashSet<i64> =
        store::memories_by_source_prefix_or_project(conn, improve::OUTCOME_SOURCE_PREFIX, improve::OUTCOME_PROJECT)?
            .iter()
            .map(|m| m.id)
            .collect();
    let new_memories: Vec<Memory> = store::memories_since(conn, state.last_memory_id.unwrap_or(0))?
        .into_iter()
        .filter(|m| !history_ids.contains(&m.id))
        // Index-owned code summaries are not behavioural evidence about the
        // user (final-review item 7): a nightly index run must neither
        // trip improve's signal threshold nor pad its prompt.
        .filter(|m| !store::is_index_owned(m))
        .collect();
    let relations: Vec<store::Relation> = store::relations_since(conn, state.last_relation_id.unwrap_or(0))?
        .into_iter()
        .filter(|r| improve::is_signal_predicate(&r.predicate))
        .collect();
    Ok((new_memories, relations))
}

fn build_improve_bundle(
    conn: &Connection,
    targets: &improve::Targets,
    new_memories: Vec<Memory>,
    relations: &[store::Relation],
    now_secs: u64,
) -> Result<improve::Bundle, KbError> {
    let model_lines: Vec<String> = store::mental_model(conn)?.iter().map(format_model_row).collect();
    let history =
        store::memories_by_source_prefix_or_project(conn, improve::OUTCOME_SOURCE_PREFIX, improve::OUTCOME_PROJECT)?;
    let mut rel_lines = Vec::new();
    for r in relations {
        let name = |id: i64| -> Result<String, KbError> {
            Ok(store::get_entity(conn, id)?.map(|e| e.name).unwrap_or_else(|| format!("entity#{}", id)))
        };
        let evidence = match r.evidence_memory_id {
            Some(id) => store::get(conn, id)?.map(|m| m.content),
            None => None,
        };
        rel_lines.push(improve::RelationLine {
            id: r.id,
            src: name(r.src)?,
            predicate: r.predicate.clone(),
            dst: name(r.dst)?,
            evidence_id: r.evidence_memory_id,
            evidence,
        });
    }
    let since = store::now_rfc3339_from_secs(now_secs.saturating_sub(improve::SKILL_USAGE_WINDOW_DAYS * 86_400));
    let blobs = store::skill_usage_since(conn, &since)?;
    let skill_usage = ingest::merge_skill_usage(blobs.iter().map(|(_, j)| j.as_str()));
    Ok(improve::Bundle {
        model_lines,
        new_memories,
        history,
        relations: rel_lines,
        skill_usage,
        inventory: improve::inventory(targets),
    })
}

fn record_improve_outcome<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    outcome: &improve::Outcome,
    now: &str,
) -> Result<(), KbError> {
    let text = outcome.memory_text();
    let embedding = embedder.embed(&text).ok();
    store::insert(
        conn,
        &text,
        Some(&outcome.memory_source(now)),
        Some(improve::OUTCOME_PROJECT),
        true,
        embedding.as_deref(),
        improve::OUTCOME_IMPORTANCE,
    )?;
    Ok(())
}

/// The whole guarded run after the gate has opened: preflight, snapshot,
/// the agentic call, verification, commit, outcome memory, watermark.
/// Network-free except through `llm`/`vcs`, so tests drive it with fakes.
#[allow(clippy::too_many_arguments)]
fn run_improve<E: Embedder, L: ImproveLlm, V: Vcs>(
    conn: &Connection,
    embedder: &E,
    llm: &L,
    vcs: &V,
    targets: &improve::Targets,
    snapshot_root: &Path,
    model: &str,
    timeout: std::time::Duration,
    min_signal: usize,
    force: bool,
    dry_run: bool,
    now: &str,
    now_secs: u64,
) -> Result<ImproveRun, KbError> {
    let state = store::get_improve_state(conn)?;
    let (new_memories, relations) = improve_signal(conn, &state)?;
    let signal = new_memories.len() + relations.len();
    if !improve::should_run(signal, min_signal, force) {
        store::mark_improve_completed(conn, now)?;
        return Ok(ImproveRun::BelowThreshold { signal, min_signal });
    }

    let bundle = build_improve_bundle(conn, targets, new_memories, &relations, now_secs)?;
    let prompt = improve::build_prompt(&bundle, targets);
    if dry_run {
        return Ok(ImproveRun::DryRun { prompt });
    }

    // A failure anywhere below is recorded as an outcome memory and never
    // advances the watermark, so the same evidence gets another look.
    let finish = |outcome: improve::Outcome, advance: bool| -> Result<ImproveRun, KbError> {
        record_improve_outcome(conn, embedder, &outcome, now)?;
        if advance {
            // after the insert, so the outcome memory itself is never "new"
            store::update_improve_state(
                conn,
                now,
                Some(store::latest_memory_id(conn)?),
                Some(store::latest_relation_id(conn)?),
            )?;
        }
        store::mark_improve_completed(conn, now)?;
        // Deliberately no desktop notification: this pass runs unattended on
        // a timer and its result is not something to interrupt the user for.
        // Every outcome is already recorded as a memory (above) and a run of
        // consecutive failures is surfaced by `mach kb health`.
        Ok(ImproveRun::Done(outcome))
    };
    let fail = |reason: String| finish(improve::Outcome::Failed { reason }, false);

    // --- preflight: every target chezmoi-managed, source repo clean, no human WIP on a target
    let managed = match vcs.managed() {
        Ok(m) => m,
        Err(e) => return fail(format!("chezmoi managed: {}", e)),
    };
    let unmanaged = improve::unmanaged_targets(&managed, targets);
    if !unmanaged.is_empty() {
        let list: Vec<String> = unmanaged.iter().map(|p| p.display().to_string()).collect();
        return fail(format!("write targets not chezmoi-managed: {} (run `chezmoi add` on them)", list.join(", ")));
    }
    match vcs.dirty_targets(&targets.roots()) {
        Ok(dirty) if dirty.is_empty() => {}
        Ok(dirty) => return fail(improve::uncommitted_targets_reason(&dirty)),
        Err(e) => return fail(format!("git status: {}", e)),
    }
    let pre_status: HashSet<PathBuf> = match vcs.status() {
        Ok(v) => v.into_iter().collect(),
        Err(e) => return fail(format!("chezmoi status: {}", e)),
    };
    let drifted: Vec<PathBuf> = {
        let mut d: Vec<PathBuf> = pre_status.iter().filter(|p| targets.allows(p)).cloned().collect();
        d.sort();
        d
    };
    if !drifted.is_empty() {
        return fail(improve::pre_run_drift_reason(&drifted));
    }

    // --- snapshot, call, verify
    let snap = match improve::snapshot(targets, &snapshot_root.join(now.replace(':', "-"))) {
        Ok(s) => s,
        Err(e) => return fail(format!("snapshot: {}", e)),
    };
    let rollback = |why: String| -> String {
        match improve::restore(&snap) {
            Ok(()) => why,
            Err(e) => format!(
                "{} (AND restore failed: {} -- targets may be inconsistent, snapshot kept at {})",
                why,
                e,
                snap.dir.display()
            ),
        }
    };

    let output = match llm.run(&prompt, targets, model, timeout) {
        Ok(o) => o,
        Err(e) => return fail(rollback(format!("claude: {}", e))),
    };
    let Some(result) = improve::parse_result(&output) else {
        return fail(rollback("claude reply had no valid IMPROVE-RESULT block".to_string()));
    };

    // anything chezmoi now sees out of sync that it didn't before and that
    // is outside the targets: put it back from source, then roll back
    match vcs.status() {
        Ok(post) => {
            let strays: Vec<PathBuf> =
                post.into_iter().filter(|p| !pre_status.contains(p) && !targets.allows(p)).collect();
            if !strays.is_empty() {
                for p in &strays {
                    let _ = vcs.apply_force(p);
                }
                let list: Vec<String> = strays.iter().map(|p| p.display().to_string()).collect();
                return fail(rollback(format!(
                    "claude changed managed files outside the write targets: {}",
                    list.join(", ")
                )));
            }
        }
        Err(e) => return fail(rollback(format!("chezmoi status after run: {}", e))),
    }

    let changes = match improve::changed_files(&snap) {
        Ok(c) => c,
        Err(e) => return fail(rollback(format!("diffing targets against snapshot: {}", e))),
    };
    if result.action == improve::Action::None {
        if !changes.is_empty() {
            let list: Vec<String> = changes.iter().map(|c| c.path().display().to_string()).collect();
            return fail(rollback(format!("claude reported no action but changed: {}", list.join(", "))));
        }
        let _ = std::fs::remove_dir_all(&snap.dir);
        return finish(improve::Outcome::Nothing { rationale: result.rationale }, true);
    }
    if changes.is_empty() {
        // claimed an edit, made none -- record as a no-op, evidence still consumed
        let _ = std::fs::remove_dir_all(&snap.dir);
        return finish(
            improve::Outcome::Nothing {
                rationale: format!(
                    "(claude reported `{}` but changed no file) {}",
                    result.action.as_str(),
                    result.rationale
                ),
            },
            true,
        );
    }
    if let Err(e) = improve::verify_changes(&changes, targets, &snap) {
        return fail(rollback(format!("verification: {}", e)));
    }

    // --- commit
    for c in &changes {
        let r = match c {
            improve::Change::Added(p) | improve::Change::Modified(p) => vcs.add(p),
            improve::Change::Deleted(p) => vcs.forget(p),
        };
        if let Err(e) = r {
            return fail(rollback(format!("chezmoi add/forget {}: {}", c.path().display(), e)));
        }
    }
    let mut result = result;
    // trust the diff over the reply for the file list
    result.files = changes.iter().map(|c| c.path().to_path_buf()).collect();
    let sha = match vcs.commit(&improve::commit_message(&result), &targets.roots()) {
        Ok(s) => s,
        Err(e) => return fail(rollback(format!("git commit: {}", e))),
    };
    let _ = std::fs::remove_dir_all(&snap.dir);
    finish(improve::Outcome::Applied { result, sha }, true)
}

fn cmd_improve(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut dry_run = false;
    let mut force = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dry-run" => dry_run = true,
            "--force" => force = true,
            "-h" | "--help" => {
                println!("usage: mach kb improve [--dry-run] [--force]");
                println!("       --dry-run: build and print the evidence prompt, spawn nothing, change nothing");
                println!(
                    "       --force:   run even when new signal is below MACH_IMPROVE_MIN_SIGNAL ({})",
                    improve::DEFAULT_MIN_SIGNAL
                );
                println!(
                    "       env: MACH_IMPROVE_MODEL (default {}), MACH_IMPROVE_TIMEOUT_SECS (default {})",
                    improve::DEFAULT_MODEL,
                    improve::DEFAULT_TIMEOUT.as_secs()
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb improve: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let home = match std::env::var("HOME") {
        Ok(h) if !h.is_empty() => PathBuf::from(h),
        _ => {
            eprintln!("mach kb improve: HOME is not set");
            std::process::exit(1);
        }
    };
    let conn = store::open().map_err(to_io)?;
    let now = store::now_rfc3339();
    let now_secs = store::now_secs();
    let targets = improve::Targets::from_home(&home);
    let min_signal = improve::min_signal_from_env();

    // Cheap DB-only gate before the connectivity probe, same order as reflect.
    let state = store::get_improve_state(&conn).map_err(to_io)?;
    let (new_memories, relations) = improve_signal(&conn, &state).map_err(to_io)?;
    let signal = new_memories.len() + relations.len();
    if !improve::should_run(signal, min_signal, force) {
        store::mark_improve_completed(&conn, &now).map_err(to_io)?;
        println!("mach kb improve: {} new signal rows since last run (< {}), nothing to do", signal, min_signal);
        return Ok(());
    }
    if !dry_run && !reflect::claude_reachable() {
        println!("mach kb improve: offline, deferring");
        return Ok(());
    }

    let snapshot_root = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join("mach-improve");
    let embedder = OllamaEmbedder::new();
    let llm = improve::ProcessImproveLlm::new(home.clone());
    let vcs = improve::ProcessChezmoi;
    let run = run_improve(
        &conn,
        &embedder,
        &llm,
        &vcs,
        &targets,
        &snapshot_root,
        &improve::model_from_env(),
        improve::timeout_from_env(),
        min_signal,
        force,
        dry_run,
        &now,
        now_secs,
    )
    .map_err(to_io)?;
    match run {
        ImproveRun::BelowThreshold { signal, min_signal } => {
            println!("mach kb improve: {} new signal rows (< {}), nothing to do", signal, min_signal)
        }
        ImproveRun::DryRun { prompt } => print!("{}", prompt),
        ImproveRun::Done(improve::Outcome::Applied { result, sha }) => {
            println!("mach kb improve: {} -> commit {}", result.action.as_str(), sha);
            for f in &result.files {
                println!("  {}", f.display());
            }
            println!("  {}", result.rationale);
        }
        ImproveRun::Done(improve::Outcome::Nothing { rationale }) => {
            println!("mach kb improve: no change warranted. {}", rationale)
        }
        ImproveRun::Done(improve::Outcome::Failed { reason }) => {
            eprintln!("mach kb improve: FAILED, rolled back: {}", reason);
            std::process::exit(2);
        }
    }
    Ok(())
}

fn cmd_health(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut notify = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--notify" => notify = true,
            "-h" | "--help" => {
                println!("usage: mach kb health [--notify]");
                println!(
                    "       plain-text operational self-check (ollama, kb.db, kb socket, reflect"
                );
                println!(
                    "       cadence, disk headroom, recall-log dir, telegram-state staleness);"
                );
                println!("       exits nonzero if any check fails.");
                println!(
                    "       --notify: also sends one notify-send -u critical summarizing failing"
                );
                println!(
                    "       checks, suppressed for 24h once the same failing set has been notified."
                );
                return Ok(());
            }
            other => {
                eprintln!("mach kb health: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let conn_result = store::open();

    let mut checks: Vec<health::Check> = vec![
        check_ollama(),
        check_kb_db(&conn_result),
        check_reflect_completion(&conn_result),
        check_improve(&conn_result),
        check_projects(&conn_result),
        check_thresholds(&conn_result),
        check_kb_socket(),
        check_recall_log_writable(),
        check_disk_headroom(),
    ];
    if let Some(c) = check_telegram_state() {
        checks.push(c);
    }

    print!("{}", health::format_report(&checks));
    let failed = health::any_failed(&checks);

    if notify {
        maybe_send_health_notification(&checks);
    }

    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: store::ModelKind, confidence: f64, text: &str, doubted: bool, nested: bool) -> store::ModelRow {
        recent_row(kind, confidence, text, doubted, nested, false)
    }

    fn recent_row(
        kind: store::ModelKind,
        confidence: f64,
        text: &str,
        doubted: bool,
        nested: bool,
        recent: bool,
    ) -> store::ModelRow {
        store::ModelRow { kind, confidence, text: text.to_string(), doubted, nested, recent }
    }

    // -- code index CLI argument validation (final-review items 10, 12) --

    fn sargs(v: &[&str]) -> std::vec::IntoIter<String> {
        v.iter().map(|s| s.to_string()).collect::<Vec<_>>().into_iter()
    }

    #[test]
    fn parse_index_args_rejects_a_non_numeric_budget_and_a_prefix_without_project() {
        assert!(parse_index_args(sargs(&["--budget", "lots"])).unwrap_err().contains("--budget"));
        assert!(parse_index_args(sargs(&["--budget"])).unwrap_err().contains("--budget"));
        assert!(parse_index_args(sargs(&["--path-prefix", "src"])).unwrap_err().contains("--project"));
        assert!(parse_index_args(sargs(&["--bogus"])).unwrap_err().contains("--bogus"));
        let Some(o) = parse_index_args(sargs(&["--project", "p", "--budget", "7", "--path-prefix", "src", "--dry-run"])).unwrap() else {
            panic!("not help");
        };
        assert_eq!((o.project.as_deref(), o.budget, o.path_prefix.as_deref(), o.dry_run), (Some("p"), 7, Some("src"), true));
        assert!(parse_index_args(sargs(&["--help"])).unwrap().is_none());
    }

    #[test]
    fn parse_index_args_parses_history_only_and_rejects_bad_combinations() {
        assert!(parse_index_args(sargs(&["--history-only"])).unwrap_err().contains("--project"));
        assert!(parse_index_args(sargs(&["--project", "p", "--history-only", "--path-prefix", "src"]))
            .unwrap_err()
            .contains("--path-prefix"));
        let Some(o) = parse_index_args(sargs(&["--project", "p", "--history-only", "--budget", "12"])).unwrap() else {
            panic!("not help");
        };
        assert_eq!((o.project.as_deref(), o.budget, o.path_prefix.as_deref(), o.history_only), (Some("p"), 12, None, true));
        assert!(!parse_index_args(sargs(&["--project", "p"])).unwrap().unwrap().history_only, "default is false");
    }

    #[test]
    fn resolve_ask_project_errors_on_an_explicit_unknown_name() {
        let conn = store::open_with_path(Path::new(":memory:")).unwrap();
        let err = resolve_ask_project(&conn, Some("nope")).unwrap_err();
        assert!(err.to_string().contains("nope"), "{}", err);
    }

    #[test]
    fn parse_rounds_flag_rejects_non_numeric() {
        assert_eq!(parse_rounds_value(Some("2".to_string())).unwrap(), 2);
        assert!(parse_rounds_value(Some("two".to_string())).is_err());
        assert!(parse_rounds_value(None).is_err());
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

    #[test]
    fn model_budget_none_renders_all_rows() {
        let rows = vec![
            row(store::ModelKind::Theme, 0.8, "a theme", false, false),
            row(store::ModelKind::Belief, 0.6, "a belief", false, true),
        ];
        let rendered = render_model_text(&rows, None);
        assert_eq!(rendered, format!("{}\n{}\n", format_model_row(&rows[0]), format_model_row(&rows[1])));
    }

    #[test]
    fn model_budget_never_exceeds_max_chars() {
        let rows: Vec<store::ModelRow> = (0..50)
            .map(|i| row(store::ModelKind::Belief, 0.5, &format!("belief number {}", i), false, false))
            .collect();

        let rendered = render_model_text(&rows, Some(200));

        assert!(rendered.chars().count() <= 200, "rendered {} chars, over budget", rendered.chars().count());
        assert!(rendered.starts_with(&format_model_row(&rows[0])), "first row must still be present");
    }

    #[test]
    fn model_budget_skips_children_of_a_skipped_theme() {
        // A theme line long enough that it alone blows the budget, with
        // short nested children that would each individually fit -- none of
        // them may appear once their parent theme line was skipped.
        let long_theme_text = "x".repeat(300);
        let rows = vec![
            row(store::ModelKind::Theme, 0.8, &long_theme_text, false, false),
            row(store::ModelKind::Belief, 0.6, "short child a", false, true),
            row(store::ModelKind::Belief, 0.55, "short child b", false, true),
        ];

        let rendered = render_model_text(&rows, Some(50));
        assert!(rendered.is_empty(), "theme skipped for budget must skip its nested children too, got: {rendered:?}");
    }

    #[test]
    fn model_budget_keeps_children_that_fit_when_the_theme_itself_fits() {
        // The theme fits the budget; among its children, one fits and one
        // does not -- the fitting child must still be emitted, since it is
        // the theme being skipped (not the budget alone) that skips children.
        let rows = vec![
            row(store::ModelKind::Theme, 0.8, "a theme", false, false),
            row(store::ModelKind::Belief, 0.6, "a short child", false, true),
            row(store::ModelKind::Belief, 0.55, &"y".repeat(300), false, true),
        ];
        let theme_line = format_model_row(&rows[0]);
        let child_a_line = format_model_row(&rows[1]);
        let budget = theme_line.chars().count() + 1 + child_a_line.chars().count() + 1 + 5;

        let rendered = render_model_text(&rows, Some(budget));
        assert!(rendered.contains(&theme_line), "the theme itself fits and must be emitted");
        assert!(rendered.contains(&child_a_line), "a child that fits under a theme that fit must be emitted");
        assert!(!rendered.contains("yyyy"), "the oversized second child must not be emitted");
    }

    #[test]
    fn model_budget_reserves_at_least_30_percent_for_recent_rows() {
        // 10 old, non-recent, higher-ranked rows -- by rank alone these
        // would fill the whole budget and leave nothing for the 3 recent
        // rows ranked below them.
        let mut rows: Vec<store::ModelRow> = (0..10)
            .map(|i| recent_row(store::ModelKind::Belief, 0.9, &format!("old belief {i}"), false, false, false))
            .collect();
        for i in 0..3 {
            rows.push(recent_row(store::ModelKind::Belief, 0.5, &format!("recent belief {i}"), false, false, true));
        }

        // Exactly enough room for all 10 old rows and nothing else, absent
        // any reservation for recent rows.
        let old_line_len = format_model_row(&rows[0]).chars().count() + 1;
        let budget = old_line_len * 10;

        let rendered = render_model_text(&rows, Some(budget));
        let recent_chars: usize = rows
            .iter()
            .filter(|r| r.recent)
            .map(format_model_row)
            .filter(|line| rendered.contains(line.as_str()))
            .map(|line| line.chars().count() + 1)
            .sum();

        assert!(
            recent_chars * 10 >= budget * 3,
            "recent rows got {recent_chars} of {budget} budget chars, want >= 30%"
        );
        assert!(
            rendered.lines().any(|l| l.contains("recent belief")),
            "at least one recent row must have made it in, got: {rendered:?}"
        );
    }

    #[test]
    fn model_budget_never_emits_the_same_row_twice() {
        let rows = vec![
            row(store::ModelKind::Theme, 0.9, "theme one", false, false),
            row(store::ModelKind::Belief, 0.85, "child of theme one", false, true),
            recent_row(store::ModelKind::Belief, 0.4, "a recent belief", false, false, true),
            row(store::ModelKind::Belief, 0.3, "an old belief", false, false),
        ];
        let rendered = render_model_text(&rows, Some(1000));
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), rows.len(), "every row fits comfortably in this budget and must appear exactly once");
        let mut seen = std::collections::HashSet::new();
        for l in &lines {
            assert!(seen.insert(*l), "row rendered more than once: {l}");
        }
    }

    #[test]
    fn model_budget_never_orphans_a_recent_child_whose_theme_never_fits() {
        // The theme line alone is too big for any of the three passes; its
        // nested child is flagged recent, but the recency reservation
        // (pass 2) must not let it appear without its theme.
        let long_theme_text = "x".repeat(2000);
        let rows = vec![
            row(store::ModelKind::Theme, 0.9, &long_theme_text, false, false),
            recent_row(store::ModelKind::Belief, 0.8, "recent child", false, true, true),
        ];
        let rendered = render_model_text(&rows, Some(100));
        assert!(
            rendered.is_empty(),
            "a recent child must not appear once its own theme can never be selected, got: {rendered:?}"
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

    /// Shared seeding for a single dedupe candidate pair: inserts the pair
    /// via `insert_pair` and marks one side as "new" so
    /// `reflect::dedupe_candidate_pairs` surfaces it. Used by both the
    /// plain `run_dedupe_pass` tests and the `LoggedLlm` wiring test below,
    /// so the seeding logic lives in exactly one place. Returns the pair ids,
    /// the set of new ids, and the current timestamp.
    fn seed_one_dedupe_pair(conn: &Connection) -> ((i64, i64), HashSet<i64>, String) {
        let (a, b) = insert_pair(conn);
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        ((a, b), new_ids, store::now_rfc3339())
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
        assert!(failed, "caller must report the failure");
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
        let ((a, b), new_ids, now) = seed_one_dedupe_pair(&conn);
        let llm = FixedReflectLlm { reply: Ok("DISTINCT") };
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 0);
        assert!(!failed);
        let seen = store::dedupe_seen_pairs(&conn).unwrap();
        // Both memories from the seeded pair are still active (DISTINCT
        // never merges), so they're exactly the (lo, hi) pair recorded.
        let mut ids = [a, b];
        ids.sort_unstable();
        assert_eq!(store::active_memories_for_dormancy(&conn).unwrap().len(), 2, "seeding must leave exactly the two paired memories active");
        assert_eq!(seen, [(ids[0], ids[1])].into_iter().collect());
    }

    #[test]
    fn dedupe_pass_through_logged_llm_writes_judge_log_rows() {
        let conn = mem_conn();
        // Seed exactly as the existing DISTINCT dedupe test above does, so
        // one candidate pair exists and `new_ids` covers it.
        let (_pair_ids, new_ids, now) = seed_one_dedupe_pair(&conn);
        let inner = FixedReflectLlm { reply: Ok("DISTINCT") };
        let llm = reflect::LoggedLlm::new(&conn, "dedupe", &inner);

        run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();

        let (n, pass, reply): (i64, String, String) = conn
            .query_row("SELECT count(*), max(pass), max(reply) FROM judge_log", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert_eq!((n, pass.as_str(), reply.as_str()), (1, "dedupe", "DISTINCT"));
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

    #[test]
    fn dedupe_pass_never_tombstones_a_pinned_row() {
        // The bug this closes: a restored row got re-tombstoned by the
        // very next dedupe pass. Pin the loser (as `mach kb restore`
        // would) and confirm KEEP never merges it away.
        let conn = mem_conn();
        let (a, b) = insert_pair(&conn);
        let now = store::now_rfc3339();
        let sibling = store::insert(&conn, "throwaway", None, None, true, None, 5).unwrap();
        store::supersede(&conn, b, sibling, &now).unwrap();
        store::restore(&conn, b, &now).unwrap();
        assert!(store::is_pinned(&conn, b).unwrap(), "test setup must actually pin b");

        let llm = OwnedReplyLlm { reply: format!("KEEP {}", a) };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 0, "a pinned loser must never be merged away");
        assert!(!failed);

        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded(), "pinned loser stays active");
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(
            store::dedupe_seen_pairs(&conn).unwrap(),
            [(lo, hi)].into_iter().collect(),
            "the pair must be recorded as judged so it isn't re-asked every run"
        );
    }

    #[test]
    fn dedupe_pass_blocked_by_supersession_guard_marks_seen_not_merged() {
        // Same shape as the pinned-row test above, but the loser is undated
        // — the loser names a specific date the winner drops, so
        // `store::supersession_guard`'s date check must block the merge
        // even though the judge said KEEP.
        let conn = mem_conn();
        let a = store::insert(
            &conn,
            "as of 2026-09-07 the bank held 150 memories",
            None,
            None,
            true,
            Some(&unit_vec(4, 0)),
            5,
        )
        .unwrap();
        let b = store::insert(
            &conn,
            "the bank holds many memories now",
            None,
            None,
            true,
            Some(&unit_vec(4, 0)),
            5,
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = OwnedReplyLlm { reply: format!("KEEP {}", b) }; // b (undated) would win, a (dated) would lose
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 0, "a dated loser must never be merged away on KEEP alone");
        assert!(!failed);

        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded(), "dated loser stays active");
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(
            store::dedupe_seen_pairs(&conn).unwrap(),
            [(lo, hi)].into_iter().collect(),
            "the pair must be recorded as judged so it isn't re-asked every run"
        );
    }

    #[test]
    fn dedupe_pass_never_tombstones_an_index_owned_loser() {
        // Fix-list item 6: same shape as the pinned-row and date-guard
        // tests above, but the loser is an index-owned code-index summary
        // mirror -- `store::supersession_guard`'s "index-owned" block must
        // stop a dedupe KEEP from merging it away, end to end through
        // `run_dedupe_pass`.
        let conn = mem_conn();
        let a = store::insert(&conn, "the user drinks tea", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let b = store::upsert_index_memory(
            &conn,
            "proj",
            "code-index:proj:src/",
            "src/ handles tea brewing",
            Some(&unit_vec(4, 0)),
            &store::now_rfc3339(),
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = OwnedReplyLlm { reply: format!("KEEP {}", a) }; // a would win, b (index-owned) would lose
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 0, "an index-owned loser must never be merged away");
        assert!(!failed);

        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded(), "index-owned loser stays active");
        // Final-review item 1: index-owned rows are now excluded from the
        // candidate pool, so the pair never reaches the judge (nothing to
        // record as seen); the guard itself is covered separately.
        assert!(store::dedupe_seen_pairs(&conn).unwrap().is_empty(), "an index-owned row never forms a candidate pair");
    }

    #[test]
    fn dedupe_pass_never_tombstones_a_code_history_owned_loser() {
        // Task-3 extension of the test above: `code-history:` rows must be
        // excluded from the dedupe candidate pool exactly like
        // `code-index:` ones, via the same shared helper.
        let conn = mem_conn();
        let a = store::insert(&conn, "the team shipped picking work in August", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let b = store::upsert_index_memory(
            &conn,
            "proj",
            "code-history:proj:2026-08",
            "proj 2026-08: picking work shipped",
            Some(&unit_vec(4, 0)),
            &store::now_rfc3339(),
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = OwnedReplyLlm { reply: format!("KEEP {}", a) };
        let new_ids: HashSet<i64> = [a].into_iter().collect();
        let (deduped, failed) = run_dedupe_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(deduped, 0, "a history-owned loser must never be merged away");
        assert!(!failed);

        assert!(!store::get(&conn, b).unwrap().unwrap().is_superseded(), "history-owned loser stays active");
        assert!(store::dedupe_seen_pairs(&conn).unwrap().is_empty(), "a history-owned row never forms a candidate pair");
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

    /// Reply-fixed `ReflectLlm` that also counts calls, so a test can
    /// prove a pair never reached the judge at all.
    struct CountingReplyLlm {
        reply: String,
        calls: std::cell::Cell<usize>,
    }

    impl ReflectLlm for CountingReplyLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn improve_signal_ignores_index_owned_memories() {
        let conn = mem_conn();
        let user = store::insert(&conn, "the user rejects verbose logging", None, None, true, None, 5).unwrap();
        store::upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking", None, &store::now_rfc3339()).unwrap();
        let st = store::get_improve_state(&conn).unwrap();
        let (nm, _) = improve_signal(&conn, &st).unwrap();
        assert_eq!(nm.iter().map(|m| m.id).collect::<Vec<_>>(), vec![user]);
    }

    #[test]
    fn improve_signal_ignores_code_history_owned_memories_too() {
        // Task-3 extension: a nightly commit-history summary is derived
        // prose about code, not behavioural evidence about the user, same
        // as a code-index summary.
        let conn = mem_conn();
        let user = store::insert(&conn, "the user rejects verbose logging", None, None, true, None, 5).unwrap();
        store::upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: aug work", None, &store::now_rfc3339()).unwrap();
        let st = store::get_improve_state(&conn).unwrap();
        let (nm, _) = improve_signal(&conn, &st).unwrap();
        assert_eq!(nm.iter().map(|m| m.id).collect::<Vec<_>>(), vec![user]);
    }

    #[test]
    fn dedupe_pass_never_lets_an_index_owned_row_win_over_a_user_memory() {
        // KEEP names the index row: pre-fix this tombstoned the user memory
        // into text the indexer later replaces wholesale.
        let conn = mem_conn();
        let user = store::insert(&conn, "src/ handles picking", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let idx = store::upsert_index_memory(
            &conn,
            "helios",
            "code-index:helios:src/",
            "src/ handles picking",
            Some(&unit_vec(4, 0)),
            &store::now_rfc3339(),
        )
        .unwrap();
        let llm = CountingReplyLlm { reply: format!("KEEP {}", idx), calls: std::cell::Cell::new(0) };
        let new_ids: HashSet<i64> = [user, idx].into_iter().collect();
        let (deduped, _) = run_dedupe_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(deduped, 0);
        assert_eq!(llm.calls.get(), 0, "index-owned rows are excluded from the dedupe pool");
        assert!(!store::get(&conn, user).unwrap().unwrap().is_superseded());
        assert!(!store::get(&conn, idx).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn contradiction_pass_never_lets_a_newer_index_owned_row_win_over_a_user_memory() {
        // newer_wins would pick the (newer) index row as the winner.
        let conn = mem_conn();
        let user = store::insert(&conn, "TESTBOSS: Moses is the user's boss", None, None, true, Some(&[1.0f32, 0.0]), 5).unwrap();
        let idx = store::upsert_index_memory(
            &conn,
            "helios",
            "code-index:helios:/",
            "TESTBOSS: the user's boss Ivar approved the RHI redesign",
            Some(&[1.0f32, 1.0]),
            "2099-01-01T00:00:00Z",
        )
        .unwrap();
        let llm = CountingReplyLlm { reply: "CONFLICT".to_string(), calls: std::cell::Cell::new(0) };
        let new_ids: HashSet<i64> = [user, idx].into_iter().collect();
        let (resolved, _) = run_contradiction_pass(&conn, &llm, &new_ids, &store::now_rfc3339()).unwrap();
        assert_eq!(resolved, 0);
        assert_eq!(llm.calls.get(), 0, "index-owned rows are excluded from the contradiction pool");
        assert!(!store::get(&conn, user).unwrap().unwrap().is_superseded());
    }

    #[test]
    fn apply_contradiction_verdict_blocks_an_index_owned_winner_even_if_it_reached_the_judge() {
        // Belt and braces below the pool filter: the guard itself refuses.
        let conn = mem_conn();
        let user = store::insert(&conn, "src/ handles picking", None, None, true, None, 5).unwrap();
        let user_date = store::get(&conn, user).unwrap().unwrap().created_at;
        let idx =
            store::upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking and rendering", None, "2099-01-01T00:00:00Z")
                .unwrap();
        let now = store::now_rfc3339();
        let applied = apply_contradiction_verdict(
            &conn,
            reflect::ContradictionVerdict::Conflict,
            user,
            &user_date,
            idx,
            "2099-01-01T00:00:00Z",
            &now,
        )
        .unwrap();
        assert!(!applied);
        assert!(!store::get(&conn, user).unwrap().unwrap().is_superseded());
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
        assert!(failed, "caller must report the failure");
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
    fn contradiction_pass_never_tombstones_a_pinned_row() {
        // Same bug, contradiction side: pin the would-be loser (as `mach
        // kb restore` would) and confirm CONFLICT never supersedes it.
        let conn = mem_conn();
        let (a, b) = insert_contradiction_pair(&conn); // a = Moses (old, would-be loser)
        let now = store::now_rfc3339();
        let sibling = store::insert(&conn, "throwaway", None, None, true, None, 5).unwrap();
        store::supersede(&conn, a, sibling, &now).unwrap();
        store::restore(&conn, a, &now).unwrap();
        assert!(store::is_pinned(&conn, a).unwrap(), "test setup must actually pin a");

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 0, "a pinned loser must never be superseded");
        assert!(!failed);

        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded(), "pinned loser stays active");
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(
            store::contradiction_seen_pairs(&conn).unwrap(),
            [(lo, hi)].into_iter().collect(),
            "the pair must be recorded as judged so it isn't re-asked every run"
        );
    }

    #[test]
    fn contradiction_pass_blocked_by_supersession_guard_marks_seen_not_tombstoned() {
        // Same shape as the pinned-row test above, but via the
        // deterministic date guard: the would-be loser names a date the
        // would-be winner drops, so CONFLICT must never supersede it.
        let conn = mem_conn();
        let a = store::insert(
            &conn,
            "as of 2026-09-07 the bank held 150 memories",
            None,
            None,
            true,
            Some(&[1.0f32, 0.0]),
            5,
        )
        .unwrap(); // dated, would-be loser (older -> newer_wins picks b)
        let b = store::insert(
            &conn,
            "the bank now holds a different count of memories entirely",
            None,
            None,
            true,
            Some(&[1.0f32, 1.0]),
            5,
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 0, "a dated loser must never be superseded on CONFLICT alone");
        assert!(!failed);

        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded(), "dated loser stays active");
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(
            store::contradiction_seen_pairs(&conn).unwrap(),
            [(lo, hi)].into_iter().collect(),
            "the pair must be recorded as judged so it isn't re-asked every run"
        );
    }

    #[test]
    fn contradiction_pass_never_tombstones_an_index_owned_loser() {
        // Fix-list item 6: same shape as the pinned-row and date-guard
        // tests above, but the would-be loser is an index-owned code-index
        // summary mirror -- `store::supersession_guard`'s "index-owned"
        // block must stop a CONFLICT verdict from superseding it.
        let conn = mem_conn();
        let a = store::upsert_index_memory(
            &conn,
            "proj",
            "code-index:proj:src/",
            "TESTBOSS: src/ handles the user's boss module",
            Some(&[1.0f32, 0.0]),
            &store::now_rfc3339(),
        )
        .unwrap(); // index-owned, would-be loser (older -> newer_wins picks b)
        let b = store::insert(
            &conn,
            "TESTBOSS: the user's boss module was rewritten entirely",
            None,
            None,
            true,
            Some(&[1.0f32, 1.0]),
            5,
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 0, "an index-owned loser must never be superseded on CONFLICT alone");
        assert!(!failed);

        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded(), "index-owned loser stays active");
        // Final-review item 1: index-owned rows are now excluded from the
        // candidate pool, so the pair never reaches the judge (nothing to
        // record as seen); the guard itself is covered separately.
        assert!(store::contradiction_seen_pairs(&conn).unwrap().is_empty(), "an index-owned row never forms a candidate pair");
    }

    #[test]
    fn contradiction_pass_never_tombstones_a_code_history_owned_loser() {
        // Task-3 extension: `code-history:` rows must be excluded from the
        // contradiction candidate pool exactly like `code-index:` ones.
        let conn = mem_conn();
        let a = store::upsert_index_memory(
            &conn,
            "proj",
            "code-history:proj:2026-08",
            "TESTBOSS: proj 2026-08 touched the user's boss module",
            Some(&[1.0f32, 0.0]),
            &store::now_rfc3339(),
        )
        .unwrap();
        let b = store::insert(
            &conn,
            "TESTBOSS: the user's boss module was rewritten entirely",
            None,
            None,
            true,
            Some(&[1.0f32, 1.0]),
            5,
        )
        .unwrap();
        let now = store::now_rfc3339();

        let llm = FixedReflectLlm { reply: Ok("CONFLICT") };
        let new_ids: HashSet<i64> = [b].into_iter().collect();
        let (resolved, failed) = run_contradiction_pass(&conn, &llm, &new_ids, &now).unwrap();
        assert_eq!(resolved, 0, "a history-owned loser must never be superseded on CONFLICT alone");
        assert!(!failed);

        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded(), "history-owned loser stays active");
        assert!(store::contradiction_seen_pairs(&conn).unwrap().is_empty(), "a history-owned row never forms a candidate pair");
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
        assert!(failed2, "a failed judge call must be reported");

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

    // --- curation pass: schema-accelerated consolidation ---

    fn insert_settled_unreviewed_with_embedding(conn: &Connection, content: &str, importance: i64, embedding: &[f32]) -> i64 {
        let id = store::insert(conn, content, Some("session-digest"), None, false, Some(embedding), importance).unwrap();
        let old_ts = store::now_rfc3339_from_secs(store::now_secs() - 10 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![old_ts, id]).unwrap();
        id
    }

    #[test]
    fn run_curation_pass_promote_near_an_existing_insight_gets_the_schema_stability_boost() {
        let conn = mem_conn();
        let emb = unit_vec(4, 0);
        store::insert_insight(&conn, "an existing durable insight", 0.7, &["1".to_string(), "2".to_string()], Some(&emb)).unwrap();
        let id = insert_settled_unreviewed_with_embedding(&conn, "a fact that fits the existing schema", 5, &emb);
        // stability starts at importance * 7.0 = 35.0
        assert_eq!(store::get(&conn, id).unwrap().unwrap().stability, Some(35.0));

        let llm = FixedReflectLlm { reply: Ok("PROMOTE") };
        let (_examined, promoted, _demoted, failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(promoted, 1);
        assert!(!failed);
        let m = store::get(&conn, id).unwrap().unwrap();
        assert!(m.reviewed);
        assert_eq!(
            m.stability,
            Some(35.0 * reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER),
            "a promotion coherent with an existing insight must get the schema-consolidation boost"
        );
    }

    #[test]
    fn run_curation_pass_promote_an_orphan_fact_keeps_the_default_stability() {
        let conn = mem_conn();
        let emb = unit_vec(4, 0);
        // An insight exists, but its embedding is orthogonal to the
        // promoted fact's own -- nothing coherent to attach to.
        store::insert_insight(&conn, "an unrelated insight", 0.7, &["1".to_string(), "2".to_string()], Some(&unit_vec(4, 1)))
            .unwrap();
        let id = insert_settled_unreviewed_with_embedding(&conn, "an orphan fact", 5, &emb);

        let llm = FixedReflectLlm { reply: Ok("PROMOTE") };
        let (_examined, promoted, _demoted, failed) = run_curation_pass(&conn, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(promoted, 1);
        assert!(!failed);
        let m = store::get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(35.0), "an orphan promotion must keep its ordinary default stability");
    }

    #[test]
    fn run_curation_pass_engagement_fast_path_also_gets_the_schema_boost() {
        let conn = mem_conn();
        let emb = unit_vec(4, 0);
        store::insert_insight(&conn, "an existing durable insight", 0.7, &["1".to_string(), "2".to_string()], Some(&emb)).unwrap();
        let id = insert_settled_unreviewed_with_embedding(&conn, "used twice, and coherent with the schema", 5, &emb);
        let now = store::now_rfc3339();
        store::touch(&conn, &[id], &now).unwrap();
        store::touch(&conn, &[id], &now).unwrap();
        // Read back whatever touch() actually grew stability to (interval-
        // aware now, not necessarily a flat 1.3x per call) so this test
        // only asserts the schema multiplier is layered on top of it.
        let stability_after_touches = store::get(&conn, id).unwrap().unwrap().stability.unwrap();

        let llm = FixedReflectLlm { reply: Err("must never be called") };
        let (_examined, promoted, _demoted, failed) = run_curation_pass(&conn, &llm, &now).unwrap();
        assert_eq!(promoted, 1);
        assert!(!failed);
        let m = store::get(&conn, id).unwrap().unwrap();
        let expected = (stability_after_touches * reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER).min(365.0);
        assert!(
            (m.stability.unwrap() - expected).abs() < 1e-6,
            "the fast path is still a promotion and must get the schema boost too, got {:?}",
            m.stability
        );
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
        let llm =
            FixedReflectLlm { reply: Ok("STATED: The user prefers dark mode.\nINFERRED: The user works on project Zenith.") };
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

        // The durable record `mach kb recall-stats` reads: both ids were
        // shown (the judge returned a verdict for both), only one engaged.
        let stats = store::recall_stats_since(&conn, "2000-01-01T00:00:00Z").unwrap();
        assert_eq!(stats, vec![store::RecallStatsRow { session_id: "sess-b".to_string(), shown: 2, engaged: 1 }]);
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

        // The rejected reply's all-SHOWN fallback is not a genuine judge
        // verdict, so it must NOT be durably recorded via
        // `record_engagement` -- recall-stats would otherwise claim the
        // judge reviewed and passed on this id, when it actually just
        // failed to produce a usable reply.
        let stats = store::recall_stats_since(&conn, "2000-01-01T00:00:00Z").unwrap();
        assert_eq!(stats, vec![], "a rejected verdict reply's all-SHOWN fallback must write no recall-stats row");
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

    // --- recall-log pruning ---

    /// Writes `<root>/recall-log/<session_id>.jsonl` and backdates its mtime
    /// by `age_secs` (or leaves it at "now" when `None`) -- mirrors
    /// `write_transcript`'s own mtime-backdating helper, since pruning's
    /// age gate is real-file-mtime-driven too.
    fn write_aged_recall_log(root: &Path, session_id: &str, age_secs: Option<u64>) -> PathBuf {
        write_recall_log(root, session_id, &[r#"{"ts":"2026-01-01T00:00:00Z","ids":[1]}"#]);
        let path = root.join("recall-log").join(format!("{}.jsonl", session_id));
        if let Some(age) = age_secs {
            let mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
            set_mtime(&path, mtime);
        }
        path
    }

    #[test]
    fn prune_recall_logs_deletes_an_ingested_old_enough_log() {
        let scratch = ScratchDir::new("prune-ok");
        let conn = mem_conn();
        store::mark_session_ingested(&conn, "sess-old", &store::now_rfc3339()).unwrap();
        let path = write_aged_recall_log(&scratch.path, "sess-old", Some(ingest::RECALL_LOG_PRUNE_MIN_AGE_SECS + 60));

        let pruned = prune_recall_logs(&conn, &scratch.path.join("recall-log"));

        assert_eq!(pruned, 1);
        assert!(!path.exists());
    }

    #[test]
    fn prune_recall_logs_never_touches_an_un_ingested_session_regardless_of_age() {
        let scratch = ScratchDir::new("prune-not-ingested");
        let conn = mem_conn();
        // Deliberately never marked ingested.
        let path = write_aged_recall_log(&scratch.path, "sess-never-ingested", Some(ingest::RECALL_LOG_PRUNE_MIN_AGE_SECS * 10));

        let pruned = prune_recall_logs(&conn, &scratch.path.join("recall-log"));

        assert_eq!(pruned, 0, "an un-ingested session's recall log must never be pruned, no matter its age");
        assert!(path.exists());
    }

    #[test]
    fn prune_recall_logs_keeps_an_ingested_but_too_fresh_log() {
        let scratch = ScratchDir::new("prune-too-fresh");
        let conn = mem_conn();
        store::mark_session_ingested(&conn, "sess-fresh", &store::now_rfc3339()).unwrap();
        let path = write_aged_recall_log(&scratch.path, "sess-fresh", Some(ingest::RECALL_LOG_PRUNE_MIN_AGE_SECS - 60));

        let pruned = prune_recall_logs(&conn, &scratch.path.join("recall-log"));

        assert_eq!(pruned, 0, "younger than the 14-day floor must be kept regardless of ingested status");
        assert!(path.exists());
    }

    #[test]
    fn prune_recall_logs_missing_root_is_a_silent_no_op() {
        let scratch = ScratchDir::new("prune-missing-root");
        let conn = mem_conn();
        let pruned = prune_recall_logs(&conn, &scratch.path.join("recall-log")); // never created
        assert_eq!(pruned, 0);
    }

    #[test]
    fn run_ingest_sessions_prunes_recall_logs_of_already_ingested_sessions_at_the_end_of_the_run() {
        let scratch = ScratchDir::new("prune-end-of-run");
        let projects_root = scratch.path.join("projects");
        let recall_log_root = scratch.path.join("recall-log");
        let conn = mem_conn();

        // A session ingested in some earlier run, whose recall log has
        // long since aged out -- pruning must catch this even though
        // nothing about it is scanned as a candidate this run (no fresh
        // transcript, already in `ingested_sessions`).
        store::mark_session_ingested(&conn, "sess-stale-ingested", &store::now_rfc3339()).unwrap();
        let old_log = write_aged_recall_log(&scratch.path, "sess-stale-ingested", Some(ingest::RECALL_LOG_PRUNE_MIN_AGE_SECS + 1));

        let embedder = FakeEmbedder;
        let llm = FixedReflectLlm { reply: Ok("") };
        let filter = FixedFilter { dialogue: None };

        let summary =
            run_ingest_sessions(&conn, &embedder, &llm, &filter, &projects_root, &recall_log_root, None, &store::now_rfc3339())
                .unwrap();

        assert_eq!(summary.pruned_recall_logs, 1);
        assert!(!old_log.exists());
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

    // --- graph layer: entity resolution, edge conflict, extraction pass ---

    /// An `Embedder` test double that always returns the same fixed vector
    /// regardless of input text -- lets a connections/search test control
    /// exactly which entity a query embedding matches, without depending on
    /// `FakeEmbedder`'s byte-length-sensitive cosine behavior.
    struct FixedVecEmbedder(Vec<f32>);

    impl Embedder for FixedVecEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, KbError> {
            Ok(self.0.clone())
        }
    }

    /// A SAME verdict is remembered, so the name resolves later with no
    /// call at all. Proven by a judge that would answer DIFFERENT: if it
    /// were consulted again the test would fail.
    #[test]
    fn a_judged_alias_is_cached_and_never_re_asked() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&[1.0, 0.0])).unwrap();
        store::mark_alias_verdict(&conn, "Moses Kimani", moses, true, &now).unwrap();
        let would_refuse = FixedReflectLlm { reply: Ok("DIFFERENT") };
        let (id, created) =
            resolve_or_create_entity(&conn, &FakeEmbedder, &would_refuse, "Moses Kimani", None, &now).unwrap();
        assert_eq!(id, moses);
        assert!(!created, "the cached SAME resolves without asking again");
    }

    /// A DIFFERENT verdict is remembered too, so a name that is genuinely
    /// distinct is not re-judged on every extraction that mentions it.
    #[test]
    fn a_rejected_alias_is_remembered_rather_than_re_judged() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let gh = store::insert_entity(&conn, "GitHub", Some("technology"), Some(&[1.0, 0.0])).unwrap();
        store::mark_alias_verdict(&conn, "GitHub Actions", gh, false, &now).unwrap();
        assert_eq!(store::alias_verdict(&conn, "GitHub Actions", gh).unwrap(), Some(false));
        assert_eq!(store::alias_target(&conn, "GitHub Actions").unwrap(), None, "DIFFERENT is not an alias");
    }

    /// A judge that fails must not leave a verdict behind: recording
    /// DIFFERENT would cache an answer nobody gave, and the pair would
    /// never be asked about again.
    #[test]
    fn a_failed_alias_judge_records_nothing_and_mints_the_entity() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let existing = store::insert_entity(&conn, "Remos Space", Some("organization"), Some(&[1.0, 0.0])).unwrap();
        let broken = FixedReflectLlm { reply: Err("offline") };
        let (id, created) =
            resolve_or_create_entity(&conn, &FakeEmbedder, &broken, "Remos Spaces", None, &now).unwrap();
        assert!(created, "no verdict means no merge");
        assert_ne!(id, existing);
        assert_eq!(
            store::alias_verdict(&conn, "Remos Spaces", existing).unwrap(),
            None,
            "a failed call must stay unjudged so a later run can decide"
        );
    }

    #[test]
    fn resolve_or_create_entity_reuses_an_exact_case_insensitive_name_match() {
        let conn = mem_conn();
        let existing = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, &FixedReflectLlm { reply: Ok("DIFFERENT") }, "MOSES", Some("person"), &store::now_rfc3339()).unwrap();
        assert_eq!(id, existing);
        assert!(!created);
    }

    #[test]
    fn resolve_or_create_entity_reuses_via_embedding_similarity() {
        let conn = mem_conn();
        // Same byte length, one letter apart -- FakeEmbedder's byte-vector
        // cosine between these two is comfortably above
        // ENTITY_RESOLUTION_SIM_THRESHOLD, and the exact-name path can never
        // catch this (the strings differ).
        let existing = store::insert_entity(&conn, "Moses", Some("person"), Some(&FakeEmbedder.embed("Moses").unwrap())).unwrap();
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, &FixedReflectLlm { reply: Ok("SAME") }, "Moxes", Some("person"), &store::now_rfc3339()).unwrap();
        assert_eq!(id, existing, "a near-spelling must resolve to the same entity via similarity");
        assert!(!created);
    }

    #[test]
    fn resolve_or_create_entity_creates_a_new_entity_when_nothing_matches() {
        let conn = mem_conn();
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, &FixedReflectLlm { reply: Ok("DIFFERENT") }, "Umoja", Some("project"), &store::now_rfc3339()).unwrap();
        assert!(created);
        let e = store::get_entity(&conn, id).unwrap().unwrap();
        assert_eq!(e.name, "Umoja");
        assert_eq!(e.kind.as_deref(), Some("project"));
    }

    #[test]
    fn resolve_pending_conflicts_conflict_supersedes_the_older_edge() {
        // The boss test: "Moses boss-of user" already active; a candidate
        // pair naming it as the "old" edge against a newly inserted "Ivar
        // boss-of user" edge, with a CONFLICT verdict, must tombstone the
        // Moses edge in favor of the new one -- the newly inserted edge
        // always wins, no timestamp comparison needed.
        let conn = mem_conn();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        let old_edge = store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let new_edge = store::insert_relation(&conn, ivar, "boss-of", user, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: CONFLICT") };
        let failed = resolve_pending_conflicts(&conn, &llm, &[(new_edge, old_edge)], &now).unwrap();
        assert!(!failed);
        assert!(store::get_relation(&conn, new_edge).unwrap().unwrap().is_active());
        assert!(!store::get_relation(&conn, old_edge).unwrap().unwrap().is_active(), "the older edge must be tombstoned");
        assert_eq!(store::get_relation(&conn, old_edge).unwrap().unwrap().superseded_by, Some(new_edge));
    }

    #[test]
    fn resolve_pending_conflicts_both_hold_leaves_both_edges_active() {
        let conn = mem_conn();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        let old_edge = store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let new_edge = store::insert_relation(&conn, ivar, "boss-of", user, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: BOTH_HOLD") };
        let _failed = resolve_pending_conflicts(&conn, &llm, &[(new_edge, old_edge)], &now).unwrap();
        assert!(store::get_relation(&conn, old_edge).unwrap().unwrap().is_active());
        assert!(store::get_relation(&conn, new_edge).unwrap().unwrap().is_active());
    }

    #[test]
    fn resolve_pending_conflicts_failed_judge_call_leaves_both_edges_active_and_reports_failure() {
        let conn = mem_conn();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        let old_edge = store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let new_edge = store::insert_relation(&conn, ivar, "boss-of", user, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Err("offline") };
        let failed = resolve_pending_conflicts(&conn, &llm, &[(new_edge, old_edge)], &now).unwrap();
        assert!(failed);
        assert!(store::get_relation(&conn, old_edge).unwrap().unwrap().is_active());
        assert!(store::get_relation(&conn, new_edge).unwrap().unwrap().is_active());
    }

    #[test]
    fn resolve_pending_conflicts_judges_multiple_pairs_in_one_call() {
        let conn = mem_conn();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let elena = store::insert_entity(&conn, "Elena", Some("person"), None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let tea = store::insert_entity(&conn, "tea", Some("concept"), None).unwrap();
        let coffee = store::insert_entity(&conn, "coffee", Some("concept"), None).unwrap();
        let now = store::now_rfc3339();
        let old_boss = store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let new_boss = store::insert_relation(&conn, ivar, "boss-of", user, None, Some(0.9), &now).unwrap();
        let old_pref = store::insert_relation(&conn, user, "prefers", tea, None, Some(0.9), &now).unwrap();
        let new_pref = store::insert_relation(&conn, user, "prefers", coffee, None, Some(0.9), &now).unwrap();
        let _ = elena; // unused entity, just padding the graph a bit for realism

        // One call, one reply covering both pairs -- a pair-numbered verdict
        // per line, no ids anywhere in the prompt or reply.
        let llm = FixedReflectLlm { reply: Ok("1: CONFLICT\n2: BOTH_HOLD") };
        let failed =
            resolve_pending_conflicts(&conn, &llm, &[(new_boss, old_boss), (new_pref, old_pref)], &now).unwrap();
        assert!(!failed);
        assert!(!store::get_relation(&conn, old_boss).unwrap().unwrap().is_active());
        assert!(store::get_relation(&conn, old_pref).unwrap().unwrap().is_active(), "BOTH_HOLD leaves both active");
        assert!(store::get_relation(&conn, new_pref).unwrap().unwrap().is_active());
    }

    #[test]
    fn resolve_pending_conflicts_skips_a_pair_already_resolved_earlier_in_the_same_batch() {
        // Both pairs name the SAME old edge as their loser -- once the first
        // pair's CONFLICT verdict tombstones it, the second pair must be a
        // silent no-op rather than erroring or double-tombstoning.
        let conn = mem_conn();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let elena = store::insert_entity(&conn, "Elena", Some("person"), None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        let old_edge = store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let ivar_edge = store::insert_relation(&conn, ivar, "boss-of", user, None, Some(0.9), &now).unwrap();
        let elena_edge = store::insert_relation(&conn, elena, "boss-of", user, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: CONFLICT\n2: CONFLICT") };
        let failed = resolve_pending_conflicts(&conn, &llm, &[(ivar_edge, old_edge), (elena_edge, old_edge)], &now).unwrap();
        assert!(!failed);
        assert!(!store::get_relation(&conn, old_edge).unwrap().unwrap().is_active());
        assert_eq!(store::get_relation(&conn, old_edge).unwrap().unwrap().superseded_by, Some(ivar_edge));
        assert!(store::get_relation(&conn, ivar_edge).unwrap().unwrap().is_active());
        assert!(store::get_relation(&conn, elena_edge).unwrap().unwrap().is_active());
    }

    #[test]
    fn run_insight_dedupe_pass_merges_same_and_remembers_different() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let a = store::insert_insight(&conn, "the user audits before building", 0.6, &["1".to_string()], Some(&[1.0, 0.0])).unwrap();
        let b = store::insert_insight(&conn, "the user checks state first", 0.9, &["2".to_string()], Some(&[0.99, 0.14])).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: SAME") };
        let (examined, merged, failed) = run_insight_dedupe_pass(&conn, &llm, &now).unwrap();
        assert_eq!((examined, merged, failed), (1, 1, false));
        let kept = store::get_insight(&conn, a).unwrap().unwrap();
        assert_eq!(kept.source_ids.len(), 2, "evidence is unioned onto the keeper");
        assert!((kept.confidence - 0.9).abs() < 1e-9);
        assert!(!store::get_insight(&conn, b).unwrap().unwrap().is_active());
        assert_eq!(run_insight_dedupe_pass(&conn, &llm, &now).unwrap().0, 0, "nothing left to pair");
    }

    #[test]
    fn run_insight_dedupe_pass_never_marks_seen_on_a_failed_call() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert_insight(&conn, "one phrasing", 0.6, &["1".to_string()], Some(&[1.0, 0.0])).unwrap();
        store::insert_insight(&conn, "another phrasing", 0.6, &["2".to_string()], Some(&[0.99, 0.14])).unwrap();

        let broken = FixedReflectLlm { reply: Err("offline") };
        let (examined, merged, failed) = run_insight_dedupe_pass(&conn, &broken, &now).unwrap();
        assert_eq!((examined, merged), (0, 0));
        assert!(failed);
        assert!(store::insight_dedupe_seen_pairs(&conn).unwrap().is_empty(), "retried, not written off");

        let different = FixedReflectLlm { reply: Ok("1: DIFFERENT") };
        let (examined, merged, _) = run_insight_dedupe_pass(&conn, &different, &now).unwrap();
        assert_eq!((examined, merged), (1, 0));
        assert_eq!(store::insight_dedupe_seen_pairs(&conn).unwrap().len(), 1, "and never re-judged");
    }

    // --- run_entity_card_pass ---

    /// Replies with a well-formed card citing the first two memory ids the
    /// prompt actually lists, so a batch of different entities each get a
    /// valid citation (a fixed reply could only ever cite one entity's rows).
    struct CardEchoLlm;

    impl ReflectLlm for CardEchoLlm {
        fn call(&self, _model: &str, prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            let ids: Vec<String> = prompt
                .lines()
                .filter_map(|l| l.trim().strip_prefix('[').and_then(|r| r.split(']').next()).map(str::to_string))
                .filter(|t| t.parse::<i64>().is_ok())
                .take(2)
                .collect();
            Ok(format!("- a durable line\n- another durable line\nevidence: {}", ids.join(", ")))
        }
    }

    fn carded_entity(conn: &Connection, name: &str, kind: &str, n: i64) -> i64 {
        let id = store::insert_entity(conn, name, Some(kind), None).unwrap();
        for i in 0..n {
            store::insert(conn, &format!("{} did thing {}", name, i), None, None, true, None, 5).unwrap();
        }
        id
    }

    #[test]
    fn run_entity_card_pass_builds_a_card_and_stops_being_due() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = carded_entity(&conn, "Moses", "person", store::CARD_MIN_MENTIONS);
        let llm = FixedReflectLlm {
            reply: Ok("- argues hotfixes are underrated\n- pushes for owner-settable TLEs\nevidence: 1, 2, 999"),
        };
        let (examined, built, error) = run_entity_card_pass(&conn, &FakeEmbedder, &llm, &now).unwrap();
        assert_eq!((examined, built, error), (1, 1, None));

        let card = store::get_entity_card(&conn, moses).unwrap().unwrap();
        assert!(card.text.starts_with("- argues hotfixes"));
        assert_eq!(card.source_ids, vec!["1".to_string(), "2".to_string()], "the hallucinated id 999 is dropped");
        assert_eq!(card.mention_count, store::CARD_MIN_MENTIONS);
        assert!(card.embedding.is_some());
        assert!(store::entity_card_candidates(&conn, 10).unwrap().is_empty(), "not due again until evidence moves");
    }

    #[test]
    fn run_entity_card_pass_keeps_the_old_card_on_none_or_failure() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = carded_entity(&conn, "Moses", "person", store::CARD_MIN_MENTIONS);
        store::upsert_entity_card(&conn, moses, "- the old card", &["1".to_string()], None, 0, 0, &now).unwrap();

        let none = FixedReflectLlm { reply: Ok("NONE") };
        let (examined, built, error) = run_entity_card_pass(&conn, &FakeEmbedder, &none, &now).unwrap();
        assert_eq!((examined, built, error), (1, 0, None));
        assert_eq!(store::get_entity_card(&conn, moses).unwrap().unwrap().text, "- the old card");

        let broken = FixedReflectLlm { reply: Err("offline") };
        let (examined, built, error) = run_entity_card_pass(&conn, &FakeEmbedder, &broken, &now).unwrap();
        assert_eq!((examined, built), (1, 0));
        assert_eq!(
            error.as_deref(),
            Some("offline"),
            "the failure reason reaches the caller, not just the fact of failure"
        );
        assert_eq!(store::get_entity_card(&conn, moses).unwrap().unwrap().text, "- the old card");
        assert!(!store::entity_card_candidates(&conn, 10).unwrap().is_empty(), "still due after a failure");
    }

    #[test]
    fn run_entity_card_pass_is_capped_per_run_and_skips_thin_entities() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        for i in 0..(store::CARD_MAX_PER_RUN + 2) {
            carded_entity(&conn, &format!("Entity{}", i), "concept", store::CARD_MIN_MENTIONS);
        }
        carded_entity(&conn, "Thin", "person", 1);
        let (examined, built, _) = run_entity_card_pass(&conn, &FakeEmbedder, &CardEchoLlm, &now).unwrap();
        assert_eq!(examined, store::CARD_MAX_PER_RUN, "one run never re-cards the whole graph");
        assert_eq!(built, store::CARD_MAX_PER_RUN);
        // the two entities the cap left out are still due next run
        assert_eq!(store::entity_card_candidates(&conn, 10).unwrap().len(), 2);
        let thin = store::find_entity_by_name(&conn, "Thin").unwrap().unwrap();
        assert!(store::get_entity_card(&conn, thin.id).unwrap().is_none());
    }

    #[test]
    fn graph_hops_are_appended_after_the_limit_not_squeezed_out_by_it() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let q = unit_vec(4, 0);
        // Two strong direct matches fill limit=2 outright.
        for i in 0..2 {
            store::insert(&conn, &format!("Moses direct match {}", i), None, None, true, Some(&q), 5).unwrap();
        }
        // Orthogonal to the query, reachable only over the shared entity.
        let hop = store::insert(&conn, "Moses prefers pragmatic hotfixes", None, None, true, Some(&unit_vec(4, 1)), 5)
            .unwrap();
        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "what about Moses", 2, false, false, 0.3, &now, None, None, None).unwrap();
        let direct: Vec<&SearchHit> = resp.hits.iter().filter(|h| h.via_assoc.is_none()).collect();
        assert_eq!(direct.len(), 2, "the limit still bounds query matches");
        let hopped = resp.hits.iter().find(|h| h.id == hop).expect("the hop is appended, not truncated away");
        assert_eq!(hopped.via_edge.as_deref(), Some("entity:Moses"));
        assert!(resp.hits.len() <= 2 + store::SPREAD_MAX_OUT);
    }

    #[test]
    fn search_surfaces_the_card_of_a_named_entity() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        store::insert(&conn, "Moses asked about TLEs", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::upsert_entity_card(&conn, moses, "- argues hotfixes are underrated", &["1".to_string()], None, 1, 1, &now)
            .unwrap();
        let embedder = FixedVecEmbedder(unit_vec(4, 0));
        let resp = search_hits(&conn, &embedder, "what does Moses want", 5, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.cards.len(), 1);
        assert_eq!(resp.cards[0].entity, "Moses");
        assert_eq!(resp.cards[0].kind.as_deref(), Some("person"));
        assert_eq!(resp.cards[0].evidence, 1);
        assert!(resp.cards[0].text.contains("hotfixes"));

        let none = search_hits(&conn, &embedder, "unrelated question about nothing", 5, false, false, 0.0, &now, None, None, None).unwrap();
        assert!(none.cards.is_empty());
    }

    #[test]
    fn the_project_card_is_injected_for_the_session_project_only() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let (pid, _) = store::upsert_project(&conn, "git:abc", "umoja", "/tmp/umoja", &now).unwrap();
        store::set_project_card(&conn, pid, "layout: fastapi-hub, react-app").unwrap();

        let card = project_card_for(&conn, Some("umoja")).unwrap();
        assert!(card.unwrap().contains("fastapi-hub"));
        assert!(project_card_for(&conn, Some("helios")).unwrap().is_none(), "another project's card must not leak");
        assert!(project_card_for(&conn, None).unwrap().is_none(), "no project, no card");
    }

    #[test]
    fn search_hits_carries_the_session_projects_card_when_one_is_passed() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let (pid, _) = store::upsert_project(&conn, "git:abc", "umoja", "/tmp/umoja", &now).unwrap();
        store::set_project_card(&conn, pid, "layout: fastapi-hub, react-app").unwrap();
        let embedder = FixedVecEmbedder(unit_vec(4, 0));

        let resp = search_hits(&conn, &embedder, "anything", 5, false, false, 0.0, &now, Some("umoja"), None, None).unwrap();
        let card = resp.project_card.expect("the session's own project card should ride along with its hits");
        assert!(card.contains("fastapi-hub"));

        // No project passed -- the field stays absent, same as before this
        // parameter existed.
        let none = search_hits(&conn, &embedder, "anything", 5, false, false, 0.0, &now, None, None, None).unwrap();
        assert!(none.project_card.is_none());
    }

    #[test]
    fn search_hits_down_weights_other_registered_project_memories() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let emb = unit_vec(4, 0);
        // Both tags must be REGISTERED projects, and the session project
        // must be resolved from `cwd` (not the `project` basename guess),
        // per the fix to `apply_project_penalty`.
        store::upsert_project(&conn, "git:alpha", "alpha", "/tmp/alpha", &now).unwrap();
        store::upsert_project(&conn, "git:beta", "beta", "/tmp/beta", &now).unwrap();
        // Same embedding, same importance, inserted moments apart -- equal
        // content relevance and near-identical recency/strength, so any
        // score gap between the two is attributable to the project penalty
        // alone, not to some other channel.
        store::insert(&conn, "alpha project fact about widgets", None, Some("alpha"), true, Some(&emb), 5).unwrap();
        store::insert(&conn, "beta project fact about widgets", None, Some("beta"), true, Some(&emb), 5).unwrap();
        let embedder = FixedVecEmbedder(emb);

        let resp =
            search_hits(&conn, &embedder, "widgets", 5, false, false, 0.0, &now, None, Some("/tmp/alpha"), None).unwrap();
        assert_eq!(resp.hits.len(), 2, "both memories should still surface -- down-weighted, not dropped");
        assert_eq!(resp.hits[0].project.as_deref(), Some("alpha"), "the session project's own memory ranks first");
        assert_eq!(resp.hits[1].project.as_deref(), Some("beta"));

        let alpha_score = resp.hits[0].score;
        let beta_score = resp.hits[1].score;
        assert!(alpha_score > 0.0);
        assert!(
            (beta_score - alpha_score * OTHER_PROJECT_PENALTY).abs() < 0.01,
            "beta's score ({beta_score}) should be about half alpha's ({alpha_score}), the OTHER_PROJECT_PENALTY"
        );
    }

    #[test]
    fn search_hits_does_not_penalize_an_unregistered_project_tag() {
        // (a) A memory tagged `claude-config` -- never registered as a
        // project -- must not be penalized even in a session resolved to a
        // registered project.
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let emb = unit_vec(4, 0);
        store::upsert_project(&conn, "git:alpha", "alpha", "/tmp/alpha", &now).unwrap();
        store::insert(&conn, "alpha project fact about widgets", None, Some("alpha"), true, Some(&emb), 5).unwrap();
        store::insert(&conn, "config fact about widgets", None, Some("claude-config"), true, Some(&emb), 5).unwrap();
        let embedder = FixedVecEmbedder(emb);

        let resp =
            search_hits(&conn, &embedder, "widgets", 5, false, false, 0.0, &now, None, Some("/tmp/alpha"), None).unwrap();
        assert_eq!(resp.hits.len(), 2);
        let alpha = resp.hits.iter().find(|h| h.project.as_deref() == Some("alpha")).unwrap();
        let cfg = resp.hits.iter().find(|h| h.project.as_deref() == Some("claude-config")).unwrap();
        assert!((alpha.score - cfg.score).abs() < 0.0001, "an unregistered project tag must not be penalized");
    }

    #[test]
    fn search_hits_registered_other_project_is_still_penalized_alongside_unregistered() {
        // (b) In the same session, a tag matching ANOTHER registered
        // project is still penalized even while an unregistered tag next to
        // it is not -- confirms the registered-set check discriminates
        // rather than disabling the penalty outright.
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let emb = unit_vec(4, 0);
        store::upsert_project(&conn, "git:alpha", "alpha", "/tmp/alpha", &now).unwrap();
        store::upsert_project(&conn, "git:beta", "beta", "/tmp/beta", &now).unwrap();
        store::insert(&conn, "beta project fact about widgets", None, Some("beta"), true, Some(&emb), 5).unwrap();
        store::insert(&conn, "config fact about widgets", None, Some("claude-config"), true, Some(&emb), 5).unwrap();
        let embedder = FixedVecEmbedder(emb);

        let resp =
            search_hits(&conn, &embedder, "widgets", 5, false, false, 0.0, &now, None, Some("/tmp/alpha"), None).unwrap();
        assert_eq!(resp.hits.len(), 2);
        let beta = resp.hits.iter().find(|h| h.project.as_deref() == Some("beta")).unwrap();
        let cfg = resp.hits.iter().find(|h| h.project.as_deref() == Some("claude-config")).unwrap();
        assert!(beta.score < cfg.score, "the registered other-project tag must still be penalized");
        assert!(
            (beta.score - cfg.score * OTHER_PROJECT_PENALTY).abs() < 0.01,
            "beta ({}) should be about half the unregistered tag's score ({}), the OTHER_PROJECT_PENALTY",
            beta.score,
            cfg.score
        );
    }

    #[test]
    fn search_hits_applies_no_penalty_for_a_basename_only_project() {
        // (c) A `project` basename guess with no `cwd` match applies no
        // penalty at all -- only a `cwd` resolved via `project_for_path` is
        // an actual identity claim.
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let emb = unit_vec(4, 0);
        store::upsert_project(&conn, "git:alpha", "alpha", "/tmp/alpha", &now).unwrap();
        store::upsert_project(&conn, "git:beta", "beta", "/tmp/beta", &now).unwrap();
        store::insert(&conn, "alpha project fact about widgets", None, Some("alpha"), true, Some(&emb), 5).unwrap();
        store::insert(&conn, "beta project fact about widgets", None, Some("beta"), true, Some(&emb), 5).unwrap();
        let embedder = FixedVecEmbedder(emb);

        // `project` given, no `cwd` at all.
        let resp = search_hits(&conn, &embedder, "widgets", 5, false, false, 0.0, &now, Some("alpha"), None, None).unwrap();
        assert_eq!(resp.hits.len(), 2);
        assert!(
            (resp.hits[0].score - resp.hits[1].score).abs() < 0.0001,
            "no cwd match means no penalty, even with a project basename guess"
        );

        // `cwd` given but it doesn't resolve to any registered project --
        // still falls back to the basename guess, still no penalty.
        let resp2 = search_hits(
            &conn,
            &embedder,
            "widgets",
            5,
            false,
            false,
            0.0,
            &now,
            Some("alpha"),
            Some("/tmp/unregistered-dir"),
            None,
        )
        .unwrap();
        assert_eq!(resp2.hits.len(), 2);
        assert!(
            (resp2.hits[0].score - resp2.hits[1].score).abs() < 0.0001,
            "cwd with no registry match must not apply a penalty either"
        );
    }

    #[test]
    fn search_hits_without_project_is_unchanged() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let emb = unit_vec(4, 0);
        store::insert(&conn, "alpha project fact about widgets", None, Some("alpha"), true, Some(&emb), 5).unwrap();
        store::insert(&conn, "beta project fact about widgets", None, Some("beta"), true, Some(&emb), 5).unwrap();
        let embedder = FixedVecEmbedder(emb);

        // No session project known (neither `project` nor `cwd` given) --
        // no memory is down-weighted, so equally relevant hits keep equal
        // scores exactly as before this penalty existed.
        let resp = search_hits(&conn, &embedder, "widgets", 5, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.hits.len(), 2);
        assert!((resp.hits[0].score - resp.hits[1].score).abs() < 0.0001);
    }

    #[test]
    fn pack_to_budget_drops_the_project_card_that_does_not_fit() {
        let resp = SearchResponse {
            hits: Vec::new(),
            connections: Vec::new(),
            cards: Vec::new(),
            project_card: Some("layout: fastapi-hub, react-app".to_string()),
        };
        let cost = est_tokens("layout: fastapi-hub, react-app");
        assert!(cost > 1, "fixture text must actually cost something to make the budget bite");
        let packed = pack_to_budget(resp, cost - 1);
        assert!(packed.project_card.is_none(), "a card that cannot fit must not be injected");
    }

    #[test]
    fn pack_to_budget_keeps_the_project_card_alongside_an_entity_card_when_both_fit() {
        let project_text = "layout: fastapi-hub, react-app".to_string();
        let entity_card = CardHit {
            entity: "Moses".to_string(),
            kind: Some("person".to_string()),
            text: "- argues hotfixes are underrated".to_string(),
            updated: "2026-01-01".to_string(),
            evidence: 1,
        };
        // Budget large enough that the card also clears its own quarter-
        // budget reservation cap (see PROJECT_CARD_BUDGET_NUMERATOR/
        // DENOMINATOR), not just the overall budget.
        let budget = est_tokens(&project_text) * PROJECT_CARD_BUDGET_DENOMINATOR / PROJECT_CARD_BUDGET_NUMERATOR
            + est_tokens(&entity_card.text)
            + 10;
        let resp = SearchResponse {
            hits: Vec::new(),
            connections: Vec::new(),
            cards: vec![entity_card],
            project_card: Some(project_text.clone()),
        };
        let packed = pack_to_budget(resp, budget);
        // Both charge against the same budget and both fit: the project
        // card being charged first (see `pack_to_budget`'s doc comment)
        // must not starve the entity card out when there's room for both.
        assert_eq!(packed.project_card, Some(project_text));
        assert_eq!(packed.cards.len(), 1);
        assert_eq!(packed.cards[0].entity, "Moses");
    }

    fn hit_with_content(id: i64, content: &str) -> SearchHit {
        SearchHit {
            id,
            content: content.to_string(),
            source: None,
            project: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            score: 1.0,
            sim: 1.0,
            recency: 1.0,
            strength: 1.0,
            importance: 0,
            superseded: false,
            derived: false,
            confidence: None,
            level: None,
            via_assoc: None,
            via_edge: None,
            basis: None,
            lexical: 0.0,
        }
    }

    #[test]
    fn pack_to_budget_never_lets_an_oversized_project_card_evict_hits_that_fit() {
        let budget = 400;
        // 600 chars -> 150 tokens, over budget/4 (100) though under the
        // whole budget -- the case that used to evict a ranked hit.
        let big_card = "x".repeat(600);
        assert!(est_tokens(&big_card) > budget * PROJECT_CARD_BUDGET_NUMERATOR / PROJECT_CARD_BUDGET_DENOMINATOR);
        // Three hits at 100 tokens each (400 chars) -- 300 total, well
        // within the 400 budget on their own.
        let hits = vec![
            hit_with_content(1, &"a".repeat(400)),
            hit_with_content(2, &"b".repeat(400)),
            hit_with_content(3, &"c".repeat(400)),
        ];
        let resp = SearchResponse { hits, connections: Vec::new(), cards: Vec::new(), project_card: Some(big_card) };
        let packed = pack_to_budget(resp, budget);
        assert!(packed.project_card.is_none(), "a card over a quarter of the budget must not be kept");
        assert_eq!(packed.hits.len(), 3, "all three hits must survive when the oversized card is dropped");
    }

    #[test]
    fn pack_to_budget_keeps_a_small_project_card_alongside_hits_that_all_fit() {
        let budget = 400;
        // 160 chars -> 40 tokens, well under budget/4 (100).
        let small_card = "x".repeat(160);
        assert!(est_tokens(&small_card) <= budget * PROJECT_CARD_BUDGET_NUMERATOR / PROJECT_CARD_BUDGET_DENOMINATOR);
        let hits = vec![
            hit_with_content(1, &"a".repeat(400)),
            hit_with_content(2, &"b".repeat(400)),
            hit_with_content(3, &"c".repeat(400)),
        ];
        let resp = SearchResponse { hits, connections: Vec::new(), cards: Vec::new(), project_card: Some(small_card.clone()) };
        let packed = pack_to_budget(resp, budget);
        assert_eq!(packed.project_card, Some(small_card));
        assert_eq!(packed.hits.len(), 3, "a small card must not crowd out any of the three hits");
    }

    /// Records one `mach kb improve` outcome memory the same way
    /// `record_improve_outcome` does, so `check_improve` tests exercise the
    /// real memory shape (source prefix, project, content) rather than a
    /// hand-rolled approximation.
    fn insert_improve_outcome(conn: &Connection, outcome: &improve::Outcome, now: &str) -> i64 {
        store::insert(conn, &outcome.memory_text(), Some(&outcome.memory_source(now)), Some(improve::OUTCOME_PROJECT), true, None, improve::OUTCOME_IMPORTANCE).unwrap()
    }

    #[test]
    fn check_improve_names_uncommitted_targets_since_the_streak_started() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::mark_improve_completed(&conn, &now).unwrap();
        let dirty1 = improve::uncommitted_targets_reason(&[PathBuf::from("/home/u/.claude/skills")]);
        let dirty2 = improve::uncommitted_targets_reason(&[PathBuf::from("/home/u/.claude/skills"), PathBuf::from("/home/u/.claude/CLAUDE.md")]);
        let first = insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: dirty1 }, "2026-09-20T08:00:00Z");
        insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: dirty2.clone() }, "2026-09-21T09:00:00Z");
        insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: dirty2 }, "2026-09-22T10:00:00Z");
        // `store::insert`'s `created_at` is the real wall clock, not the
        // `now` passed to `memory_source` (that only tags `source`) -- read
        // the oldest outcome's actual timestamp back rather than assume it.
        let first_created_at = store::get(&conn, first).unwrap().unwrap().created_at;

        let check = check_improve(&Ok(conn));
        assert!(!check.ok, "three consecutive failures must still fail the check");
        assert_eq!(
            check.detail,
            format!("blocked since {} — uncommitted: /home/u/.claude/skills, /home/u/.claude/CLAUDE.md", first_created_at)
        );

        // Prove this actually reaches `mach kb health`'s printed report --
        // `check_improve` is one of `cmd_health`'s assembled checks, and
        // `health::format_report` is exactly what `cmd_health` prints.
        let report = health::format_report(&[check]);
        assert_eq!(
            report,
            format!("[FAIL] improve: blocked since {} — uncommitted: /home/u/.claude/skills, /home/u/.claude/CLAUDE.md\n", first_created_at)
        );
    }

    #[test]
    fn check_improve_names_pre_run_drift_targets_since_the_streak_started() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::mark_improve_completed(&conn, &now).unwrap();
        let drift1 = improve::pre_run_drift_reason(&[PathBuf::from("/home/u/.claude/CLAUDE.md")]);
        let drift2 = improve::pre_run_drift_reason(&[PathBuf::from("/home/u/.claude/CLAUDE.md"), PathBuf::from("/home/u/.claude/settings.json")]);
        let first = insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: drift1 }, "2026-09-20T08:00:00Z");
        insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: drift2.clone() }, "2026-09-21T09:00:00Z");
        insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: drift2 }, "2026-09-22T10:00:00Z");
        let first_created_at = store::get(&conn, first).unwrap().unwrap().created_at;

        let check = check_improve(&Ok(conn));
        assert!(!check.ok, "three consecutive failures must still fail the check");
        assert_eq!(
            check.detail,
            format!(
                "blocked since {} — targets differ from chezmoi source: /home/u/.claude/CLAUDE.md, /home/u/.claude/settings.json",
                first_created_at
            )
        );

        let report = health::format_report(&[check]);
        assert_eq!(
            report,
            format!(
                "[FAIL] improve: blocked since {} — targets differ from chezmoi source: /home/u/.claude/CLAUDE.md, /home/u/.claude/settings.json\n",
                first_created_at
            )
        );
    }

    #[test]
    fn check_improve_keeps_the_generic_message_for_a_non_uncommitted_streak() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::mark_improve_completed(&conn, &now).unwrap();
        for _ in 0..3 {
            insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: "claude: timed out".into() }, &now);
        }
        let check = check_improve(&Ok(conn));
        assert!(!check.ok);
        assert!(check.detail.contains("consecutive failures since"), "{}", check.detail);
        assert!(!check.detail.contains("uncommitted"), "{}", check.detail);
    }

    #[test]
    fn check_improve_ignores_a_single_uncommitted_failure_under_the_streak_threshold() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::mark_improve_completed(&conn, &now).unwrap();
        insert_improve_outcome(&conn, &improve::Outcome::Applied { result: improve::ImproveResult { action: improve::Action::None, files: vec![], rationale: "r".into(), evidence: "none".into() }, sha: "abc".into() }, &now);
        insert_improve_outcome(&conn, &improve::Outcome::Failed { reason: improve::uncommitted_targets_reason(&[PathBuf::from("/home/u/.claude/CLAUDE.md")]) }, &now);
        let check = check_improve(&Ok(conn));
        assert!(check.ok, "one failure after a success is not a streak: {}", check.detail);
        assert!(check.detail.starts_with("last completed"), "{}", check.detail);
    }

    #[test]
    fn check_projects_names_the_forget_escape_hatch_when_a_root_is_missing() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::upsert_project(&conn, "path:/tmp/does-not-exist-mach-kb-test", "vanished", "/tmp/does-not-exist-mach-kb-test", &now).unwrap();
        let check = check_projects(&Ok(conn));
        assert!(!check.ok, "a missing root must still fail the check");
        assert!(
            check.detail.contains("mach kb projects forget"),
            "the health output must name the escape hatch that clears a permanently-missing project: {}",
            check.detail
        );
    }

    #[test]
    fn run_graph_extraction_pass_extracts_edges_resolves_entities_and_marks_extracted() {
        let conn = mem_conn();
        let id = store::insert(&conn, "the user works on a project called Umoja", None, None, true, None, 5).unwrap();
        let llm = FixedReflectLlm { reply: Ok("1: user | | works-on | Umoja | project | 0.9") };

        let (examined, edges, entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, 1);
        assert_eq!(edges, 1);
        assert_eq!(entities, 2, "both \"user\" and \"Umoja\" are newly created");
        assert!(!failed);

        let mem = store::get(&conn, id).unwrap().unwrap();
        assert!(mem.graph_extracted_at.is_some());

        let user = store::find_entity_by_name(&conn, "user").unwrap().unwrap();
        let edges_for_user = store::active_relations_for_entity(&conn, user.id).unwrap();
        assert_eq!(edges_for_user.len(), 1);
        assert_eq!(edges_for_user[0].predicate, "works-on");
        assert_eq!(edges_for_user[0].evidence_memory_id, Some(id));
    }

    #[test]
    fn run_graph_extraction_pass_none_reply_still_marks_the_memory_extracted() {
        let conn = mem_conn();
        let id = store::insert(&conn, "a fact with no stated relation", None, None, true, None, 5).unwrap();
        let llm = FixedReflectLlm { reply: Ok("1: NONE") };

        let (examined, edges, entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, 1);
        assert_eq!(edges, 0);
        assert_eq!(entities, 0);
        assert!(!failed);
        assert!(store::get(&conn, id).unwrap().unwrap().graph_extracted_at.is_some());
    }

    #[test]
    fn run_graph_extraction_pass_failed_call_never_marks_extracted() {
        let conn = mem_conn();
        let id = store::insert(&conn, "a fact", None, None, true, None, 5).unwrap();
        let llm = FixedReflectLlm { reply: Err("offline") };

        let (examined, edges, entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, 0);
        assert_eq!(edges, 0);
        assert_eq!(entities, 0);
        assert!(failed);
        assert!(
            store::get(&conn, id).unwrap().unwrap().graph_extracted_at.is_none(),
            "a failed call must never mark the memory extracted -- it must be retried next run"
        );
    }

    #[test]
    fn run_graph_extraction_pass_penalizes_confidence_for_an_unreviewed_source() {
        let conn = mem_conn();
        store::insert(&conn, "an unreviewed fact about tea", None, None, false, None, 5).unwrap();
        let llm = FixedReflectLlm { reply: Ok("1: user | | prefers | tea | | 1.0") };

        run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        let user = store::find_entity_by_name(&conn, "user").unwrap().unwrap();
        let edge = &store::active_relations_for_entity(&conn, user.id).unwrap()[0];
        assert!(
            (edge.confidence.unwrap() - reflect::GRAPH_EXTRACTION_UNREVIEWED_PENALTY).abs() < 1e-9,
            "confidence 1.0 * the unreviewed penalty, got {:?}",
            edge.confidence
        );
    }

    // --- graph extraction: batching ---

    #[test]
    fn run_graph_extraction_pass_batches_multiple_memories_into_one_call() {
        struct CountingLlm {
            reply: &'static str,
            calls: std::cell::Cell<usize>,
        }
        impl ReflectLlm for CountingLlm {
            fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                self.calls.set(self.calls.get() + 1);
                Ok(self.reply.to_string())
            }
        }

        let conn = mem_conn();
        for i in 0..3 {
            store::insert(&conn, &format!("fact number {}", i), None, None, true, None, 5).unwrap();
        }
        let llm = CountingLlm { reply: "1: NONE\n2: NONE\n3: NONE", calls: std::cell::Cell::new(0) };
        let (examined, edges, entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, 3);
        assert_eq!(edges, 0);
        assert_eq!(entities, 0);
        assert!(!failed);
        assert_eq!(llm.calls.get(), 1, "three memories well under the batch size must cost exactly one call");
    }

    #[test]
    fn run_graph_extraction_pass_splits_into_multiple_batches_over_the_batch_size() {
        struct CountingLlm {
            calls: std::cell::Cell<usize>,
        }
        impl ReflectLlm for CountingLlm {
            fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                self.calls.set(self.calls.get() + 1);
                // Generously covers the largest possible batch -- entries
                // beyond a smaller chunk's own fact count are simply
                // out-of-range and ignored by `parse_batch_extraction`.
                let mut out = String::new();
                for i in 1..=reflect::GRAPH_EXTRACTION_BATCH_SIZE {
                    out.push_str(&format!("{}: NONE\n", i));
                }
                Ok(out)
            }
        }

        let conn = mem_conn();
        let total = reflect::GRAPH_EXTRACTION_BATCH_SIZE + 5;
        for i in 0..total {
            store::insert(&conn, &format!("fact number {}", i), None, None, true, None, 5).unwrap();
        }
        let llm = CountingLlm { calls: std::cell::Cell::new(0) };
        let (examined, _edges, _entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, total);
        assert!(!failed);
        assert_eq!(llm.calls.get(), 2, "batch_size + 5 candidates at a batch size of batch_size must take exactly 2 calls");
    }

    #[test]
    fn run_graph_extraction_pass_partial_batch_reply_only_marks_addressed_facts_extracted() {
        let conn = mem_conn();
        let id1 = store::insert(&conn, "fact one", None, None, true, None, 5).unwrap();
        let id2 = store::insert(&conn, "fact two", None, None, true, None, 5).unwrap();
        // The reply never mentions fact 2 at all -- a truncated/malformed
        // batch reply, not a legitimate NONE for it.
        let llm = FixedReflectLlm { reply: Ok("1: NONE") };
        let (examined, _edges, _entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert_eq!(examined, 1);
        assert!(!failed, "the call itself succeeded -- only part of its reply was usable");
        assert!(store::get(&conn, id1).unwrap().unwrap().graph_extracted_at.is_some());
        assert!(
            store::get(&conn, id2).unwrap().unwrap().graph_extracted_at.is_none(),
            "an unaddressed fact must stay retry-able next run, never silently treated as NONE"
        );
    }

    /// The other half of the truncation rule: when the reply DOES address
    /// the last fact it cannot have been cut short, so a fact it skipped
    /// carries no extractable relation and must stop being retried. Without
    /// this the skipped rows became a permanent backlog that starved every
    /// newly added memory of extraction.
    #[test]
    fn a_complete_reply_that_skips_a_fact_marks_it_extracted_anyway() {
        let conn = mem_conn();
        let id1 = store::insert(&conn, "fact one", None, None, true, None, 5).unwrap();
        let id2 = store::insert(&conn, "fact two", None, None, true, None, 5).unwrap();
        let id3 = store::insert(&conn, "fact three", None, None, true, None, 5).unwrap();
        // Facts 1 and 3 answered, fact 2 silently skipped. Fact 3 is the
        // last, so the reply is complete and fact 2 was a real omission.
        let llm = FixedReflectLlm { reply: Ok("1: NONE\n3: NONE") };
        let (_examined, _edges, _entities, failed) =
            run_graph_extraction_pass(&conn, &FakeEmbedder, &llm, &store::now_rfc3339()).unwrap();
        assert!(!failed);
        for id in [id1, id2, id3] {
            assert!(
                store::get(&conn, id).unwrap().unwrap().graph_extracted_at.is_some(),
                "memory {} should not be retried forever",
                id
            );
        }
        assert!(store::graph_extraction_candidates(&conn, 10).unwrap().is_empty(), "backlog drained");
    }

    // --- reflect_working_set: oldest/newest backlog split (Task 2) ---

    fn insert_n(conn: &Connection, n: usize, tag: &str) -> Vec<i64> {
        (0..n).map(|i| store::insert(conn, &format!("{} {}", tag, i), None, None, true, None, 5).unwrap()).collect()
    }

    #[test]
    fn reflect_working_set_splits_100_unreflected_into_40_oldest_and_20_newest() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 100, "backlog row");

        let working = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        assert_eq!(working.len(), 60);

        let got: Vec<i64> = working.iter().map(|m| m.id).collect();
        let mut expected: Vec<i64> = ids[0..40].to_vec(); // 40 oldest
        expected.extend(&ids[80..100]); // 20 newest
        expected.sort_unstable();
        assert_eq!(got, expected, "deterministic: oldest 40 + newest 20, sorted ascending");
    }

    #[test]
    fn reflect_working_set_with_10_unreflected_returns_all_10() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 10, "small backlog");

        let working = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        let got: Vec<i64> = working.iter().map(|m| m.id).collect();
        let mut expected = ids.clone();
        expected.sort_unstable();
        assert_eq!(got, expected, "fewer than the cap -- everything comes back, no duplicates");
    }

    #[test]
    fn reflect_working_set_excludes_index_owned_memories_even_if_unreflected() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 5, "ordinary");
        // `upsert_index_memory` always sets `reflected_at` at insert time,
        // so this row wouldn't reach `unreflected_active_newest`/`_oldest`
        // in the first place -- clearing it back to NULL here simulates the
        // edge case the source-prefix filter guards against (a pre-phase-2
        // row, a bug, a manual edit) rather than relying solely on
        // `reflected_at` staying set forever.
        let index_id =
            store::upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking", None, &store::now_rfc3339())
                .unwrap();
        conn.execute("UPDATE memories SET reflected_at = NULL WHERE id = ?1", rusqlite::params![index_id]).unwrap();

        let working = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        let got: Vec<i64> = working.iter().map(|m| m.id).collect();
        assert!(!got.contains(&index_id), "an index-owned memory must never enter the insight-stage working set");
        assert_eq!(got.len(), ids.len(), "the ordinary rows must still all be present");
    }

    #[test]
    fn reflect_working_set_excludes_code_history_owned_memories_too() {
        // Task-3 extension of the test above.
        let conn = mem_conn();
        let ids = insert_n(&conn, 5, "ordinary");
        let history_id = store::upsert_index_memory(
            &conn,
            "helios",
            "code-history:helios:2026-08",
            "helios 2026-08: aug work",
            None,
            &store::now_rfc3339(),
        )
        .unwrap();
        conn.execute("UPDATE memories SET reflected_at = NULL WHERE id = ?1", rusqlite::params![history_id]).unwrap();

        let working = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        let got: Vec<i64> = working.iter().map(|m| m.id).collect();
        assert!(!got.contains(&history_id), "a history-owned memory must never enter the insight-stage working set");
        assert_eq!(got.len(), ids.len(), "the ordinary rows must still all be present");
    }

    #[test]
    fn reflect_working_set_moves_forward_after_marking_the_previous_run_reflected() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 100, "moving backlog");

        let first = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        let first_ids: Vec<i64> = first.iter().map(|m| m.id).collect();
        store::mark_memories_reflected(&conn, &first_ids, &store::now_rfc3339()).unwrap();

        // 40 remain (ids 40..80, the untouched middle), all under the cap.
        assert_eq!(store::unreflected_active_count(&conn).unwrap(), 40);
        let second = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        let second_ids: Vec<i64> = second.iter().map(|m| m.id).collect();
        assert_eq!(second_ids.len(), 40);
        assert!(
            second_ids.iter().all(|id| !first_ids.contains(id)),
            "the second run's set must not repeat anything the first run already reflected"
        );
        let mut expected_middle: Vec<i64> = ids[40..80].to_vec();
        expected_middle.sort_unstable();
        assert_eq!(second_ids, expected_middle);
    }

    // --- run_insight_stage: failure isolation (Task 1 + fix round 1) ---

    /// A `ReflectLlm` test double that replays a fixed sequence of replies,
    /// one per call, in order -- for tests that need to distinguish
    /// stage-1's call from stage-2's from re-verification's (all "haiku" or
    /// "sonnet" by model name alone, which isn't enough to tell them apart
    /// -- `run_insight_stage` calls them in a fixed order: stage-1, then
    /// stage-2 per question, then re-verification per due insight). Panics
    /// if called more times than it has scripted replies, so a test asserts
    /// its own assumption about exactly how many calls should happen.
    struct ScriptedSequenceLlm {
        replies: std::cell::RefCell<std::collections::VecDeque<Result<&'static str, &'static str>>>,
    }

    impl ScriptedSequenceLlm {
        fn new(replies: Vec<Result<&'static str, &'static str>>) -> Self {
            ScriptedSequenceLlm { replies: std::cell::RefCell::new(replies.into_iter().collect()) }
        }
    }

    impl ReflectLlm for ScriptedSequenceLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            match self.replies.borrow_mut().pop_front() {
                Some(Ok(s)) => Ok(s.to_string()),
                Some(Err(e)) => Err(e.to_string()),
                None => panic!("ScriptedSequenceLlm: called more times than it has scripted replies"),
            }
        }
    }

    /// Inserts `n` memories all sharing one simple embedding, so
    /// `store::top_similar_active` reliably returns them together as
    /// mutual evidence (stage-2's `evidence.len() >= 2` floor, and
    /// re-verification's own candidate search) -- unlike `insert_n`, whose
    /// rows have no embedding at all and so never satisfy either.
    fn insert_n_with_shared_embedding(conn: &Connection, n: usize, tag: &str) -> Vec<i64> {
        let emb = unit_vec(4, 0);
        (0..n)
            .map(|i| store::insert(conn, &format!("{} {}", tag, i), None, None, true, Some(&emb), 5).unwrap())
            .collect()
    }

    #[test]
    fn run_insight_stage_marks_the_working_set_reflected_when_the_stage_succeeds() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 5, "insight stage success");
        let llm = FixedReflectLlm { reply: Ok("what does the user prefer?") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, &[], &now).unwrap();

        assert_eq!(outcome.examined, 5);
        assert!(!outcome.stage_failed);
        assert!(!outcome.any_failed);
        assert_eq!(outcome.reflected, 5);
        assert_eq!(outcome.max_reflected_id, ids.iter().max().copied());
        assert_eq!(store::unreflected_active_count(&conn).unwrap(), 0, "the whole working set was marked reflected");
    }

    #[test]
    fn run_insight_stage_marks_nothing_when_stage_1_itself_fails() {
        let conn = mem_conn();
        let n_before = 5;
        insert_n(&conn, n_before, "insight stage failure");
        // Stage 1's own haiku call fails outright.
        let llm = FixedReflectLlm { reply: Err("'claude' exited with Some(1) (no stderr)") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, &[], &now).unwrap();

        assert_eq!(outcome.examined, 5, "the working set was still selected and examined");
        assert!(outcome.stage_failed);
        assert!(outcome.any_failed);
        assert_eq!(outcome.reflected, 0, "a failure in stage 1 itself marks nothing");
        assert_eq!(outcome.max_reflected_id, None);
        assert_eq!(
            store::unreflected_active_count(&conn).unwrap(),
            n_before as i64,
            "every row in the working set stays due for the next run"
        );
    }

    #[test]
    fn run_insight_stage_marks_nothing_when_stage_2_fails_even_though_stage_1_succeeded() {
        let conn = mem_conn();
        insert_n_with_shared_embedding(&conn, 5, "stage 2 failure");
        // Stage 1 succeeds with one question; stage 2's own sonnet call
        // then fails for it.
        let llm = ScriptedSequenceLlm::new(vec![Ok("what pattern holds here?"), Err("'claude' exited with Some(1)")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, &[], &now).unwrap();

        assert_eq!(outcome.questions, 1, "stage 1 succeeded and produced one question");
        assert!(outcome.stage_failed, "stage 2's own call failure must still gate reflected_at");
        assert!(outcome.any_failed);
        assert_eq!(outcome.reflected, 0, "nothing marked -- stage 2 failed");
    }

    #[test]
    fn run_insight_stage_marks_nothing_when_the_embedder_fails_in_stage_2() {
        let conn = mem_conn();
        insert_n(&conn, 5, "embedder failure in stage 2");
        // Stage 1 succeeds; the embedder then fails for the question stage
        // 2 needs embedded before it can even look for evidence -- this is
        // a failure of this stage's own machinery, not "insufficient
        // evidence", and must gate reflected_at exactly like a failed
        // `claude` call does.
        struct AlwaysFailingEmbedder;
        impl Embedder for AlwaysFailingEmbedder {
            fn embed(&self, _text: &str) -> Result<Vec<f32>, KbError> {
                Err(KbError::Other("ollama unreachable".to_string()))
            }
        }
        let llm = FixedReflectLlm { reply: Ok("what does the evidence show?") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &AlwaysFailingEmbedder, &llm, &[], &now).unwrap();

        assert_eq!(outcome.questions, 1);
        assert!(outcome.stage_failed, "an embedding-provider error in stage 2 must set stage_failed");
        assert!(outcome.any_failed);
        assert_eq!(outcome.reflected, 0, "nothing marked -- the embedder failed");
    }

    #[test]
    fn run_insight_stage_drops_a_question_with_fewer_than_2_evidence_rows_without_failing() {
        // The `evidence.len() < 2` floor is a legitimate outcome (the
        // query succeeded and just found too little), never a failure --
        // must NOT set stage_failed, unlike the embedder/evidence-query
        // error cases above. With no embeddings on any memory,
        // `top_similar_active` returns nothing for the question, so the
        // floor is never cleared and the sonnet call is never even placed
        // -- if this incorrectly set stage_failed, reflected would be 0.
        let conn = mem_conn();
        insert_n(&conn, 5, "too little evidence");
        let llm = FixedReflectLlm { reply: Ok("a question nothing backs") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, &[], &now).unwrap();

        assert!(!outcome.stage_failed, "insufficient evidence is not a failure");
        assert!(!outcome.any_failed);
        assert_eq!(outcome.reflected, 5, "the working set is still marked reflected");
    }

    #[test]
    fn run_insight_stage_marks_the_60_rows_even_when_a_verification_call_fails() {
        // The exact scenario the fix-round controller ruling names: stage
        // 1 and stage 2 both succeed; a re-verification call fails. The
        // working set must still be marked reflected in full, and the
        // verification failure must surface only via `any_failed`.
        let conn = mem_conn();
        let ids = insert_n_with_shared_embedding(&conn, 60, "verification failure isolation");
        let cite_a = ids[0].to_string();
        let cite_b = ids[1].to_string();
        let due_insight_id =
            store::insert_insight(&conn, "a belief due for re-verification", 0.6, &[cite_a, cite_b], Some(&unit_vec(4, 0)))
                .unwrap();
        let due = store::get_insight(&conn, due_insight_id).unwrap().unwrap();

        // Call order: stage-1 (haiku, one question) -> stage-2 (sonnet,
        // "NONE" -- a successful call that simply declines to synthesize
        // anything, which is fine; only the CALL failing would matter) ->
        // re-verification (haiku, fails).
        let llm = ScriptedSequenceLlm::new(vec![
            Ok("what does this working set suggest?"),
            Ok("NONE"),
            Err("'claude' exited with Some(1) (no stderr)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.examined, 60);
        assert!(!outcome.stage_failed, "stage 1 and stage 2 both succeeded");
        assert!(outcome.any_failed, "the verification failure still shows up here, for reporting");
        assert_eq!(outcome.reflected, 60, "the 60 rows ARE marked despite the verification failure");
        assert_eq!(store::unreflected_active_count(&conn).unwrap(), 0);
        // And the insight itself is untouched -- still due, stays due.
        let after = store::get_insight(&conn, due_insight_id).unwrap().unwrap();
        assert!(!after.is_flagged());
        assert_eq!(after.last_verified_at, due.last_verified_at, "a failed check must not be credited");
    }

    #[test]
    fn run_insight_stage_with_an_empty_backlog_still_runs_verification_and_marks_nothing() {
        let conn = mem_conn();
        // No memory backlog at all -- only a stale insight due for
        // re-verification. should_mark_reflected must see "no working set"
        // and mark nothing, regardless of whether verification succeeds.
        let ins_id =
            store::insert_insight(&conn, "a durable belief", 0.6, &["1".to_string(), "2".to_string()], None).unwrap();
        let stale = store::get_insight(&conn, ins_id).unwrap().unwrap();
        let llm = FixedReflectLlm { reply: Ok("NONE") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&stale), &now).unwrap();

        assert_eq!(outcome.examined, 0);
        assert_eq!(outcome.reflected, 0);
        assert_eq!(outcome.max_reflected_id, None);
    }

    // --- run_insight_stage: verification revises insight text (Task 3) ---

    #[test]
    fn run_insight_stage_revises_a_contradicted_insight_when_the_judge_says_revise() {
        let conn = mem_conn();
        // Real, active citations -- otherwise the broken-citation check
        // (weaken, not contradiction) fires first and this never reaches
        // the contradiction/revise machinery at all.
        let m1 = store::insert(&conn, "shipped mid-week this sprint", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "shipped again mid-week", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        assert_eq!((m1, m2), (1, 2), "sanity: fresh :memory: db, ids are deterministic");
        // Keep the memory working set empty so only re-verification's own
        // calls consume the scripted LLM replies below.
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "the user always ships on Fridays",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        // No memory backlog for the insight stage itself (empty working
        // set) -- so the only two calls made are re-verification's own:
        // the contradiction check (haiku) -> "YES 1", then the revise/
        // drop/keep judge (sonnet) -> "REVISE: ..." citing both evidence
        // rows (m1=1, m2=2) the contradiction check itself was shown.
        let llm = ScriptedSequenceLlm::new(vec![
            Ok("YES 1"),
            Ok("REVISE: the user ships mid-week, not on Fridays (because of: 1, 2)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.revised, 1);
        assert_eq!(outcome.flagged, 0);
        assert_eq!(outcome.verified, 0);
        assert!(!outcome.any_failed);

        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert_eq!(after.text, "the user ships mid-week, not on Fridays");
        assert_eq!(after.prev_text.as_deref(), Some("the user always ships on Fridays"));
        assert_eq!(after.revised_at.as_deref(), Some(now.as_str()));
        assert!(!after.is_flagged(), "a REVISE verdict must clear any prior flag");
        assert_eq!(
            after.confidence,
            0.6 - store::INSIGHT_CONFIDENCE_STEP,
            "a REVISE lowers confidence by one step (fix-list item 3)"
        );
        assert_eq!(
            after.source_ids,
            vec![m1.to_string(), m2.to_string()],
            "both cited ids were already source_ids -- deduped, not duplicated"
        );
    }

    #[test]
    fn run_insight_stage_flags_a_contradicted_insight_when_the_judge_says_drop() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "no longer true", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "also no longer true", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "a belief the evidence no longer supports at all",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        let llm = ScriptedSequenceLlm::new(vec![Ok("YES 1"), Ok("DROP")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1);
        assert_eq!(outcome.revised, 0);
        assert!(!outcome.any_failed);
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(after.is_flagged());
        assert!(after.revised_at.is_none());
        assert_eq!(after.text, "a belief the evidence no longer supports at all", "DROP must never touch the text");
    }

    #[test]
    fn run_insight_stage_verifies_a_contradicted_insight_when_the_judge_says_keep() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "ambiguous evidence", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "more ambiguous evidence", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "a belief the second judge says still holds",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        // The first (haiku) judge flags a contradiction; the second
        // (sonnet) revise/drop/keep judge disagrees -- treated as a clean
        // verification, not a flag.
        let llm = ScriptedSequenceLlm::new(vec![Ok("YES 1"), Ok("KEEP")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.verified, 1);
        assert_eq!(outcome.flagged, 0);
        assert_eq!(outcome.revised, 0);
        assert!(!outcome.any_failed);
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(!after.is_flagged());
        assert_eq!(after.text, "a belief the second judge says still holds");
    }

    #[test]
    fn run_insight_stage_flags_on_an_unparseable_revise_reply_never_revising() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "some fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "another fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "a belief whose judge reply is garbage",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        let llm = ScriptedSequenceLlm::new(vec![Ok("YES 1"), Ok("uh, not sure?")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1, "an unparseable reply falls back to the pre-existing flag behavior");
        assert_eq!(outcome.revised, 0);
        assert!(outcome.any_failed, "a bad reply is still visible in the degraded-run report");
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(after.is_flagged());
        assert_eq!(after.text, "a belief whose judge reply is garbage", "a parse failure must never revise the text");
    }

    #[test]
    fn run_insight_stage_flags_when_the_revise_call_itself_fails() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "some fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "another fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "a belief whose revise call fails outright",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        let llm = ScriptedSequenceLlm::new(vec![Ok("YES 1"), Err("'claude' exited with Some(1) (no stderr)")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1, "the contradiction is already established -- a failed second call falls back to flagging");
        assert_eq!(outcome.revised, 0);
        assert!(outcome.any_failed);
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(after.is_flagged());
    }

    // --- run_insight_stage: final-fix-list items 1-3, 5 ---

    #[test]
    fn run_insight_stage_applies_revise_as_drop_when_citations_are_insufficient() {
        // Mirrors reflect::parse_revise_without_two_valid_citations_is_applied_as_drop
        // through the real verification loop: a REVISE with no (or bad)
        // citations must be applied exactly like an explicit DROP -- flagged,
        // never revised, and NOT counted as a failure (it's a legitimate,
        // fully-parsed verdict that just failed the evidence floor).
        let conn = mem_conn();
        let m1 = store::insert(&conn, "shipped mid-week this sprint", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "shipped again mid-week", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "the user always ships on Fridays",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        // Only one cited id, and it isn't among the evidence rows shown --
        // fails the >= 2-among-evidence floor either way.
        let llm = ScriptedSequenceLlm::new(vec![
            Ok("YES 1"),
            Ok("REVISE: the user ships mid-week, not on Fridays (because of: 99)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1, "an under-cited REVISE is applied as DROP");
        assert_eq!(outcome.revised, 0);
        assert!(!outcome.any_failed, "an under-cited REVISE is a legitimate verdict, not a call/parse failure");
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(after.is_flagged());
        assert_eq!(after.text, "the user always ships on Fridays", "an applied-as-DROP verdict must never touch the text");
    }

    #[test]
    fn run_insight_stage_never_revises_a_level_2_theme() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "some fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "another fact", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let theme_id = store::insert_theme(
            &conn,
            "a theme the evidence has turned against",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, theme_id).unwrap().unwrap();
        assert_eq!(due.level, 2, "sanity: insert_theme must produce a level-2 row");

        // Only ONE scripted reply -- the contradiction check (haiku). If the
        // revise/drop/keep (sonnet) call were made for a theme,
        // ScriptedSequenceLlm would panic ("called more times than it has
        // scripted replies").
        let llm = ScriptedSequenceLlm::new(vec![Ok("YES 1")]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1, "a contradicted theme is flagged directly, never revised");
        assert_eq!(outcome.revised, 0);
        assert!(!outcome.any_failed, "the old flag path is not a failure");
        let after = store::get_insight(&conn, theme_id).unwrap().unwrap();
        assert!(after.is_flagged());
        assert_eq!(after.text, "a theme the evidence has turned against", "flagging must never touch the text");
    }

    #[test]
    fn run_insight_stage_applies_revise_as_drop_when_the_insight_was_revised_within_14_days() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "shipped mid-week this sprint", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "shipped again mid-week", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "the user ships on Tuesdays now",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        // A prior REVISE 5 days ago -- inside the 14-day flip-flop window.
        let five_days_ago = store::now_rfc3339_from_secs(store::now_secs() - 5 * 86400);
        store::revise_insight(
            &conn,
            ins_id,
            "the user ships on Tuesdays now",
            Some(&unit_vec(4, 0)),
            &[],
            &five_days_ago,
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(due.revised_at.is_some(), "sanity: the insight was already revised recently");

        let llm = ScriptedSequenceLlm::new(vec![
            Ok("YES 1"),
            Ok("REVISE: the user ships mid-week, not on Tuesdays (because of: 1, 2)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.flagged, 1, "a REVISE within 14 days of the last revision is applied as DROP");
        assert_eq!(outcome.revised, 0);
        assert!(!outcome.any_failed, "this is a deliberate policy application, not a failure");
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(after.is_flagged());
        assert_eq!(after.text, "the user ships on Tuesdays now", "a suppressed REVISE must never touch the text");
    }

    #[test]
    fn run_insight_stage_revises_normally_when_the_last_revision_was_over_14_days_ago() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "shipped mid-week this sprint", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "shipped again mid-week", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "the user ships on Tuesdays now",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        // A prior REVISE 15 days ago -- OUTSIDE the 14-day flip-flop window.
        let fifteen_days_ago = store::now_rfc3339_from_secs(store::now_secs() - 15 * 86400);
        store::revise_insight(
            &conn,
            ins_id,
            "the user ships on Tuesdays now",
            Some(&unit_vec(4, 0)),
            &[],
            &fifteen_days_ago,
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();
        let confidence_before = due.confidence;

        let llm = ScriptedSequenceLlm::new(vec![
            Ok("YES 1"),
            Ok("REVISE: the user ships mid-week, not on Tuesdays (because of: 1, 2)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.revised, 1, "past the 14-day window, REVISE proceeds normally");
        assert_eq!(outcome.flagged, 0);
        assert!(!outcome.any_failed);
        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert_eq!(after.text, "the user ships mid-week, not on Tuesdays");
        assert_eq!(after.confidence, confidence_before - store::INSIGHT_CONFIDENCE_STEP);
    }

    #[test]
    fn run_insight_stage_skips_the_revise_and_reports_any_failed_when_the_embedder_fails() {
        let conn = mem_conn();
        let m1 = store::insert(&conn, "shipped mid-week this sprint", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let m2 = store::insert(&conn, "shipped again mid-week", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        store::mark_memories_reflected(&conn, &[m1, m2], "1970-01-01T00:00:00Z").unwrap();
        let ins_id = store::insert_insight(
            &conn,
            "the user always ships on Fridays",
            0.6,
            &[m1.to_string(), m2.to_string()],
            Some(&unit_vec(4, 0)),
        )
        .unwrap();
        let due = store::get_insight(&conn, ins_id).unwrap().unwrap();

        // Succeeds embedding the insight's OWN text (needed for the
        // contradiction check's candidate search), fails only on the new
        // corrected text -- isolating the fix-list item 5 code path.
        struct FailsOnThisText(&'static str);
        impl Embedder for FailsOnThisText {
            fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
                if text == self.0 {
                    Err(KbError::Other("ollama unreachable".to_string()))
                } else {
                    Ok(text.bytes().map(|b| b as f32).collect())
                }
            }
        }
        let new_text = "the user ships mid-week, not on Fridays";
        let embedder = FailsOnThisText(new_text);

        let llm = ScriptedSequenceLlm::new(vec![
            Ok("YES 1"),
            Ok("REVISE: the user ships mid-week, not on Fridays (because of: 1, 2)"),
        ]);
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &embedder, &llm, std::slice::from_ref(&due), &now).unwrap();

        assert_eq!(outcome.revised, 0, "an embed failure must skip the revise entirely");
        assert_eq!(outcome.flagged, 0, "and must NOT fall back to flagging either -- left unchanged, due again");
        assert_eq!(outcome.verified, 0);
        assert!(outcome.any_failed, "the failure must still be visible in the degraded-run report");

        let after = store::get_insight(&conn, ins_id).unwrap().unwrap();
        assert!(!after.is_flagged());
        assert!(after.revised_at.is_none());
        assert_eq!(after.text, "the user always ships on Fridays", "text must be completely untouched");
        assert_eq!(after.confidence, 0.6, "confidence must be completely untouched too");
    }

    /// The behavioral counterpart to `reflect::should_mark_reflected`'s own
    /// pure-predicate tests: a pass this function never runs (graph
    /// extraction, entity cards, dedupe, ...) has no code path into
    /// `run_insight_stage` at all, so its failure literally cannot affect
    /// `outcome.stage_failed` -- this test documents that by driving the
    /// stage to success and confirming the working set is marked reflected
    /// exactly as it would be on a fully clean run, i.e. nothing about
    /// "some other pass elsewhere in this same `mach kb reflect`
    /// invocation is about to fail" changes this function's own behavior.
    #[test]
    fn run_insight_stage_succeeds_independently_of_any_other_pass() {
        let conn = mem_conn();
        insert_n(&conn, 3, "isolated from graph extraction's own failures");
        let llm = FixedReflectLlm { reply: Ok("what pattern holds here?") };
        let now = store::now_rfc3339();
        let outcome = run_insight_stage(&conn, &FakeEmbedder, &llm, &[], &now).unwrap();
        assert!(!outcome.stage_failed);
        assert!(!outcome.any_failed);
        assert_eq!(outcome.reflected, 3);
    }

    // --- cap_stage1_prompt: stage-1 prompt size cap (fix round 1, item 3) ---

    #[test]
    fn cap_stage1_prompt_shrinks_the_oldest_slice_to_fit_the_char_cap_keeping_all_newest_rows() {
        let conn = mem_conn();
        // 5 oldest rows, each large enough that all 5 together blow past a
        // small cap; 3 "newest" rows, small, always kept.
        let big = "x".repeat(2000);
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(store::insert(&conn, &format!("{} {}", big, i), None, None, true, None, 5).unwrap());
        }
        let mut newest_ids = Vec::new();
        for i in 0..3 {
            newest_ids.push(store::insert(&conn, &format!("small newest {}", i), None, None, true, None, 5).unwrap());
        }
        ids.extend(&newest_ids);

        let working_vec = reflect_working_set(&conn, 8, 3).unwrap();
        assert_eq!(working_vec.len(), 8, "sanity: nothing capped yet at the selection stage");

        let cap = 3_000usize; // small enough to force trimming
        let capped = cap_stage1_prompt(working_vec, 3, cap);

        let lines: Vec<(i64, String)> = capped.iter().map(|m| (m.id, m.content.clone())).collect();
        let prompt_len = reflect::build_questions_prompt(&lines).chars().count();
        assert!(prompt_len <= cap, "prompt is {} chars, over the {} cap", prompt_len, cap);

        let capped_ids: std::collections::HashSet<i64> = capped.iter().map(|m| m.id).collect();
        for id in &newest_ids {
            assert!(capped_ids.contains(id), "the newest slice must never be trimmed");
        }
        assert!(capped.len() < 8, "at least one oldest-slice row must have been dropped to fit");

        // The working set really is exactly the rows in the (now-fitting)
        // prompt -- nothing extra, nothing missing.
        assert_eq!(capped_ids.len(), capped.len());
    }

    #[test]
    fn cap_stage1_prompt_is_a_noop_when_it_already_fits() {
        let conn = mem_conn();
        let ids = insert_n(&conn, 10, "small enough to fit");
        let working_vec = reflect_working_set(&conn, 10, 5).unwrap();
        let capped = cap_stage1_prompt(working_vec, 5, REFLECT_STAGE1_PROMPT_CHAR_CAP);
        assert_eq!(capped.iter().map(|m| m.id).collect::<Vec<_>>(), ids);
    }

    #[test]
    fn cap_stage1_prompt_never_trims_when_everything_is_within_the_newest_slice() {
        let conn = mem_conn();
        let big = "y".repeat(50_000); // one row alone would blow the cap
        let id = store::insert(&conn, &big, None, None, true, None, 5).unwrap();
        let working_vec = reflect_working_set(&conn, 60, 20).unwrap();
        // The single row is entirely within the newest slice (1 <= 20), so
        // there is no oldest part to trim -- per the controller ruling,
        // the newest slice is never shrunk even if still oversized.
        let capped = cap_stage1_prompt(working_vec, 20, 100);
        assert_eq!(capped.iter().map(|m| m.id).collect::<Vec<_>>(), vec![id]);
    }

    /// The literal cap from the controller ruling (20,000 chars), exercised
    /// end to end through `reflect_working_set` + `cap_stage1_prompt`
    /// together, the same composition `run_insight_stage` itself uses.
    #[test]
    fn cap_stage1_prompt_enforces_the_real_20k_char_cap_on_an_oversized_oldest_slice() {
        let conn = mem_conn();
        let big = "z".repeat(600); // 40 oldest rows * ~600 chars each > 20k
        let mut oldest_ids = Vec::new();
        for i in 0..40 {
            oldest_ids.push(store::insert(&conn, &format!("{} {}", big, i), None, None, true, None, 5).unwrap());
        }
        let mut newest_ids = Vec::new();
        for i in 0..20 {
            newest_ids.push(store::insert(&conn, &format!("newest {}", i), None, None, true, None, 5).unwrap());
        }

        let working_vec = reflect_working_set(&conn, REFLECT_WORKING_SET_CAP, REFLECT_NEWEST_SLICE).unwrap();
        assert_eq!(working_vec.len(), 60);
        let full_prompt_len = {
            let lines: Vec<(i64, String)> = working_vec.iter().map(|m| (m.id, m.content.clone())).collect();
            reflect::build_questions_prompt(&lines).chars().count()
        };
        assert!(full_prompt_len > REFLECT_STAGE1_PROMPT_CHAR_CAP, "sanity: the uncapped prompt really is oversized");

        let capped = cap_stage1_prompt(working_vec, REFLECT_NEWEST_SLICE, REFLECT_STAGE1_PROMPT_CHAR_CAP);
        let lines: Vec<(i64, String)> = capped.iter().map(|m| (m.id, m.content.clone())).collect();
        assert!(reflect::build_questions_prompt(&lines).chars().count() <= REFLECT_STAGE1_PROMPT_CHAR_CAP);

        let capped_ids: std::collections::HashSet<i64> = capped.iter().map(|m| m.id).collect();
        for id in &newest_ids {
            assert!(capped_ids.contains(id), "all 20 newest rows must survive");
        }
        assert!(capped.len() < 60, "some oldest-slice rows were dropped to fit");
        let _ = oldest_ids;
    }

    // --- search enrichment: entity connections ---

    #[test]
    fn search_hits_surfaces_connections_for_a_closely_matching_entity() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let mem_id = store::insert(&conn, "Moses is the user's boss", None, None, true, None, 5).unwrap();
        store::insert_relation(&conn, moses, "boss-of", user, Some(mem_id), Some(0.9), &store::now_rfc3339()).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &store::now_rfc3339(), None, None, None)
            .unwrap();
        assert_eq!(resp.connections.len(), 1);
        assert_eq!(resp.connections[0].src_name, "Moses");
        assert_eq!(resp.connections[0].predicate, "boss-of");
        assert_eq!(resp.connections[0].dst_name, "user");
        assert_eq!(resp.connections[0].evidence_date.len(), 10);
    }

    #[test]
    fn search_hits_connections_empty_when_nothing_clears_the_threshold() {
        let conn = mem_conn();
        // An entity exists but its embedding is orthogonal to the query.
        store::insert_entity(&conn, "Moses", Some("person"), Some(&unit_vec(4, 1))).unwrap();
        let embedder = FixedVecEmbedder(unit_vec(4, 0));
        let resp =
            search_hits(&conn, &embedder, "anything", 10, false, false, 0.0, &store::now_rfc3339(), None, None, None).unwrap();
        assert!(resp.connections.is_empty());
    }

    // --- search enrichment: 2-hop spreading activation ---

    #[test]
    fn entity_connections_walks_a_second_hop_when_hop1_confidence_clears_the_floor() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let umoja = store::insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let now = store::now_rfc3339();
        let mem1 = store::insert(&conn, "Moses is the user's boss", None, None, true, None, 5).unwrap();
        store::insert_relation(&conn, moses, "boss-of", user, Some(mem1), Some(0.9), &now).unwrap();
        let mem2 = store::insert(&conn, "the user works on Umoja", None, None, true, None, 5).unwrap();
        store::insert_relation(&conn, user, "works-on", umoja, Some(mem2), Some(0.8), &now).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.connections.len(), 2, "one hop-1 edge plus one hop-2 chain through it");

        let hop1 = &resp.connections[0];
        assert_eq!(hop1.hops, 1);
        assert_eq!(hop1.src_name, "Moses");
        assert_eq!(hop1.dst_name, "user");
        assert!(hop1.predicate2.is_none());

        let hop2 = &resp.connections[1];
        assert_eq!(hop2.hops, 2);
        assert_eq!(hop2.src_name, "Moses");
        assert_eq!(hop2.predicate, "boss-of");
        assert_eq!(hop2.dst_name, "user");
        assert_eq!(hop2.predicate2.as_deref(), Some("works-on"));
        assert_eq!(hop2.src_name2.as_deref(), Some("user"));
        assert_eq!(hop2.dst_name2.as_deref(), Some("Umoja"));
    }

    #[test]
    fn search_hits_pulls_in_a_memory_through_a_shared_entity() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let q = unit_vec(4, 0);
        let direct = store::insert(&conn, "Moses wants TLE ownership", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let hop = store::insert(&conn, "Moses prefers pragmatic hotfixes", None, None, true, Some(&unit_vec(4, 1)), 5).unwrap();
        let embedder = FixedVecEmbedder(q);
        // min_score above what recency alone earns, so the orthogonal row is
        // NOT a direct hit and has to arrive over the entity link.
        let resp = search_hits(&conn, &embedder, "who wants ownership", 10, false, false, 0.3, &now, None, None, None).unwrap();
        let d = resp.hits.iter().find(|h| h.id == direct).expect("direct hit");
        let h = resp.hits.iter().find(|h| h.id == hop).expect("hop hit");
        assert!(d.via_assoc.is_none());
        assert_eq!(h.via_assoc, Some(direct));
        assert_eq!(h.via_edge.as_deref(), Some("entity:Moses"));
        assert!(h.score < d.score);
    }

    #[test]
    fn why_report_traces_a_memory_and_an_insight() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let old = store::insert(&conn, "TESTBOSS: Moses is the boss", None, None, true, None, 5).unwrap();
        let new = store::insert_with_basis(&conn, "TESTBOSS: Ivar is the boss now", Some("session-digest"), None, false, None, 5, Some("stated")).unwrap();
        conn.execute("UPDATE memories SET superseded_by = ?1, invalidated_at = ?2 WHERE id = ?3", rusqlite::params![new, now, old]).unwrap();
        let ins = store::insert_insight(&conn, "leadership changed hands", 0.8, &[new.to_string()], None).unwrap();
        let theme = store::insert_theme(&conn, "org churn", 0.6, &[ins.to_string()], None).unwrap();
        let a = store::insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let b = store::insert_entity(&conn, "user", None, None).unwrap();
        store::insert_relation(&conn, a, "boss-of", b, Some(new), Some(0.9), &now).unwrap();
        conn.execute("INSERT INTO ingested_sessions (session_id, ingested_at) VALUES ('sess-42', ?1)", rusqlite::params![now]).unwrap();

        let r = why_report(&conn, WhyTarget::Memory(new), None, None).unwrap();
        assert!(r.contains(&format!("memory #{}", new)), "{}", r);
        assert!(r.contains("you said this in a session"), "{}", r);
        assert!(r.contains("basis: stated"), "{}", r);
        assert!(r.contains(&format!("supersedes #{}", old)), "{}", r);
        assert!(r.contains("likely session"), "{}", r);
        assert!(r.contains("sess-42"), "{}", r);
        assert!(r.contains(&format!("insight #{}", ins)), "{}", r);
        assert!(r.contains("Ivar —boss-of→ user"), "{}", r);
        assert!(!r.contains("recall:"), "recall section is skipped without a recall dir");

        let r2 = why_report(&conn, WhyTarget::Insight(ins), None, None).unwrap();
        assert!(r2.contains(&format!("insight #{}", ins)), "{}", r2);
        assert!(r2.contains("Ivar is the boss now"), "{}", r2);
        assert!(r2.contains(&format!("cited by theme #{}", theme)), "{}", r2);

        let r3 = why_report(&conn, WhyTarget::Insight(theme), None, None).unwrap();
        assert!(r3.starts_with(&format!("theme #{}", theme)), "{}", r3);
        assert!(r3.contains(&format!("insight #{} (confidence 0.80, 1 memories)", ins)), "{}", r3);

        assert_eq!(why_report(&conn, WhyTarget::Memory(9999), None, None).unwrap(), "no memory #9999");
    }

    #[test]
    fn why_report_renders_an_index_owned_derived_memory_correctly() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let id = store::upsert_index_memory(&conn, "helios", "code-index:helios:src/", "helios src/: handles picking", None, &now).unwrap();

        let r = why_report(&conn, WhyTarget::Memory(id), None, None).unwrap();
        assert!(r.contains("derived by the code index"), "{}", r);
        assert!(r.contains("basis: derived"), "{}", r);
    }

    #[test]
    fn why_report_shows_pin_state_only_when_pinned() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let unpinned = store::insert(&conn, "never pinned", None, None, true, None, 5).unwrap();
        let r = why_report(&conn, WhyTarget::Memory(unpinned), None, None).unwrap();
        assert!(!r.contains("pinned:"), "an unpinned row must not print a pinned line: {}", r);

        let old_id = store::insert(&conn, "old fact", None, None, true, None, 5).unwrap();
        let new_id = store::insert(&conn, "new fact", None, None, true, None, 5).unwrap();
        assert!(store::supersede(&conn, old_id, new_id, &now).unwrap());
        assert!(store::restore(&conn, old_id, &now).unwrap());
        let r2 = why_report(&conn, WhyTarget::Memory(old_id), None, None).unwrap();
        assert!(r2.contains(&format!("pinned: {}", now)), "{}", r2);
    }

    #[test]
    fn parse_why_target_accepts_the_three_spellings() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_why_target(&s(&["214"])), Some(WhyTarget::Memory(214)));
        assert_eq!(parse_why_target(&s(&["i28"])), Some(WhyTarget::Insight(28)));
        assert_eq!(parse_why_target(&s(&["insight", "28"])), Some(WhyTarget::Insight(28)));
        assert_eq!(parse_why_target(&s(&["theme", "3"])), Some(WhyTarget::Insight(3)));
        assert_eq!(parse_why_target(&s(&["nope"])), None);
        assert_eq!(parse_why_target(&s(&[])), None);
    }

    #[test]
    fn provenance_phrase_matches_the_hook() {
        assert_eq!(provenance_phrase(Some("session-digest"), None), "picked up from a session");
        assert_eq!(provenance_phrase(Some("session-digest"), Some("stated")), "you said this in a session");
        assert_eq!(provenance_phrase(Some("session-digest"), Some("inferred")), "I inferred this from a session");
        assert_eq!(provenance_phrase(Some("meeting:x"), None), "from a meeting");
        assert_eq!(provenance_phrase(Some("memory-backfill:u/x.md"), None), "from earlier project memory");
        assert_eq!(provenance_phrase(Some("note:2026"), Some("stated")), "you told me");
        assert_eq!(provenance_phrase(None, Some("inferred")), "I inferred this from context");
        assert_eq!(provenance_phrase(Some("session-digest"), Some("experience")), "I did this in a session");
        assert_eq!(provenance_phrase(None, None), "you told me");
        assert_eq!(
            provenance_phrase(Some("code-index:helios:src/"), Some("derived")),
            "derived by the code index",
            "an index-owned module/repo summary is never \"you told me\""
        );
    }

    #[test]
    fn entity_connections_render_each_claim_once_however_many_rows_back_it() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let site = store::insert_entity(&conn, "remosspace.com", Some("infrastructure"), Some(&q)).unwrap();
        let now = store::now_rfc3339();
        // One claim, three separate extractions of it (three evidence rows).
        for _ in 0..3 {
            store::insert_relation(&conn, user, "deploys-to", site, None, Some(0.9), &now).unwrap();
        }
        // Predicate case must not defeat the dedupe either.
        store::insert_relation(&conn, user, "Deploys-To", site, None, Some(0.9), &now).unwrap();
        // A genuinely different claim about the same entity still shows.
        let dash = store::insert_entity(&conn, "dashboard", Some("component"), None).unwrap();
        store::insert_relation(&conn, dash, "deployed-to", site, None, Some(0.9), &now).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "remosspace.com", 5, false, false, 0.0, &now, None, None, None).unwrap();
        let ones: Vec<&ConnectionHit> = resp.connections.iter().filter(|c| c.hops == 1).collect();
        assert_eq!(ones.len(), 2, "one line per distinct claim: {:?}", ones.iter().map(|c| (&c.src_name, &c.predicate, &c.dst_name)).collect::<Vec<_>>());
        assert_eq!(ones.iter().filter(|c| c.predicate.eq_ignore_ascii_case("deploys-to")).count(), 1);
    }

    #[test]
    fn entity_connections_report_each_hop_in_its_stored_direction_never_flipped() {
        // Stored: user --deploys-to--> remosspace.com, user --intends-to-build--> tools.
        // Query matches remosspace.com (the DST of hop 1) and the pivot is
        // user (the SRC of hop 2). Both hops must come back exactly as
        // stored -- never as "remosspace.com --deploys-to--> user".
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let site = store::insert_entity(&conn, "remosspace.com", Some("infrastructure"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let tools = store::insert_entity(&conn, "personal tools", Some("concept"), None).unwrap();
        let now = store::now_rfc3339();
        store::insert_relation(&conn, user, "deploys-to", site, None, Some(0.9), &now).unwrap();
        store::insert_relation(&conn, user, "intends-to-build", tools, None, Some(0.9), &now).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "what runs on the site", 10, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.connections.len(), 2);

        let hop1 = &resp.connections[0];
        assert_eq!((hop1.src_name.as_str(), hop1.predicate.as_str(), hop1.dst_name.as_str()), ("user", "deploys-to", "remosspace.com"));

        let hop2 = &resp.connections[1];
        assert_eq!(hop2.hops, 2);
        assert_eq!((hop2.src_name.as_str(), hop2.predicate.as_str(), hop2.dst_name.as_str()), ("user", "deploys-to", "remosspace.com"));
        assert_eq!(hop2.src_name2.as_deref(), Some("user"));
        assert_eq!(hop2.predicate2.as_deref(), Some("intends-to-build"));
        assert_eq!(hop2.dst_name2.as_deref(), Some("personal tools"));
    }

    #[test]
    fn entity_connections_skips_second_hop_below_the_confidence_floor() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let umoja = store::insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let now = store::now_rfc3339();
        // Hop-1 edge confidence sits below SEARCH_HOP2_MIN_CONFIDENCE (0.6).
        store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.5), &now).unwrap();
        store::insert_relation(&conn, user, "works-on", umoja, None, Some(0.9), &now).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.connections.len(), 1, "a low-confidence hop-1 edge must not seed a hop-2 walk");
        assert_eq!(resp.connections[0].hops, 1);
    }

    #[test]
    fn entity_connections_never_double_counts_a_reverse_edge_to_the_matched_entity_as_a_new_hop2() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        // A second, distinct edge between the same two entities, in the
        // other direction -- both are genuine hop-1 edges (each touches the
        // matched entity directly), but walking the pivot's own edges for
        // hop 2 must not rediscover either of them as new "hop 2"
        // information -- a spurious cycle straight back to the matched
        // entity, whether caught by edge-id dedup or the explicit
        // "not the matched entity" check.
        store::insert_relation(&conn, user, "frustrated-by", moses, None, Some(0.9), &now).unwrap();

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.connections.len(), 2, "both direct edges between Moses and user are hop-1 connections");
        assert!(resp.connections.iter().all(|c| c.hops == 1), "neither edge must be re-surfaced as a spurious hop-2 walk");
    }

    #[test]
    fn entity_connections_caps_hop2_at_two_per_neighbor() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let now = store::now_rfc3339();
        store::insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        for i in 0..3 {
            let far = store::insert_entity(&conn, &format!("Thing{}", i), Some("concept"), None).unwrap();
            store::insert_relation(&conn, user, "likes", far, None, Some(0.9), &now).unwrap();
        }

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now, None, None, None).unwrap();
        // 1 hop-1 edge + at most 2 hop-2 edges through the one neighbor.
        assert_eq!(resp.connections.len(), 3);
        assert_eq!(resp.connections.iter().filter(|c| c.hops == 2).count(), 2);
    }

    #[test]
    fn entity_connections_caps_at_five_total_favoring_hop1() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let now = store::now_rfc3339();
        for i in 0..6 {
            let other = store::insert_entity(&conn, &format!("Person{}", i), Some("person"), None).unwrap();
            store::insert_relation(&conn, moses, "knows", other, None, Some(0.9), &now).unwrap();
        }

        let embedder = FixedVecEmbedder(q);
        let resp = search_hits(&conn, &embedder, "who does Moses know", 10, false, false, 0.0, &now, None, None, None).unwrap();
        assert_eq!(resp.connections.len(), 5, "6 direct edges must still cap at the overall limit");
        assert!(resp.connections.iter().all(|c| c.hops == 1), "hop-1 edges fill the cap before any hop-2 walk runs");
    }

    // --- graph hygiene pass: evidence-death propagation + entity merge ---

    #[test]
    fn run_evidence_death_propagation_invalidates_edges_with_dead_evidence_only() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let umoja = store::insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let alive = store::insert(&conn, "Moses proposes a feature for Umoja", None, None, true, None, 5).unwrap();
        let dying = store::insert(&conn, "a fact about to be superseded", None, None, true, None, 5).unwrap();
        let r_alive = store::insert_relation(&conn, moses, "proposes-feature-for", umoja, Some(alive), Some(0.9), &now).unwrap();
        let r_dying = store::insert_relation(&conn, moses, "used-to-lead", umoja, Some(dying), Some(0.7), &now).unwrap();

        let replacement = store::insert(&conn, "replacement fact", None, None, true, None, 5).unwrap();
        store::supersede(&conn, dying, replacement, &now).unwrap();

        let count = run_evidence_death_propagation(&conn, &now).unwrap();
        assert_eq!(count, 1);
        assert!(!store::get_relation(&conn, r_dying).unwrap().unwrap().is_active());
        assert_eq!(store::get_relation(&conn, r_dying).unwrap().unwrap().superseded_by, None);
        assert!(store::get_relation(&conn, r_alive).unwrap().unwrap().is_active());
    }

    #[test]
    fn run_entity_merge_pass_same_verdict_repoints_and_deletes_the_newer_entity() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let older = store::insert_entity(&conn, "Umoja", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let newer = store::insert_entity(&conn, "umoja project", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let edge = store::insert_relation(&conn, moses, "proposes-feature-for", newer, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: SAME") };
        let (merged, failed) = run_entity_merge_pass(&conn, &llm, &now).unwrap();
        assert_eq!(merged, 1);
        assert!(!failed);
        assert!(store::get_entity(&conn, newer).unwrap().is_none(), "the newer entity row must be deleted");
        assert!(store::get_entity(&conn, older).unwrap().is_some(), "the older entity survives");
        assert_eq!(store::get_relation(&conn, edge).unwrap().unwrap().dst, older, "its edge must be repointed to the survivor");
    }

    #[test]
    fn run_entity_merge_pass_different_verdict_marks_seen_and_touches_nothing() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let a = store::insert_entity(&conn, "Umoja", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let b = store::insert_entity(&conn, "umoja project", Some("project"), Some(&unit_vec(4, 0))).unwrap();

        let llm = FixedReflectLlm { reply: Ok("1: DIFFERENT") };
        let (merged, failed) = run_entity_merge_pass(&conn, &llm, &now).unwrap();
        assert_eq!(merged, 0);
        assert!(!failed);
        assert!(store::get_entity(&conn, a).unwrap().is_some());
        assert!(store::get_entity(&conn, b).unwrap().is_some());
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        assert_eq!(store::entity_merge_seen_pairs(&conn).unwrap(), [(lo, hi)].into_iter().collect());
    }

    #[test]
    fn run_entity_merge_pass_failed_call_never_marks_seen_or_merges() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert_entity(&conn, "Umoja", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        store::insert_entity(&conn, "umoja project", Some("project"), Some(&unit_vec(4, 0))).unwrap();

        let llm = FixedReflectLlm { reply: Err("offline") };
        let (merged, failed) = run_entity_merge_pass(&conn, &llm, &now).unwrap();
        assert_eq!(merged, 0);
        assert!(failed);
        assert!(store::entity_merge_seen_pairs(&conn).unwrap().is_empty(), "a transport failure must not be recorded as seen");
    }

    #[test]
    fn run_entity_merge_pass_no_candidates_below_similarity_never_calls_the_judge() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert_entity(&conn, "Moses", Some("person"), Some(&unit_vec(4, 0))).unwrap();
        store::insert_entity(&conn, "Ivar", Some("person"), Some(&unit_vec(4, 1))).unwrap();

        struct PanicLlm;
        impl ReflectLlm for PanicLlm {
            fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                panic!("must never be called when there are no candidates");
            }
        }
        let (merged, failed) = run_entity_merge_pass(&conn, &PanicLlm, &now).unwrap();
        assert_eq!(merged, 0);
        assert!(!failed);
    }

    // --- mach kb graph audit ---

    #[test]
    fn run_graph_audit_invalidates_poisoned_and_generic_keeps_the_rest() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let umoja = store::insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let boss = store::insert_entity(&conn, "boss", None, None).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();

        // Known-good edge that must survive.
        let r_good = store::insert_relation(&conn, moses, "proposes-feature-for", umoja, None, Some(0.9), &now).unwrap();
        // A test-describing edge -- poisoned.
        let r_poisoned =
            store::insert_relation(&conn, moses, "supersedes-as-boss", umoja, None, Some(0.5), &now).unwrap();
        // The dead generic-role pattern this guard exists for.
        let r_generic = store::insert_relation(&conn, user, "has-boss", boss, None, Some(0.5), &now).unwrap();

        let llm = OwnedReplyLlm { reply: "1: KEEP\n2: POISONED\n3: GENERIC\n".to_string() };
        let (kept, invalidated, descs, failed) = run_graph_audit(&conn, &llm, &now).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(invalidated, 2);
        assert!(!failed);
        assert_eq!(descs.len(), 2);

        assert!(store::get_relation(&conn, r_good).unwrap().unwrap().is_active());
        assert!(!store::get_relation(&conn, r_poisoned).unwrap().unwrap().is_active());
        assert!(!store::get_relation(&conn, r_generic).unwrap().unwrap().is_active());
        // Invalidation only, never deleted -- the row is still readable.
        assert!(store::get_relation(&conn, r_poisoned).unwrap().is_some());
    }

    #[test]
    fn run_graph_audit_unaddressed_edge_is_treated_as_kept_not_invalidated() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let a = store::insert_entity(&conn, "A", None, None).unwrap();
        let b = store::insert_entity(&conn, "B", None, None).unwrap();
        let edge = store::insert_relation(&conn, a, "rel", b, None, Some(0.9), &now).unwrap();

        // The reply never addresses edge number 1 at all.
        let llm = FixedReflectLlm { reply: Ok("garbled reply with no verdict lines") };
        let (kept, invalidated, descs, failed) = run_graph_audit(&conn, &llm, &now).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(invalidated, 0);
        assert!(descs.is_empty());
        assert!(!failed, "the call itself succeeded -- only its content was unaddressed");
        assert!(store::get_relation(&conn, edge).unwrap().unwrap().is_active());
    }

    #[test]
    fn run_graph_audit_failed_batch_call_counts_as_kept_and_marks_the_run_degraded() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        let a = store::insert_entity(&conn, "A", None, None).unwrap();
        let b = store::insert_entity(&conn, "B", None, None).unwrap();
        let edge = store::insert_relation(&conn, a, "rel", b, None, Some(0.9), &now).unwrap();

        let llm = FixedReflectLlm { reply: Err("offline") };
        let (kept, invalidated, descs, failed) = run_graph_audit(&conn, &llm, &now).unwrap();
        assert_eq!(kept, 1);
        assert_eq!(invalidated, 0);
        assert!(descs.is_empty());
        assert!(failed);
        assert!(store::get_relation(&conn, edge).unwrap().unwrap().is_active(), "never destructive on a failed call");
    }

    // --- mach kb audit-supersessions ---

    /// Seeds one tombstoned old/new pair with distinct content so the
    /// prompt/parser and the pass's own db writes have something real to
    /// check against. Returns `(old_id, new_id, now)`.
    fn seed_one_supersession(conn: &Connection) -> (i64, i64, String) {
        let old_id = store::insert(conn, "old fact", None, None, true, None, 5).unwrap();
        let new_id = store::insert(conn, "new fact", None, None, true, None, 5).unwrap();
        let now = store::now_rfc3339();
        assert!(store::supersede(conn, old_id, new_id, &now).unwrap());
        (old_id, new_id, now)
    }

    #[test]
    fn run_supersession_audit_dry_run_records_verdicts_but_changes_nothing() {
        let conn = mem_conn();
        let (old_id, new_id, now) = seed_one_supersession(&conn);
        let llm = OwnedReplyLlm { reply: format!("{} LOSSY dropped a dated fact", old_id) };

        let result = run_supersession_audit(&conn, &llm, &now, None, false, false).unwrap();
        assert_eq!(result.audited, 1);
        assert_eq!(result.lossy, 1);
        assert_eq!(result.ok, 0);
        assert_eq!(result.no_verdict, 0);
        assert!(!result.llm_failed);
        assert_eq!(result.lossy_rows.len(), 1);
        assert_eq!(result.lossy_rows[0].old_id, old_id);
        assert_eq!(result.lossy_rows[0].new_id, new_id);
        assert!(!result.lossy_rows[0].restored, "dry run must never restore");

        // The row is still tombstoned -- a dry run changes nothing about
        // the memories themselves.
        let old = store::get(&conn, old_id).unwrap().unwrap();
        assert!(old.is_superseded());
        assert!(!store::is_pinned(&conn, old_id).unwrap());

        // The verdict IS durably recorded, though, so a rerun won't re-ask.
        let row = store::get_supersession_audit(&conn, old_id).unwrap().unwrap();
        assert_eq!(row.verdict, "LOSSY");
        assert_eq!(row.reason, "dropped a dated fact");
    }

    #[test]
    fn run_supersession_audit_apply_restores_and_pins_only_lossy_rows() {
        let conn = mem_conn();
        let (lossy_old, lossy_new, now) = seed_one_supersession(&conn);
        let ok_old = store::insert(&conn, "old ok fact", None, None, true, None, 5).unwrap();
        let ok_new = store::insert(&conn, "new fact restates it", None, None, true, None, 5).unwrap();
        assert!(store::supersede(&conn, ok_old, ok_new, &now).unwrap());

        let llm = OwnedReplyLlm {
            reply: format!("{} LOSSY dropped a dated fact\n{} OK fully restated", lossy_old, ok_old),
        };
        let result = run_supersession_audit(&conn, &llm, &now, None, true, false).unwrap();
        assert_eq!(result.audited, 2);
        assert_eq!(result.lossy, 1);
        assert_eq!(result.ok, 1);
        assert_eq!(result.lossy_rows.len(), 1);
        assert!(result.lossy_rows[0].restored);

        // Only the LOSSY row was restored and pinned.
        let restored = store::get(&conn, lossy_old).unwrap().unwrap();
        assert!(!restored.is_superseded());
        assert!(store::is_pinned(&conn, lossy_old).unwrap());

        // The OK row stays exactly as it was: still tombstoned, never pinned.
        let still_tombstoned = store::get(&conn, ok_old).unwrap().unwrap();
        assert!(still_tombstoned.is_superseded());
        assert!(!store::is_pinned(&conn, ok_old).unwrap());

        assert_eq!(store::get_supersession_audit(&conn, lossy_old).unwrap().unwrap().verdict, "LOSSY");
        assert_eq!(store::get_supersession_audit(&conn, ok_old).unwrap().unwrap().verdict, "OK");
        let _ = lossy_new;
    }

    #[test]
    fn run_supersession_audit_unaddressed_pair_gets_no_verdict_and_is_never_restored() {
        let conn = mem_conn();
        let (old_id, _new_id, now) = seed_one_supersession(&conn);
        let llm = FixedReflectLlm { reply: Ok("garbled reply with no verdict lines") };

        let result = run_supersession_audit(&conn, &llm, &now, None, true, false).unwrap();
        assert_eq!(result.audited, 0);
        assert_eq!(result.no_verdict, 1);
        assert!(result.lossy_rows.is_empty());
        assert!(!result.llm_failed, "the call itself succeeded -- only its content was unaddressed");

        assert!(store::get(&conn, old_id).unwrap().unwrap().is_superseded(), "never restore on no verdict");
        assert!(store::get_supersession_audit(&conn, old_id).unwrap().is_none(), "unaddressed pairs are retried, not recorded");
    }

    #[test]
    fn run_supersession_audit_failed_batch_call_never_restores_or_records() {
        let conn = mem_conn();
        let (old_id, _new_id, now) = seed_one_supersession(&conn);
        let llm = FixedReflectLlm { reply: Err("offline") };

        let result = run_supersession_audit(&conn, &llm, &now, None, true, false).unwrap();
        assert_eq!(result.no_verdict, 1);
        assert!(result.llm_failed);
        assert!(store::get(&conn, old_id).unwrap().unwrap().is_superseded());
        assert!(store::get_supersession_audit(&conn, old_id).unwrap().is_none());
    }

    #[test]
    fn run_supersession_audit_already_audited_rows_are_skipped_unless_reaudit() {
        let conn = mem_conn();
        let (old_id, new_id, now) = seed_one_supersession(&conn);
        store::record_supersession_audit(&conn, old_id, new_id, "OK", "already judged", &now).unwrap();

        struct PanicLlm;
        impl ReflectLlm for PanicLlm {
            fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                panic!("must never be called when every candidate is already audited");
            }
        }
        let result = run_supersession_audit(&conn, &PanicLlm, &now, None, false, false).unwrap();
        assert_eq!(result.audited, 0);
        assert_eq!(result.lossy, 0);
        assert_eq!(result.ok, 0);
        assert_eq!(result.no_verdict, 0);

        // --reaudit re-examines it.
        let llm = OwnedReplyLlm { reply: format!("{} LOSSY re-examined and found lossy", old_id) };
        let result = run_supersession_audit(&conn, &llm, &now, None, false, true).unwrap();
        assert_eq!(result.audited, 1);
        assert_eq!(result.lossy, 1);
        assert_eq!(store::get_supersession_audit(&conn, old_id).unwrap().unwrap().verdict, "LOSSY", "reaudit overwrites the prior verdict");
    }

    #[test]
    fn audit_supersessions_apply_never_undoes_a_newer_manual_supersede() {
        // Regression test for "audit --apply can undo a manual supersede":
        // the original restore filter checked only "is this row still
        // tombstoned", so a LOSSY verdict recorded against an old hop
        // (A -> B) could restore A even after a human had since manually
        // re-superseded A to a completely different, later row (A -> D)
        // via `mach kb supersede A D` -- silently reverting a manual
        // decision the guard must never touch. Fixed by (a) restoring only
        // when the memory's CURRENT superseded_by still equals the
        // recorded new_id, and (b) treating a candidate as already-audited
        // only when the recorded new_id equals the CURRENT new_id, so the
        // fresh A -> D hop is judged again rather than silently skipped.
        let conn = mem_conn();
        let (a, b, now) = seed_one_supersession(&conn); // A -> B

        // A dry run finds and records A -> B as LOSSY (never restores).
        let dry_llm = OwnedReplyLlm { reply: format!("{} LOSSY dropped context", a) };
        let dry = run_supersession_audit(&conn, &dry_llm, &now, None, false, false).unwrap();
        assert_eq!(dry.lossy, 1);

        // The row is restored (e.g. by a prior --apply run, or `mach kb
        // restore` by hand).
        assert!(store::restore(&conn, a, &now).unwrap());
        assert!(!store::get(&conn, a).unwrap().unwrap().is_superseded());

        // A human manually supersedes A to a brand-new row D -- a fresh
        // decision the stale A -> B audit record knows nothing about.
        let d = store::insert(&conn, "even newer fact D", None, None, true, None, 5).unwrap();
        assert!(store::supersede(&conn, a, d, &now).unwrap());

        // The new A -> D hop must be sent to the judge as a fresh
        // candidate -- not silently skipped as "already audited" just
        // because A was audited before (part b of the fix). Judge it OK
        // so this test isolates the restore-filter behavior (part a) from
        // the audit's own verdict content.
        let fresh_llm = OwnedReplyLlm { reply: format!("{} OK fully restated in D", a) };
        let result = run_supersession_audit(&conn, &fresh_llm, &now, None, true, false).unwrap();
        assert_eq!(result.audited, 1, "A->D must be judged fresh, not skipped as already-audited");
        assert_eq!(result.ok, 1);

        // Crucially (part a of the fix): --apply must NOT restore A over
        // this newer manual supersede, even though a LOSSY verdict for A
        // is still on record in supersession_audit (against the old B,
        // not the current D).
        let still = store::get(&conn, a).unwrap().unwrap();
        assert!(still.is_superseded(), "A must stay tombstoned -- a stale A->B LOSSY verdict must never restore over a newer manual A->D supersede");
        assert_eq!(still.superseded_by, Some(d));
        let _ = b;
    }

    #[test]
    fn run_supersession_audit_apply_restores_a_previously_recorded_lossy_row_without_reasking_the_judge() {
        // The bug this closes: a dry run finds and records a LOSSY verdict
        // but (by definition) never restores it. A LATER `--apply` run
        // must still restore that row even though the pair is now
        // already-audited and so never goes back to the judge -- the
        // real evalhome run hit exactly this: 93 LOSSY rows recorded by a
        // dry run stayed tombstoned forever because a naive `--apply`
        // only restored pairs it freshly judged in that same call.
        let conn = mem_conn();
        let (old_id, _new_id, now) = seed_one_supersession(&conn);

        let dry_llm = OwnedReplyLlm { reply: format!("{} LOSSY dropped a dated fact", old_id) };
        let dry = run_supersession_audit(&conn, &dry_llm, &now, None, false, false).unwrap();
        assert_eq!(dry.lossy, 1);
        assert!(store::get(&conn, old_id).unwrap().unwrap().is_superseded(), "dry run must not restore");

        struct PanicLlm;
        impl ReflectLlm for PanicLlm {
            fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
                panic!("must never re-ask a pair already recorded in supersession_audit");
            }
        }
        let apply_result = run_supersession_audit(&conn, &PanicLlm, &now, None, true, false).unwrap();
        assert_eq!(apply_result.audited, 0, "nothing new to judge -- the pair was already recorded");
        assert_eq!(apply_result.lossy_rows.len(), 1);
        assert!(apply_result.lossy_rows[0].restored);
        assert_eq!(apply_result.lossy_rows[0].old_id, old_id);

        let restored = store::get(&conn, old_id).unwrap().unwrap();
        assert!(!restored.is_superseded());
        assert!(store::is_pinned(&conn, old_id).unwrap());

        // A third run (dry or apply) reports nothing further to do -- the
        // row is no longer tombstoned, so it drops out of the repair queue.
        let again = run_supersession_audit(&conn, &PanicLlm, &now, None, true, false).unwrap();
        assert!(again.lossy_rows.is_empty(), "an already-restored row is not reported again");
    }

    #[test]
    fn run_supersession_audit_limit_caps_candidates_oldest_first() {
        let conn = mem_conn();
        let (old_a, _new_a, now) = seed_one_supersession(&conn);
        let old_b = store::insert(&conn, "old fact b", None, None, true, None, 5).unwrap();
        let new_b = store::insert(&conn, "new fact b", None, None, true, None, 5).unwrap();
        assert!(store::supersede(&conn, old_b, new_b, &now).unwrap());

        // old_a was inserted (and tombstoned) first, so it is the older
        // candidate even when both share a timestamp -- id breaks the tie.
        let llm = OwnedReplyLlm { reply: format!("{} OK fine", old_a) };
        let result = run_supersession_audit(&conn, &llm, &now, Some(1), false, false).unwrap();
        assert_eq!(result.audited, 1, "--limit 1 sends only the oldest candidate");
    }

    // --- recall enrichment: dormant evidence excluded, never tombstoned ---

    #[test]
    fn entity_connections_for_query_excludes_edges_with_dormant_evidence() {
        let conn = mem_conn();
        let q = unit_vec(4, 0);
        let now = store::now_rfc3339();
        let moses = store::insert_entity(&conn, "Moses", Some("person"), Some(&q)).unwrap();
        let user = store::insert_entity(&conn, "user", None, None).unwrap();
        let evidence_mem = store::insert(&conn, "Moses is the user's boss", None, None, true, None, 5).unwrap();
        let edge = store::insert_relation(&conn, moses, "boss-of", user, Some(evidence_mem), Some(0.9), &now).unwrap();

        // Still active/live: the connection surfaces.
        let before = entity_connections_for_query(&conn, "unrelated query text", &q);
        assert_eq!(before.len(), 1);

        // Evidence naps -- must be excluded from recall connections, but the
        // edge itself must remain untouched (never invalidated).
        store::set_dormant(&conn, evidence_mem, &now).unwrap();
        let during_nap = entity_connections_for_query(&conn, "unrelated query text", &q);
        assert!(during_nap.is_empty(), "a dormant-evidence edge must not surface in recall connections");
        assert!(store::get_relation(&conn, edge).unwrap().unwrap().is_active(), "must never be tombstoned for napping");

        // Evidence wakes -- the connection surfaces again, same edge row.
        store::wake(&conn, evidence_mem, &now).unwrap();
        let after_wake = entity_connections_for_query(&conn, "unrelated query text", &q);
        assert_eq!(after_wake.len(), 1);
        assert_eq!(after_wake[0].src_name, "Moses");
    }
    /// Counts digest calls and returns a fixed fact list -- for the
    /// checkpoint (`--partial`) flow.
    struct DigestCountingLlm {
        calls: std::cell::Cell<usize>,
        facts: &'static str,
    }
    impl ReflectLlm for DigestCountingLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: std::time::Duration) -> Result<String, String> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.facts.to_string())
        }
    }

    /// Filter that returns the raw text it was given, so a test can see
    /// which slice reached the digest by inspecting prompt-independent
    /// side effects (line counts via `seen`).
    struct EchoFilter {
        seen: std::cell::RefCell<Vec<usize>>,
    }
    impl ingest::TranscriptFilter for EchoFilter {
        fn filter(&self, raw: &str) -> Option<String> {
            self.seen.borrow_mut().push(raw.lines().count());
            if raw.trim().is_empty() {
                None
            } else {
                Some(raw.to_string())
            }
        }
    }

    #[test]
    fn partial_checkpoints_digest_only_new_lines_and_final_pass_takes_the_tail() {
        let scratch = ScratchDir::new("partial");
        let conn = mem_conn();
        let projects_root = scratch.path.join("projects");
        let recall_root = scratch.path.join("recall-log");
        let now = store::now_rfc3339();
        let llm =
            DigestCountingLlm { calls: std::cell::Cell::new(0), facts: "STATED: Ivan ships releases on Fridays\nSTATED: Ivan runs Arch Linux locally" };
        let filter = EchoFilter { seen: std::cell::RefCell::new(Vec::new()) };

        // 100 raw lines, first checkpoint: everything is new
        write_transcript(&scratch.path, "sess-p", 100, None);
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-p"), true, &now).unwrap();
        assert_eq!((s.checkpointed, s.facts_added, s.processed), (1, 2, 0));
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 100);
        assert!(!store::is_session_ingested(&conn, "sess-p").unwrap());
        assert_eq!(filter.seen.borrow().last().copied(), Some(100));

        // only 20 new lines: below PARTIAL_MIN_NEW_LINES, nothing happens
        write_transcript(&scratch.path, "sess-p", 120, None);
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-p"), true, &now).unwrap();
        assert_eq!(s.checkpointed, 0);
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 100);
        assert_eq!(llm.calls.get(), 1);

        // 60 new lines: second checkpoint sees exactly those 60
        write_transcript(&scratch.path, "sess-p", 160, None);
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-p"), true, &now).unwrap();
        assert_eq!(s.checkpointed, 1);
        assert_eq!(filter.seen.borrow().last().copied(), Some(60));
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 160);

        // final pass: engagement over the whole file (160), digest over the
        // 45-line tail only, session marked done, progress cleared
        write_transcript(&scratch.path, "sess-p", 205, None);
        let calls_before = llm.calls.get();
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-p"), false, &now).unwrap();
        assert_eq!(s.processed, 1);
        assert_eq!(s.facts_added, 2);
        assert_eq!(llm.calls.get() - calls_before, 1, "no injected ids, so exactly one digest call");
        let seen = filter.seen.borrow();
        assert_eq!(&seen[seen.len() - 2..], &[205, 45], "whole file for engagement, tail for digest");
        drop(seen);
        assert!(store::is_session_ingested(&conn, "sess-p").unwrap());
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 205, "mark persists past the final pass");
        assert_eq!(store::list(&conn, None, false).unwrap().iter().filter(|m| m.source.as_deref() == Some("session-digest")).count(), 6);

        // the session keeps growing after being marked finished: a later
        // sweep digests only the new tail, marks nothing, judges nothing
        write_transcript(&scratch.path, "sess-p", 260, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        let calls_before = llm.calls.get();
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, None, false, &now).unwrap();
        assert_eq!((s.checkpointed, s.processed, s.facts_added), (1, 0, 2));
        assert_eq!(llm.calls.get() - calls_before, 1);
        assert_eq!(filter.seen.borrow().last().copied(), Some(55));
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 260);
        // and a checkpoint on the same finished-but-alive session works too
        write_transcript(&scratch.path, "sess-p", 310, None);
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-p"), true, &now).unwrap();
        assert_eq!(s.checkpointed, 1);
        assert_eq!(store::session_progress(&conn, "sess-p").unwrap(), 310);
    }

    #[test]
    fn co_engaged_memories_get_a_hebbian_edge() {
        let scratch = ScratchDir::new("hebb");
        let conn = mem_conn();
        let projects_root = scratch.path.join("projects");
        let recall_root = scratch.path.join("recall-log");
        std::fs::create_dir_all(&recall_root).unwrap();
        let now = store::now_rfc3339();
        let a = store::insert(&conn, "fact a", None, None, true, None, 5).unwrap();
        let b = store::insert(&conn, "fact b", None, None, true, None, 5).unwrap();
        let c = store::insert(&conn, "fact c", None, None, true, None, 5).unwrap();
        write_transcript(&scratch.path, "sess-h", 10, None);
        std::fs::write(recall_root.join("sess-h.jsonl"), format!("{{\"ts\":\"t\",\"ids\":[{},{},{}]}}\n", a, b, c)).unwrap();
        let reply = format!("{} ENGAGED\n{} ENGAGED\n{} SHOWN", a, b, c);
        let llm = OwnedReplyLlm { reply };
        let filter = FixedFilter { dialogue: Some("user: talked about a and b") };
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-h"), false, &now).unwrap();
        assert_eq!((s.engaged, s.shown), (2, 1));
        assert_eq!(store::count_assoc(&conn).unwrap(), 1);
        assert_eq!(store::assoc_neighbors(&conn, a, &now).unwrap()[0].0, b);
    }

    #[test]
    fn partial_without_session_id_is_a_no_op_and_offline_keeps_the_mark() {
        let scratch = ScratchDir::new("partial-noop");
        let conn = mem_conn();
        let projects_root = scratch.path.join("projects");
        let recall_root = scratch.path.join("recall-log");
        let now = store::now_rfc3339();
        write_transcript(&scratch.path, "sess-q", 100, Some(ingest::SWEEP_MIN_IDLE_SECS + 60));
        let llm = FixedReflectLlm { reply: Err("offline") };
        let filter = FixedFilter { dialogue: Some("user: we decided x") };
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, None, true, &now).unwrap();
        assert_eq!(s.scanned, 0);
        let s = ingest_sessions_impl(&conn, &FakeEmbedder, &llm, &filter, &projects_root, &recall_root, Some("sess-q"), true, &now).unwrap();
        assert_eq!((s.checkpointed, s.deferred_offline), (0, 1));
        assert_eq!(store::session_progress(&conn, "sess-q").unwrap(), 0, "offline: mark not advanced");
    }

    #[test]
    fn search_spreads_activation_over_hebbian_edges() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        // One-hot embeddings: each text lives on its own axis, so a query
        // matches exactly one memory and is orthogonal to the others.
        struct AxisEmbedder;
        impl Embedder for AxisEmbedder {
            fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
                Ok(match text {
                    t if t.contains("hub") => vec![1.0, 0.0, 0.0],
                    t if t.contains("strangler") => vec![0.0, 1.0, 0.0],
                    _ => vec![0.0, 0.0, 1.0],
                })
            }
        }
        let e = AxisEmbedder;
        let hub = store::insert(&conn, "audit hub endpoints", None, None, true, Some(&e.embed("hub").unwrap()), 5).unwrap();
        let strangler = store::insert(&conn, "strangler pattern", None, None, true, Some(&e.embed("strangler").unwrap()), 5).unwrap();
        let _lonely = store::insert(&conn, "lonely", None, None, true, Some(&e.embed("lonely").unwrap()), 5).unwrap();

        let before = search_hits(&conn, &e, "audit hub endpoints", 5, false, false, 0.5, &now, None, None, None).unwrap();
        assert!(before.hits.iter().all(|h| h.via_assoc.is_none()));
        assert!(!before.hits.iter().any(|h| h.id == strangler), "no edge yet: strangler is not near the query");

        store::reinforce_assoc(&conn, &[hub, strangler], &now).unwrap();
        store::reinforce_assoc(&conn, &[hub, strangler], &now).unwrap();
        let after = search_hits(&conn, &e, "audit hub endpoints", 5, false, false, 0.3, &now, None, None, None).unwrap();
        let assoc = after.hits.iter().find(|h| h.id == strangler).expect("strangler pulled in by association");
        assert_eq!(assoc.via_assoc, Some(hub));
        let src = after.hits.iter().find(|h| h.id == hub).unwrap();
        assert!(assoc.score < src.score, "associate never outranks its source");
        assert!(assoc.score > 0.3);
        assert!(!after.hits.iter().any(|h| h.content.contains("lonely")));
    }

    // --- refresh_one_project ---

    #[test]
    fn refresh_registers_renames_and_retags_without_touching_other_projects() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::insert(&conn, "umoja fact", Some("project-index:umoja"), Some("umoja"), true, None, 5).unwrap();
        let (_id, _) = store::upsert_project(&conn, "git:abc", "umoja", "/tmp/old-umoja", &now).unwrap();

        // Same fingerprint appears under a new basename.
        let (renamed, retagged) = refresh_one_project(&conn, "git:abc", "umoja-v2", "/tmp/umoja-v2", None, &now).unwrap();
        assert!(renamed, "a rename did happen");
        assert_eq!(retagged, 2, "project column plus source string");
        let row = store::get_project_by_fingerprint(&conn, "git:abc").unwrap().unwrap();
        assert_eq!(row.name, "umoja-v2");
        assert_eq!(row.root_path, "/tmp/umoja-v2");
    }

    #[test]
    fn refresh_is_a_no_op_for_an_unchanged_project() {
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::upsert_project(&conn, "git:def", "helios", "/tmp/helios", &now).unwrap();
        assert_eq!(refresh_one_project(&conn, "git:def", "helios", "/tmp/helios", None, &now).unwrap(), (false, 0));
    }

    #[test]
    fn refresh_reports_a_rename_with_zero_memories_as_renamed_not_as_zero_renamed() {
        // The bug: renamed (bool from `previous.is_some()`) and retagged
        // (row count from `retag_project`) used to be conflated into one
        // number, so a project with no memories yet -- a rename with
        // nothing to retag -- printed "0 renamed" even though a rename DID
        // happen. The two must be distinguishable.
        let conn = mem_conn();
        let now = store::now_rfc3339();
        store::upsert_project(&conn, "git:xyz", "old-name", "/tmp/old-name", &now).unwrap();
        let (renamed, retagged) = refresh_one_project(&conn, "git:xyz", "new-name", "/tmp/new-name", None, &now).unwrap();
        assert!(renamed, "a rename happened even though there was nothing to retag");
        assert_eq!(retagged, 0, "no memories existed yet, so nothing was retagged");
    }

    // --- recall-stats formatter (pure: no db, no clock) ---

    #[test]
    fn format_recall_stats_empty_window_prints_an_explanatory_line() {
        let lines = format_recall_stats(&[]);
        assert_eq!(lines, vec!["mach kb recall-stats: no judged sessions in this window".to_string()]);
    }

    #[test]
    fn format_recall_stats_one_session_reports_its_own_precision_and_the_overall_line() {
        let rows = vec![store::RecallStatsRow { session_id: "abcdef1234567890".to_string(), shown: 4, engaged: 3 }];
        let lines = format_recall_stats(&rows);
        assert_eq!(lines.len(), 2, "one session row plus the overall line");
        assert!(lines[0].starts_with("abcdef12"), "session id is truncated to 8 chars: {:?}", lines[0]);
        assert!(!lines[0].contains("567890"), "must not leak past the 8-char truncation: {:?}", lines[0]);
        assert!(lines[0].contains('4'), "shown count present: {:?}", lines[0]);
        assert!(lines[0].contains('3'), "engaged count present: {:?}", lines[0]);
        assert!(lines[0].contains("75%"), "3/4 = 75%: {:?}", lines[0]);
        assert!(lines[1].starts_with("OVERALL"));
        assert!(lines[1].contains("75%"), "single session -> overall matches it: {:?}", lines[1]);
    }

    #[test]
    fn format_recall_stats_overall_line_aggregates_across_sessions() {
        let rows = vec![
            store::RecallStatsRow { session_id: "session1".to_string(), shown: 2, engaged: 2 }, // 100%
            store::RecallStatsRow { session_id: "session2".to_string(), shown: 2, engaged: 0 }, // 0%
        ];
        let lines = format_recall_stats(&rows);
        assert_eq!(lines.len(), 3);
        let overall = lines.last().unwrap();
        assert!(overall.starts_with("OVERALL"));
        assert!(overall.contains('4'), "total shown = 4: {:?}", overall);
        assert!(overall.contains('2'), "total engaged = 2: {:?}", overall);
        assert!(overall.contains("50%"), "2/4 = 50% blended, not averaged per-session: {:?}", overall);
    }

    #[test]
    fn format_recall_stats_short_session_id_is_not_truncated_further() {
        // A session id shorter than 8 chars (shouldn't happen in practice,
        // but the formatter must not panic slicing past the string's end).
        let rows = vec![store::RecallStatsRow { session_id: "abc".to_string(), shown: 1, engaged: 1 }];
        let lines = format_recall_stats(&rows);
        assert!(lines[0].starts_with("abc"));
    }
}

#[cfg(test)]
mod improve_run_tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;
    use std::time::Duration;

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().take(8).map(|b| b as f32).collect())
        }
    }

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("mach-kb-improve-run-{}-{}-{}", std::process::id(), tag, n));
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    /// A home with every target present and a db holding enough signal to
    /// open the gate.
    fn setup(tag: &str) -> (Scratch, improve::Targets, Connection) {
        let s = Scratch::new(tag);
        let t = improve::Targets::from_home(&s.0.join("home"));
        write(&t.skills_dir.join("kb/SKILL.md"), "---\nname: kb\ndescription: memory\n---\nbody\n");
        write(&t.claude_md, "# Global Rules\n\nrule one\nrule two\n");
        write(&t.settings_json, "{\"hooks\":{}}");
        write(&t.hooks_dir.join("kb-model.sh"), "#!/usr/bin/env bash\nexit 0\n");
        let conn = store::open_with_path(Path::new(":memory:")).unwrap();
        for i in 0..6 {
            store::insert(&conn, &format!("user rejects unrequested debug logging {}", i), Some("session-digest"), None, true, None, 5)
                .unwrap();
        }
        (s, t, conn)
    }

    /// Writes the given files (relative to home) then returns `reply`.
    struct FakeLlm {
        writes: Vec<(String, String)>,
        reply: String,
    }
    impl ImproveLlm for FakeLlm {
        fn run(&self, prompt: &str, targets: &improve::Targets, _m: &str, _t: Duration) -> Result<String, String> {
            assert!(prompt.contains("IMPROVE-RESULT"));
            let home = targets.claude_md.parent().unwrap().parent().unwrap();
            for (rel, content) in &self.writes {
                write(&home.join(rel), content);
            }
            Ok(self.reply.clone())
        }
    }

    #[derive(Default)]
    struct FakeVcs {
        managed: HashSet<PathBuf>,
        status_before: Vec<PathBuf>,
        status_after: Vec<PathBuf>,
        dirty: Vec<PathBuf>,
        added: RefCell<Vec<PathBuf>>,
        forgotten: RefCell<Vec<PathBuf>>,
        forced: RefCell<Vec<PathBuf>>,
        commits: RefCell<Vec<String>>,
        // argv capture: every `targets` slice `commit` was actually called
        // with, so a test can assert it is exactly the four write targets
        // and never anything else (a dirty unrelated path in particular).
        commit_targets: RefCell<Vec<Vec<PathBuf>>>,
        calls: RefCell<u32>,
    }
    impl FakeVcs {
        fn managing(t: &improve::Targets) -> Self {
            FakeVcs { managed: t.roots().iter().map(|p| p.to_path_buf()).collect(), ..Default::default() }
        }
    }
    impl Vcs for FakeVcs {
        fn managed(&self) -> Result<HashSet<PathBuf>, String> {
            Ok(self.managed.clone())
        }
        fn status(&self) -> Result<Vec<PathBuf>, String> {
            // first call is the pre-run status (`status_before`, empty/clean
            // by default), later calls are post-run (`status_after`)
            let mut c = self.calls.borrow_mut();
            *c += 1;
            Ok(if *c == 1 { self.status_before.clone() } else { self.status_after.clone() })
        }
        fn dirty_targets(&self, targets: &[&Path]) -> Result<Vec<PathBuf>, String> {
            Ok(targets.iter().map(|p| p.to_path_buf()).filter(|p| self.dirty.contains(p)).collect())
        }
        fn add(&self, path: &Path) -> Result<(), String> {
            self.added.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn forget(&self, path: &Path) -> Result<(), String> {
            self.forgotten.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn apply_force(&self, path: &Path) -> Result<(), String> {
            self.forced.borrow_mut().push(path.to_path_buf());
            Ok(())
        }
        fn commit(&self, message: &str, targets: &[&Path]) -> Result<String, String> {
            self.commits.borrow_mut().push(message.to_string());
            self.commit_targets.borrow_mut().push(targets.iter().map(|p| p.to_path_buf()).collect());
            Ok("abc1234".to_string())
        }
    }

    fn go(conn: &Connection, llm: &FakeLlm, vcs: &FakeVcs, t: &improve::Targets, snap: &Path, dry: bool) -> ImproveRun {
        run_improve(conn, &FakeEmbedder, llm, vcs, t, snap, "sonnet", Duration::from_secs(5), 5, false, dry, "2026-09-08T12:00:00Z", 1_800_000_000)
            .unwrap()
    }

    fn outcomes(conn: &Connection) -> Vec<Memory> {
        store::memories_by_source_prefix_or_project(conn, improve::OUTCOME_SOURCE_PREFIX, improve::OUTCOME_PROJECT).unwrap()
    }

    #[test]
    fn below_threshold_marks_completed_without_calling_anything() {
        let (s, t, conn) = setup("gate");
        let conn2 = store::open_with_path(Path::new(":memory:")).unwrap();
        let llm = FakeLlm { writes: vec![], reply: String::new() };
        let vcs = FakeVcs::managing(&t);
        match go(&conn2, &llm, &vcs, &t, &s.0.join("snap"), false) {
            ImproveRun::BelowThreshold { signal, min_signal } => {
                assert_eq!((signal, min_signal), (0, 5));
            }
            other => panic!("{:?}", other),
        }
        assert!(store::get_improve_state(&conn2).unwrap().last_completed_at.is_some());
        assert!(vcs.commits.borrow().is_empty());
        drop(conn);
    }

    #[test]
    fn dry_run_returns_prompt_and_touches_nothing() {
        let (s, t, conn) = setup("dry");
        let llm = FakeLlm { writes: vec![(".claude/CLAUDE.md".into(), "x".into())], reply: String::new() };
        let vcs = FakeVcs::managing(&t);
        match go(&conn, &llm, &vcs, &t, &s.0.join("snap"), true) {
            ImproveRun::DryRun { prompt } => assert!(prompt.contains("unrequested debug logging")),
            other => panic!("{:?}", other),
        }
        assert_eq!(std::fs::read_to_string(&t.claude_md).unwrap(), "# Global Rules\n\nrule one\nrule two\n");
        assert!(outcomes(&conn).is_empty());
    }

    #[test]
    fn applied_run_commits_records_outcome_and_advances_watermark() {
        let (s, t, conn) = setup("apply");
        let llm = FakeLlm {
            writes: vec![
                (".claude/skills/no-debug-logging/SKILL.md".into(), "---\nname: no-debug-logging\ndescription: Use when debugging.\n---\nDo not add logging unasked.\n".into()),
                (".claude/CLAUDE.md".into(), "# Global Rules\n\nrule one\nrule two\nrule three\n".into()),
            ],
            reply: "done\n\nIMPROVE-RESULT\naction: create\nfiles: whatever\nrationale: repeated rejections\nevidence: m1, m2\n".into(),
        };
        let vcs = FakeVcs::managing(&t);
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Applied { result, sha }) = run else { panic!("{:?}", run) };
        assert_eq!(sha, "abc1234");
        assert_eq!(result.files.len(), 2, "file list comes from the diff, not the reply");
        assert_eq!(vcs.added.borrow().len(), 2);
        let commits = vcs.commits.borrow();
        assert!(commits[0].starts_with("improve: create "), "{}", commits[0]);
        assert!(commits[0].contains("no-debug-logging/SKILL.md"));
        assert!(commits[0].contains("CLAUDE.md"));
        // The commit call is scoped to exactly the four write targets --
        // never the changed-file list (which would still be safe here, but
        // would NOT be if a stray change sat outside the targets) and never
        // "whatever else happens to be dirty in the source repo".
        let commit_targets = vcs.commit_targets.borrow();
        assert_eq!(commit_targets.len(), 1);
        let mut got: Vec<PathBuf> = commit_targets[0].clone();
        got.sort();
        let mut want: Vec<PathBuf> = t.roots().iter().map(|p| p.to_path_buf()).collect();
        want.sort();
        assert_eq!(got, want, "commit must be pathspec-scoped to exactly the write targets");

        let o = outcomes(&conn);
        assert_eq!(o.len(), 1);
        assert!(o[0].source.as_deref().unwrap().ends_with(" abc1234"));
        let st = store::get_improve_state(&conn).unwrap();
        assert_eq!(st.last_memory_id, Some(o[0].id), "watermark sits past the outcome memory");
        assert!(!s.0.join("snap").join("2026-09-08T12-00-00Z").exists(), "snapshot cleaned up");

        // next run sees nothing new: the outcome memory is history, not signal
        let (nm, rels) = improve_signal(&conn, &st).unwrap();
        assert!(nm.is_empty() && rels.is_empty());
    }

    #[test]
    fn none_with_changes_rolls_back_and_records_failure() {
        let (s, t, conn) = setup("none");
        let llm = FakeLlm {
            writes: vec![(".claude/CLAUDE.md".into(), "tampered".into())],
            reply: "IMPROVE-RESULT\naction: none\nfiles: none\nrationale: nothing\nevidence: none\n".into(),
        };
        let vcs = FakeVcs::managing(&t);
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("reported no action but changed"), "{}", reason);
        assert_eq!(std::fs::read_to_string(&t.claude_md).unwrap(), "# Global Rules\n\nrule one\nrule two\n", "restored");
        assert!(vcs.commits.borrow().is_empty());
        assert!(improve::is_failed_outcome(&outcomes(&conn)[0]));
        assert!(store::get_improve_state(&conn).unwrap().last_memory_id.is_none(), "watermark not advanced");
    }

    #[test]
    fn stray_managed_change_outside_targets_is_forced_back_and_fails() {
        let (s, t, conn) = setup("stray");
        let stray = s.0.join("home/.bashrc");
        let llm = FakeLlm {
            writes: vec![(".claude/CLAUDE.md".into(), "# Global Rules\n\nrule one\nrule two\nrule three\n".into())],
            reply: "IMPROVE-RESULT\naction: edit\nfiles: x\nrationale: r\nevidence: m1\n".into(),
        };
        let vcs = FakeVcs { status_after: vec![stray.clone()], ..FakeVcs::managing(&t) };
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("outside the write targets"), "{}", reason);
        assert_eq!(vcs.forced.borrow().as_slice(), &[stray]);
        assert_eq!(std::fs::read_to_string(&t.claude_md).unwrap(), "# Global Rules\n\nrule one\nrule two\n");
    }

    #[test]
    fn verification_failure_rolls_back() {
        let (s, t, conn) = setup("verify");
        let llm = FakeLlm {
            writes: vec![(".config/claude-hooks/new.sh".into(), "#!/usr/bin/env bash\nset -e\nexit 0\n".into())],
            reply: "IMPROVE-RESULT\naction: create\nfiles: x\nrationale: r\nevidence: m1\n".into(),
        };
        let vcs = FakeVcs::managing(&t);
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("set -e"), "{}", reason);
        assert!(!t.hooks_dir.join("new.sh").exists());
    }

    #[test]
    fn unmanaged_target_fails_before_any_call() {
        let (s, t, conn) = setup("unmanaged");
        let llm = FakeLlm { writes: vec![(".claude/CLAUDE.md".into(), "x".into())], reply: String::new() };
        let mut vcs = FakeVcs::managing(&t);
        vcs.managed.remove(&t.settings_json);
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("not chezmoi-managed"), "{}", reason);
        assert_eq!(std::fs::read_to_string(&t.claude_md).unwrap(), "# Global Rules\n\nrule one\nrule two\n", "llm never ran");
    }

    #[test]
    fn dirty_unrelated_source_path_does_not_block() {
        // A path the fake vcs was never told is dirty -- standing in for a
        // real chezmoi source repo with unrelated WIP elsewhere (e.g.
        // dot_config/mach) -- must not stop the run.
        let (s, t, conn) = setup("dirty-unrelated");
        let llm = FakeLlm {
            writes: vec![(".claude/CLAUDE.md".into(), "# Global Rules\n\nrule one\nrule two\nrule three\n".into())],
            reply: "IMPROVE-RESULT\naction: edit\nfiles: x\nrationale: r\nevidence: m1\n".into(),
        };
        let vcs = FakeVcs::managing(&t); // dirty: vec![] by default
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        assert!(matches!(run, ImproveRun::Done(improve::Outcome::Applied { .. })), "{:?}", run);
    }

    #[test]
    fn dirty_write_target_blocks_with_the_path_named() {
        let (s, t, conn) = setup("dirty-target");
        let llm = FakeLlm { writes: vec![], reply: String::new() };
        let vcs = FakeVcs { dirty: vec![t.claude_md.clone()], ..FakeVcs::managing(&t) };
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("uncommitted changes in write targets"), "{}", reason);
        assert!(reason.contains(&t.claude_md.display().to_string()), "{}", reason);
        assert!(!reason.contains(&t.skills_dir.display().to_string()), "only the dirty target is named: {}", reason);
        assert!(vcs.commits.borrow().is_empty(), "llm never ran");
    }

    // --- fix-list item 7: pre-run chezmoi drift (`pre_status`), distinct
    // --- from the git-dirty-source-repo guard above ---

    #[test]
    fn drift_unrelated_path_does_not_block() {
        // A path `chezmoi status` reports before the run, but that isn't one
        // of `improve`'s own write targets -- standing in for chezmoi
        // tracking some other file outside anything this pass ever touches
        // -- must not stop the run.
        let (s, t, conn) = setup("drift-unrelated");
        let llm = FakeLlm {
            writes: vec![(".claude/CLAUDE.md".into(), "# Global Rules\n\nrule one\nrule two\nrule three\n".into())],
            reply: "IMPROVE-RESULT\naction: edit\nfiles: x\nrationale: r\nevidence: m1\n".into(),
        };
        let vcs =
            FakeVcs { status_before: vec![PathBuf::from("/home/u/.config/mach/some-unrelated-file")], ..FakeVcs::managing(&t) };
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        assert!(matches!(run, ImproveRun::Done(improve::Outcome::Applied { .. })), "{:?}", run);
    }

    #[test]
    fn drift_write_target_blocks_with_the_path_named() {
        let (s, t, conn) = setup("drift-target");
        let llm = FakeLlm { writes: vec![], reply: String::new() };
        let vcs = FakeVcs { status_before: vec![t.claude_md.clone()], ..FakeVcs::managing(&t) };
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains("drifted from write targets before the run"), "{}", reason);
        assert!(reason.contains(&t.claude_md.display().to_string()), "{}", reason);
        assert!(!reason.contains(&t.skills_dir.display().to_string()), "only the drifted target is named: {}", reason);
        assert!(vcs.commits.borrow().is_empty(), "llm never ran");
    }

    #[test]
    fn drift_multiple_write_targets_are_all_named() {
        let (s, t, conn) = setup("drift-multi");
        let llm = FakeLlm { writes: vec![], reply: String::new() };
        let vcs =
            FakeVcs { status_before: vec![t.claude_md.clone(), t.skills_dir.clone()], ..FakeVcs::managing(&t) };
        let run = go(&conn, &llm, &vcs, &t, &s.0.join("snap"), false);
        let ImproveRun::Done(improve::Outcome::Failed { reason }) = run else { panic!("{:?}", run) };
        assert!(reason.contains(&t.claude_md.display().to_string()), "{}", reason);
        assert!(reason.contains(&t.skills_dir.display().to_string()), "{}", reason);
    }
}
