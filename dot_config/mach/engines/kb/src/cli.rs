
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
use crate::health;
use crate::improve::{self, ImproveLlm, Vcs};
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
    println!("  graph --stats           entity/edge/mention/card counts by kind — a compact view");
    println!("                          of the graph `mach kb reflect` has derived so far");
    println!("  graph audit             batched KEEP/POISONED/GENERIC judgment over every active");
    println!("                          edge; POISONED/GENERIC edges are invalidated (never deleted)");
    println!("  health [--notify]       operational self-check (ollama, kb.db, kb socket, reflect");
    println!("                          cadence, disk headroom, recall-log dir, telegram-state");
    println!("                          staleness); --notify sends one desktop alert on failure");
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
        Some("improve") => cmd_improve(args),
        Some("entity") => cmd_entity(args),
        Some("why") => cmd_why(args),
        Some("graph") => cmd_graph(args),
        Some("health") => cmd_health(args),
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
    // Normalized BM25 from the FTS5 lexical channel (`store::search_hybrid`):
    // 1.0 for the query's best exact-token match, omitted when 0 (no token
    // matched, or the hit came from insights/association where the channel
    // does not apply). `sim` stays the raw cosine; the blended `score`
    // already reflects whichever of the two was higher.
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

    for e in &hop1_edges {
        if out.len() >= SEARCH_MAX_CONNECTIONS {
            return out;
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
pub fn search_hits<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    query: &str,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<SearchResponse, KbError> {
    let q_emb = embedder.embed(query)?;
    // Hybrid: cosine over embeddings plus the FTS5 exact-token channel
    // (`store::search_hybrid`), so a NORAD number, hostname, or ticket name
    // the embedding blurs still ranks.
    let mem_hits = store::search_hybrid(conn, query, &q_emb, limit, reviewed_only, include_superseded, 0.0, now)?;
    let insight_hits = store::search_insights_ranked(conn, &q_emb, limit, now)?;
    let mut mem_hits: Vec<SearchHit> = mem_hits.into_iter().map(to_hit).collect();
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
    if min_score > 0.0 {
        hops.retain(|h| h.score >= min_score);
    }
    combined.extend(hops);
    let connections = entity_connections_for_query(conn, query, &q_emb);
    let cards = cards_for_query(conn, query, &q_emb);
    Ok(SearchResponse { hits: combined, connections, cards })
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
    // The shared merge (mem hits + insight hits + entity-connections
    // enrichment) lives in `search_hits`, so this is the exact same path
    // the kb socket daemon uses for `kb-recall.sh`'s fast path — only the
    // substring-fallback branch below is unique to this subprocess entry
    // point.
    let mut response = match search_hits(&conn, &embedder, &query, limit, reviewed_only, include_superseded, 0.0, &now) {
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
            SearchResponse { hits, connections: Vec::new(), cards: Vec::new() }
        }
    };

    if min_score > 0.0 {
        response.hits.retain(|h| h.score >= min_score);
    }

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
        run_graph_extraction_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
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
    let (evidence_dead, entities_merged, hygiene_llm_failed) = run_graph_hygiene_pass(&conn, &llm, &now).map_err(to_io)?;
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
    let (dormant, consolidated, dormancy_llm_failed) = run_dormancy_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || dormancy_llm_failed;

    // Step 4.66: entity cards — rebuild the consolidated profile of every
    // entity whose evidence has moved since its card was written. Runs after
    // merges (so a card is never built for an entity about to be merged
    // away) and after dormancy (so it reflects what actually survived), and
    // like those, every invocation regardless of has_new: staleness is
    // measured against the mention watermark, not this run's new material.
    let (cards_examined, cards_built, cards_llm_failed) =
        run_entity_card_pass(&conn, &embedder, &llm, &now).map_err(to_io)?;
    llm_failed = llm_failed || cards_llm_failed;

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

    // Records this run's completion for `mach kb health`'s own "is reflect
    // still running" check — unconditional (see
    // `store::ReflectState::last_completed_at`'s own doc comment for why
    // this is deliberately separate from the has_new/llm_failed-gated
    // watermark above).
    store::mark_reflect_completed(&conn, &now).map_err(to_io)?;

    println!(
        "mach kb reflect: examined={} questions={} insights_added={} reinforced={} \
         themes_added={} flagged={} verified={} curated={} promoted={} demoted={} dormant={} \
         consolidated={} deduped={} contradictions={} mem_verified={} mem_stale={} mem_routed={} \
         graph_examined={} graph_edges={} graph_entities={} evidence_dead={} entities_merged={} \
         cards_examined={} cards_built={}{}",
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
        graph_examined,
        graph_edges,
        graph_entities,
        evidence_dead,
        entities_merged,
        cards_examined,
        cards_built,
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
                // freeze the watermark.
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
fn resolve_or_create_entity<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    name: &str,
    kind: Option<&str>,
) -> Result<(i64, bool), KbError> {
    let name = name.trim();
    if let Some(existing) = store::find_entity_by_name(conn, name)? {
        return Ok((existing.id, false));
    }
    let embedding = embedder.embed(name).ok();
    if let Some(emb) = &embedding {
        if let Some((existing, _)) =
            store::find_entity_by_similarity(conn, emb, reflect::ENTITY_RESOLUTION_SIM_THRESHOLD)?
        {
            return Ok((existing.id, false));
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
    let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
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
/// (which gates the reflect watermark, same as every other sub-pass) but
/// never blocks marking a memory itself extracted — the call that row's own
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
    let candidates = store::graph_extraction_candidates(conn, reflect::GRAPH_EXTRACTION_MAX_PER_RUN)?;
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
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU_BATCH) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
                continue; // whole batch never marked extracted -- retried next run
            }
        };
        let parsed = reflect::parse_batch_extraction(&raw, chunk.len());

        for (i, m) in chunk.iter().enumerate() {
            let fact_num = i + 1;
            let triples = match parsed.get(&fact_num) {
                Some(t) => t,
                None => continue, // unaddressed -- not marked extracted, retried next run
            };
            examined += 1;

            // Edges from an unreviewed source memory get the same organic
            // confidence penalty `store::UNREVIEWED_SEARCH_PENALTY` applies
            // to search — see `reflect::GRAPH_EXTRACTION_UNREVIEWED_PENALTY`.
            let source_penalty = if m.reviewed { 1.0 } else { reflect::GRAPH_EXTRACTION_UNREVIEWED_PENALTY };

            for t in triples {
                let (src_id, src_new) = resolve_or_create_entity(conn, embedder, &t.src_name, t.src_kind.as_deref())?;
                let (dst_id, dst_new) = resolve_or_create_entity(conn, embedder, &t.dst_name, t.dst_kind.as_deref())?;
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
                let new_edge_id =
                    store::insert_relation(conn, src_id, &t.predicate, dst_id, Some(m.id), Some(confidence), now)?;
                edges_created += 1;

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
) -> Result<(usize, usize, bool), KbError> {
    let candidates = store::entity_card_candidates(conn, store::CARD_MAX_PER_RUN)?;
    let mut examined = 0usize;
    let mut built = 0usize;
    let mut llm_failed = false;

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
        let raw = match llm.call("haiku", &prompt, reflect::TIMEOUT_HAIKU) {
            Ok(out) => out,
            Err(_) => {
                llm_failed = true;
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

    Ok((examined, built, llm_failed))
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
            let cites = store::insights_citing(conn, id)?;
            if !cites.is_empty() {
                out.push_str("  cited by:\n");
                for i in &cites {
                    out.push_str(&format!(
                        "    {} #{} (confidence {:.2}{}) {}\n",
                        if i.level >= 2 { "theme" } else { "insight" },
                        i.id,
                        i.confidence,
                        if i.is_flagged() { ", DOUBTED" } else { "" },
                        truncate(&i.text, 90)
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
                    out.push_str(&format!("    {} —{}→ {}\n", name(e.src), e.predicate, name(e.dst)));
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
        }
    }
    Ok(out)
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
    let (kept, invalidated, invalidated_descs, llm_failed) = run_graph_audit(&conn, &llm, &now).map_err(to_io)?;

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
                        if store::insert_with_basis(
                            conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                        )
                        .is_ok()
                        {
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
                        if store::insert_with_basis(
                            conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                        )
                        .is_ok()
                        {
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
                        let verdicts = ingest::parse_engagement_verdicts(&out, &known_ids);
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
                            if store::insert_with_basis(
                                conn, &fact.content, Some("session-digest"), None, false, embedding.as_deref(), 5, fact.basis,
                            )
                            .is_ok()
                            {
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
        &llm,
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
    let recent_failed: Vec<bool> = outcomes
        .iter()
        .rev()
        .filter(|m| m.source.as_deref().map(|s| s.starts_with(improve::OUTCOME_SOURCE_PREFIX)).unwrap_or(false))
        .take(improve::HEALTH_FAIL_STREAK)
        .map(improve::is_failed_outcome)
        .collect();
    let ok = improve::health_ok(hours, &recent_failed);
    let streak = recent_failed.iter().filter(|f| **f).count();
    let detail = match hours {
        Some(h) => format!("last completed {:.1}h ago, {} recent failures", h, streak),
        None => "never completed".to_string(),
    };
    health::Check { name, ok, detail }
}

/// Checks that `machd`'s kb socket subsystem (`socket::run`) is up and
/// actually answers a `search` op — a real one-shot round trip over
/// `$XDG_RUNTIME_DIR/mach-kb.sock`, not just a file-exists check.
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
    match vcs.source_clean() {
        Ok(true) => {}
        Ok(false) => return fail("chezmoi source repo has uncommitted changes".to_string()),
        Err(e) => return fail(format!("git status: {}", e)),
    }
    let pre_status: HashSet<PathBuf> = match vcs.status() {
        Ok(v) => v.into_iter().collect(),
        Err(e) => return fail(format!("chezmoi status: {}", e)),
    };
    if let Some(p) = pre_status.iter().find(|p| targets.allows(p)) {
        return fail(format!("{} differs from its chezmoi source before the run (human edit in progress?)", p.display()));
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
    let sha = match vcs.commit(&improve::commit_message(&result)) {
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

        let llm = FixedReflectLlm { reply: Err("must never be called") };
        let (_examined, promoted, _demoted, failed) = run_curation_pass(&conn, &llm, &now).unwrap();
        assert_eq!(promoted, 1);
        assert!(!failed);
        let m = store::get(&conn, id).unwrap().unwrap();
        // touch() itself grows stability by 1.3x twice before the schema
        // multiplier applies on top -- 35.0 * 1.3 * 1.3 * 1.5.
        let expected = (35.0f64 * 1.3 * 1.3 * reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER).min(365.0);
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

    #[test]
    fn resolve_or_create_entity_reuses_an_exact_case_insensitive_name_match() {
        let conn = mem_conn();
        let existing = store::insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, "MOSES", Some("person")).unwrap();
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
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, "Moxes", Some("person")).unwrap();
        assert_eq!(id, existing, "a near-spelling must resolve to the same entity via similarity");
        assert!(!created);
    }

    #[test]
    fn resolve_or_create_entity_creates_a_new_entity_when_nothing_matches() {
        let conn = mem_conn();
        let (id, created) = resolve_or_create_entity(&conn, &FakeEmbedder, "Umoja", Some("project")).unwrap();
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
        let (examined, built, failed) = run_entity_card_pass(&conn, &FakeEmbedder, &llm, &now).unwrap();
        assert_eq!((examined, built, failed), (1, 1, false));

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
        let (examined, built, failed) = run_entity_card_pass(&conn, &FakeEmbedder, &none, &now).unwrap();
        assert_eq!((examined, built, failed), (1, 0, false));
        assert_eq!(store::get_entity_card(&conn, moses).unwrap().unwrap().text, "- the old card");

        let broken = FixedReflectLlm { reply: Err("offline") };
        let (examined, built, failed) = run_entity_card_pass(&conn, &FakeEmbedder, &broken, &now).unwrap();
        assert_eq!((examined, built), (1, 0));
        assert!(failed, "a failed call is reported so the caller can degrade");
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
        let resp = search_hits(&conn, &embedder, "what about Moses", 2, false, false, 0.3, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "what does Moses want", 5, false, false, 0.0, &now).unwrap();
        assert_eq!(resp.cards.len(), 1);
        assert_eq!(resp.cards[0].entity, "Moses");
        assert_eq!(resp.cards[0].kind.as_deref(), Some("person"));
        assert_eq!(resp.cards[0].evidence, 1);
        assert!(resp.cards[0].text.contains("hotfixes"));

        let none = search_hits(&conn, &embedder, "unrelated question about nothing", 5, false, false, 0.0, &now).unwrap();
        assert!(none.cards.is_empty());
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
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &store::now_rfc3339())
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
            search_hits(&conn, &embedder, "anything", 10, false, false, 0.0, &store::now_rfc3339()).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who wants ownership", 10, false, false, 0.3, &now).unwrap();
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
        assert_eq!(provenance_phrase(None, None), "you told me");
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
        let resp = search_hits(&conn, &embedder, "what runs on the site", 10, false, false, 0.0, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who is the user's boss", 10, false, false, 0.0, &now).unwrap();
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
        let resp = search_hits(&conn, &embedder, "who does Moses know", 10, false, false, 0.0, &now).unwrap();
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
        let llm = DigestCountingLlm { calls: std::cell::Cell::new(0), facts: "fact one\nfact two" };
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

        let before = search_hits(&conn, &e, "audit hub endpoints", 5, false, false, 0.5, &now).unwrap();
        assert!(before.hits.iter().all(|h| h.via_assoc.is_none()));
        assert!(!before.hits.iter().any(|h| h.id == strangler), "no edge yet: strangler is not near the query");

        store::reinforce_assoc(&conn, &[hub, strangler], &now).unwrap();
        store::reinforce_assoc(&conn, &[hub, strangler], &now).unwrap();
        let after = search_hits(&conn, &e, "audit hub endpoints", 5, false, false, 0.3, &now).unwrap();
        let assoc = after.hits.iter().find(|h| h.id == strangler).expect("strangler pulled in by association");
        assert_eq!(assoc.via_assoc, Some(hub));
        let src = after.hits.iter().find(|h| h.id == hub).unwrap();
        assert!(assoc.score < src.score, "associate never outranks its source");
        assert!(assoc.score > 0.3);
        assert!(!after.hits.iter().any(|h| h.content.contains("lonely")));
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
        status_after: Vec<PathBuf>,
        added: RefCell<Vec<PathBuf>>,
        forgotten: RefCell<Vec<PathBuf>>,
        forced: RefCell<Vec<PathBuf>>,
        commits: RefCell<Vec<String>>,
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
            // first call is the pre-run status (clean), later calls are post-run
            let mut c = self.calls.borrow_mut();
            *c += 1;
            Ok(if *c == 1 { Vec::new() } else { self.status_after.clone() })
        }
        fn source_clean(&self) -> Result<bool, String> {
            Ok(true)
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
        fn commit(&self, message: &str) -> Result<String, String> {
            self.commits.borrow_mut().push(message.to_string());
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
}
