
//! SQLite-backed store for the kb engine: schema, CRUD, ranked recall, and
//! save-time supersession bookkeeping. See the crate doc in `lib.rs` for why
//! cosine-over-BLOB was chosen over a `sqlite-vec` virtual table.
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use std::collections::HashMap;
use std::collections::HashSet;
use rusqlite::{params, Connection, OptionalExtension};

use crate::classify::Verdict;

#[derive(Debug)]
pub enum KbError {
    Db(rusqlite::Error),
    Io(std::io::Error),
    Embed(String),
    Other(String),
}

impl fmt::Display for KbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KbError::Db(e) => write!(f, "database error: {}", e),
            KbError::Io(e) => write!(f, "io error: {}", e),
            KbError::Embed(msg) => write!(f, "{}", msg),
            KbError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for KbError {}

impl From<rusqlite::Error> for KbError {
    fn from(e: rusqlite::Error) -> Self {
        KbError::Db(e)
    }
}

impl From<std::io::Error> for KbError {
    fn from(e: std::io::Error) -> Self {
        KbError::Io(e)
    }
}

impl From<serde_json::Error> for KbError {
    fn from(e: serde_json::Error) -> Self {
        KbError::Other(format!("json error: {}", e))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Memory {
    pub id: i64,
    pub content: String,
    pub source: Option<String>,
    pub project: Option<String>,
    pub created_at: String,
    pub reviewed: bool,
    pub embedding: Option<Vec<f32>>,
    pub importance: i64,
    // NULL is possible only on a pre-migration edge case this code never
    // produces itself (insert/migrate both always set it) — treated as
    // "derive from importance" wherever it's read, per the schema note.
    pub stability: Option<f64>,
    pub access_count: i64,
    pub first_accessed_at: Option<String>,
    pub last_accessed_at: Option<String>,
    pub valid_from: String,
    pub invalidated_at: Option<String>,
    pub superseded_by: Option<i64>,
    // Set by the nightly `mach kb reflect` dormancy pass (never by a user
    // action directly). Excluded from search/recall/reflection working sets
    // like a tombstoned row, but never deleted — `mach kb wake <id>` clears
    // it.
    pub dormant_at: Option<String>,
    // Bumped by the strength-review sampler (part of `mach kb reflect`,
    // alongside insight re-verification) when a sampled memory's evidence
    // still holds or is merely aging (STANDS/STALE) — mirrors
    // `Insight::last_verified_at`'s role for raw memories. NULL means never
    // sampled yet, which sorts first in `memories_due_for_strength_review`'s
    // `ORDER BY ... ASC`, same trick `insights_due_for_verification` uses.
    pub last_verified_at: Option<String>,
    // Set by `mach kb reflect`'s graph-extraction pass once this row has
    // been offered to the entity/relation extraction judge (whether or not
    // it actually yielded any triples) -- never set again after that, so a
    // memory is only ever offered once. NULL means "not yet extracted",
    // which is what `graph_extraction_candidates` filters on. A failed LLM
    // call must never set this (see that pass's own doc comment) -- same
    // "never mark seen on failure" house rule as `dedupe_seen`/
    // `contradiction_seen`.
    pub graph_extracted_at: Option<String>,
    // How this memory came to be known -- Honcho's explicit/deductive split
    // plus Hindsight's experience network as a third value.
    // `Some("stated")`: the user (or a named person) said it in so many
    // words: `mach kb add`, `mach note`, a decision cue, a digest line the
    // model tagged STATED. `Some("inferred")`: deduced from behavior, code,
    // or context (a digest line tagged INFERRED). `None`: written before the
    // column existed, or by a channel that does not classify (meeting
    // facts, consolidation) -- renderers fall back to source-only phrasing.
    // Never affects ranking; it exists so recall can say "you told me" vs
    // "I inferred" honestly.
    pub basis: Option<String>,
    // When the fact's CONTENT happened, as an inclusive date range
    // ("2026-09-07".."2026-09-08"), distinct from `created_at`, which is
    // when it was written down. NULL means unknown, never "same as
    // created_at": every memory here was written on one of a handful of
    // ingest days, so treating ingest time as event time would make every
    // temporal question answer "today".
    pub occurred_from: Option<String>,
    pub occurred_to: Option<String>,
    // Set by `mach kb restore` (never by anything automatic), cleared by
    // `mach kb unpin`. A pinned row is never tombstoned by an automatic
    // pass (dedupe Keep, contradiction Conflict/ConflictRetro, verdict
    // Update/Supersede) — see `is_pinned`/`unpin` and each of those
    // callers' own doc comments. Exported/imported like any other column
    // (`export::MemoryRow`) so a backup/restore round-trip can't silently
    // drop a pin and reopen the re-tombstoning bug it exists to prevent.
    pub pinned_at: Option<String>,
}

impl Memory {
    /// `stability`, falling back to the importance-derived default if the
    /// column somehow ended up NULL.
    pub fn effective_stability(&self) -> f64 {
        self.stability.unwrap_or(self.importance as f64 * 7.0)
    }

    pub fn is_superseded(&self) -> bool {
        self.invalidated_at.is_some()
    }

    pub fn is_dormant(&self) -> bool {
        self.dormant_at.is_some()
    }
}

/// A reflection-engine insight: a durable, higher-level belief derived
/// from `>= 2` independent memories, produced by `mach kb reflect` —
/// about the user, a project or system, an engineering practice, or a
/// recurring pattern, not the user alone. `source_ids` holds the
/// citations backing it, as strings — a plain memory id ("12") or an
/// insight reference ("i7") for an insight it explicitly builds on; only
/// raw memory ids count toward the evidence floor (see
/// `reflect::parse_stage2`).
#[derive(Debug, Clone, PartialEq)]
pub struct Insight {
    pub id: i64,
    pub text: String,
    pub created_at: String,
    pub confidence: f64,
    pub source_ids: Vec<String>,
    pub embedding: Option<Vec<f32>>,
    pub invalidated_at: Option<String>,
    pub flagged_at: Option<String>,
    pub last_verified_at: Option<String>,
    /// 1 = a plain insight derived from raw memories; 2 = a theme derived
    /// from >= 2 level-1 insights (a "meta-reflection" — see
    /// `cli::run_meta_pass`). The tower caps at two derived stories on top
    /// of the memory leaves, so no level higher than 2 is ever produced.
    pub level: i64,
    /// When `revise_insight` last rewrote `text` in place — `None` for an
    /// insight never revised. Set only by a `REVISE` verdict from
    /// re-verification's revise/drop/keep check (`reflect::parse_revise`,
    /// `cli::run_insight_stage`), schema v31.
    pub revised_at: Option<String>,
    /// The text `revise_insight` overwrote, one hop back — `None` for an
    /// insight never revised. Only the immediately-preceding wording is
    /// kept (a second revision overwrites this with the text it just
    /// replaced, not appended), same one-hop-back convention as
    /// `Memory::superseded_by`'s predecessor chain. Schema v31.
    pub prev_text: Option<String>,
}

impl Insight {
    pub fn is_active(&self) -> bool {
        self.invalidated_at.is_none()
    }

    pub fn is_flagged(&self) -> bool {
        self.flagged_at.is_some()
    }

    pub fn is_theme(&self) -> bool {
        self.level >= 2
    }
}

/// The single-row `reflect_state` watermark: how far `mach kb reflect` has
/// gotten through the memories table, and when it last ran.
#[derive(Debug, Clone, Default)]
pub struct ReflectState {
    pub last_run_at: Option<String>,
    pub last_memory_id: Option<i64>,
    // Set by every `mach kb reflect` invocation that actually completes
    // (including a cheap "nothing new" early exit) -- but NOT by the
    // offline-defer early exit, which means reflection genuinely didn't
    // happen this time. Distinct from `last_run_at`, which only advances
    // when there was new material AND nothing failed (see
    // `should_advance_watermark`): this field exists purely so `mach kb
    // health` can answer "is the reflect pass still running periodically at
    // all," independent of whether it's had anything to do lately.
    pub last_completed_at: Option<String>,
}

/// Watermarks for `mach kb improve`, same shape and semantics as
/// `ReflectState`: `last_run_at`/`last_memory_id`/`last_relation_id` move
/// together only when a run completed with the LLM call succeeding;
/// `last_completed_at` is set by every invocation that reaches its own end
/// (including the cheap below-threshold exit) so `mach kb health` can tell
/// "not running" from "nothing to do".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImproveState {
    pub last_run_at: Option<String>,
    pub last_memory_id: Option<i64>,
    pub last_relation_id: Option<i64>,
    pub last_completed_at: Option<String>,
}

/// `~/.local/share/mach/kb.db`, the default store location.
pub fn db_path() -> Result<PathBuf, KbError> {
    let home = env::var("HOME").map_err(|_| KbError::Other("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".local/share/mach/kb.db"))
}

fn init_schema(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT,
            project TEXT,
            created_at TEXT NOT NULL,
            reviewed INTEGER NOT NULL DEFAULT 1,
            embedding BLOB,
            importance INTEGER NOT NULL DEFAULT 5,
            stability REAL,
            access_count INTEGER NOT NULL DEFAULT 0,
            first_accessed_at TEXT,
            last_accessed_at TEXT,
            valid_from TEXT,
            invalidated_at TEXT,
            superseded_by INTEGER,
            dormant_at TEXT,
            last_verified_at TEXT,
            graph_extracted_at TEXT,
            basis TEXT,
            occurred_from TEXT,
            occurred_to TEXT
        );
        CREATE TABLE IF NOT EXISTS insights (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 0.5,
            source_ids TEXT NOT NULL,
            embedding BLOB,
            invalidated_at TEXT,
            flagged_at TEXT,
            last_verified_at TEXT,
            level INTEGER NOT NULL DEFAULT 1,
            revised_at TEXT,
            prev_text TEXT
        );
        CREATE TABLE IF NOT EXISTS reflect_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            last_run_at TEXT,
            last_memory_id INTEGER,
            last_completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS dedupe_seen (
            id_a INTEGER NOT NULL,
            id_b INTEGER NOT NULL,
            PRIMARY KEY (id_a, id_b)
        );
        CREATE TABLE IF NOT EXISTS contradiction_seen (
            id_a INTEGER NOT NULL,
            id_b INTEGER NOT NULL,
            PRIMARY KEY (id_a, id_b)
        );
        CREATE TABLE IF NOT EXISTS entity_merge_seen (
            id_a INTEGER NOT NULL,
            id_b INTEGER NOT NULL,
            PRIMARY KEY (id_a, id_b)
        );
        CREATE TABLE IF NOT EXISTS ingested_sessions (
            session_id TEXT PRIMARY KEY,
            ingested_at TEXT NOT NULL,
            skill_usage TEXT
        );
        CREATE TABLE IF NOT EXISTS session_progress (
            session_id TEXT PRIMARY KEY,
            last_line INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_assoc (
            a INTEGER NOT NULL,
            b INTEGER NOT NULL,
            count INTEGER NOT NULL,
            first_at TEXT NOT NULL,
            last_at TEXT NOT NULL,
            PRIMARY KEY (a, b)
        );
        CREATE TABLE IF NOT EXISTS improve_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            last_run_at TEXT,
            last_memory_id INTEGER,
            last_relation_id INTEGER,
            last_completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS relation_evidence (
            relation_id INTEGER NOT NULL,
            memory_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (relation_id, memory_id)
        );
        CREATE INDEX IF NOT EXISTS ix_relation_evidence_memory ON relation_evidence (memory_id);
        CREATE TABLE IF NOT EXISTS insight_dedupe_seen (
            id_a INTEGER NOT NULL,
            id_b INTEGER NOT NULL,
            PRIMARY KEY (id_a, id_b)
        );
        CREATE TABLE IF NOT EXISTS entity_cards (
            entity_id INTEGER PRIMARY KEY,
            text TEXT NOT NULL,
            source_ids TEXT NOT NULL,
            embedding BLOB,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            built_watermark INTEGER NOT NULL,
            mention_count INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_entities (
            memory_id INTEGER NOT NULL,
            entity_id INTEGER NOT NULL,
            source TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (memory_id, entity_id)
        );
        CREATE INDEX IF NOT EXISTS ix_memory_entities_entity ON memory_entities (entity_id);
        CREATE TABLE IF NOT EXISTS entities (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            kind TEXT,
            embedding BLOB,
            created_at TEXT NOT NULL
        );
        CREATE UNIQUE INDEX IF NOT EXISTS idx_entities_name_nocase ON entities (name COLLATE NOCASE);
        CREATE TABLE IF NOT EXISTS relations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            src INTEGER NOT NULL REFERENCES entities(id),
            predicate TEXT NOT NULL,
            dst INTEGER NOT NULL REFERENCES entities(id),
            evidence_memory_id INTEGER REFERENCES memories(id),
            confidence REAL,
            created_at TEXT NOT NULL,
            valid_from TEXT,
            invalidated_at TEXT,
            superseded_by INTEGER
        );",
    )?;
    ensure_fts(conn)?;
    Ok(())
}

/// FTS5 external-content index over `memories.content`, kept in step by
/// triggers. `content='memories'` means the index stores no copy of the
/// text -- it reads the row back from `memories` by rowid -- so it costs
/// only the inverted index. Tokenizer `porter unicode61` folds case, splits
/// on punctuation and stems: "10.8.0.63" indexes as four numeric tokens,
/// "react-app" as two words, a NORAD id "43005" as itself, and
/// "reported"/"reporting"/"reports" all as one term -- exactly the
/// exact-token matches cosine over an embedding blurs. Stemming matters
/// more in a personal bank than in web search: the bank is small, so a
/// query and the one memory answering it often differ only in inflection
/// ("who reported the drift bug" vs "Ivar ... reporting bugs ... drift"). Idempotent (`IF NOT EXISTS`
/// everywhere); the v14 -> v15 migration additionally issues a 'rebuild'
/// so rows written before the index existed are indexed.
///
/// `memories_fts` is derived state: never exported, never imported, always
/// rebuildable with `INSERT INTO memories_fts(memories_fts) VALUES('rebuild')`.
fn ensure_fts(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content,
            content='memories',
            content_rowid='id',
            tokenize='porter unicode61 remove_diacritics 2'
        );
        CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.id, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE OF content ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content) VALUES ('delete', old.id, old.content);
            INSERT INTO memories_fts(rowid, content) VALUES (new.id, new.content);
        END;",
    )?;
    Ok(())
}

/// Columns added by the v0 -> v1 migration, with the declaration used for
/// `ALTER TABLE ADD COLUMN` on a database that predates them.
const NEW_COLUMNS: &[(&str, &str)] = &[
    ("importance", "INTEGER NOT NULL DEFAULT 5"),
    ("stability", "REAL"),
    ("access_count", "INTEGER NOT NULL DEFAULT 0"),
    ("first_accessed_at", "TEXT"),
    ("last_accessed_at", "TEXT"),
    ("valid_from", "TEXT"),
    ("invalidated_at", "TEXT"),
    ("superseded_by", "INTEGER"),
];

/// `ALTER TABLE <table> ADD COLUMN <col> <decl>`, treating "the column is
/// already there" as success. Idempotent by construction, so it is safe
/// when two processes migrate the same file at once (machd starting up
/// while a `mach kb` CLI call from a hook opens the store): the check-then-
/// ALTER pattern this replaces lost that race with "duplicate column name"
/// and killed the daemon's kb thread. Any other error is still an error.
fn add_column_if_missing(conn: &Connection, table: &str, col: &str, decl: &str) -> Result<(), KbError> {
    match conn.execute(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, col, decl), []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn existing_columns(conn: &Connection) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// `PRAGMA user_version`-gated, idempotent 0 -> 1 migration: adds the new
/// reinforcement/supersession columns to a pre-existing `memories` table
/// (a no-op ALTER-wise on a table `init_schema` just created fresh, since
/// those columns are already there) and backfills `valid_from` and
/// `stability` on existing rows.
fn migrate_v0_to_v1(conn: &Connection) -> Result<(), KbError> {
    let cols = existing_columns(conn)?;
    for (name, decl) in NEW_COLUMNS {
        if !cols.iter().any(|c| c == name) {
            conn.execute(&format!("ALTER TABLE memories ADD COLUMN {} {}", name, decl), [])?;
        }
    }
    conn.execute_batch(
        "UPDATE memories SET valid_from = created_at WHERE valid_from IS NULL;
         UPDATE memories SET stability = importance * 7.0 WHERE stability IS NULL;",
    )?;
    conn.execute("PRAGMA user_version = 1", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 1 -> 2 migration: the reflection
/// subsystem's tables. `init_schema`'s `CREATE TABLE IF NOT EXISTS` already
/// creates `insights`/`reflect_state` on any database (fresh or pre-existing)
/// before `migrate` ever runs, so the only work left here is seeding the
/// `reflect_state` singleton row and bumping the version.
fn migrate_v1_to_v2(conn: &Connection) -> Result<(), KbError> {
    conn.execute(
        "INSERT OR IGNORE INTO reflect_state (id, last_run_at, last_memory_id) VALUES (1, NULL, NULL)",
        [],
    )?;
    conn.execute("PRAGMA user_version = 2", [])?;
    Ok(())
}

fn existing_insight_columns(conn: &Connection) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare("PRAGMA table_info(insights)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// `PRAGMA user_version`-gated, idempotent 2 -> 3 migration: adds the
/// meta-reflection level column to `insights` (level 1 = a plain insight
/// derived from raw memories, level 2 = a theme derived from >= 2 level-1
/// insights — the tower caps at three levels total counting the memory
/// leaves). `init_schema`'s `CREATE TABLE IF NOT EXISTS` already creates a
/// fresh `insights` table with this column present, so the only real work
/// here is backfilling a pre-existing table that predates it — every
/// existing row is, by definition, a plain (level 1) insight, which is
/// exactly the column's default.
fn migrate_v2_to_v3(conn: &Connection) -> Result<(), KbError> {
    let cols = existing_insight_columns(conn)?;
    if !cols.iter().any(|c| c == "level") {
        conn.execute("ALTER TABLE insights ADD COLUMN level INTEGER NOT NULL DEFAULT 1", [])?;
    }
    conn.execute("PRAGMA user_version = 3", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 3 -> 4 migration: adds the
/// dormancy column to `memories` for the nightly `mach kb reflect`
/// forgetting pass. `init_schema`'s `CREATE TABLE IF NOT EXISTS` already
/// creates a fresh `memories` table with this column present, so the only
/// real work here is backfilling a pre-existing table that predates it —
/// every existing row is, by definition, active (not dormant), which is
/// exactly the column's implicit default (NULL).
fn migrate_v3_to_v4(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "dormant_at", "TEXT")?;
    conn.execute("PRAGMA user_version = 4", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 4 -> 5 migration: the nightly
/// dedupe pass's `dedupe_seen` table. `init_schema`'s `CREATE TABLE IF NOT
/// EXISTS` already creates it on any database (fresh or pre-existing)
/// before `migrate` ever runs, so the only work left here is bumping the
/// version.
fn migrate_v4_to_v5(conn: &Connection) -> Result<(), KbError> {
    conn.execute("PRAGMA user_version = 5", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 5 -> 6 migration: adds
/// `memories.last_verified_at` (for the strength-review sampler's own
/// re-verification of raw memories — see `Memory::last_verified_at`) and the
/// contradiction patrol's `contradiction_seen` table. `init_schema`'s
/// `CREATE TABLE IF NOT EXISTS` already creates `contradiction_seen` and a
/// fresh `memories` table with the column present, so the only real work
/// here is backfilling a pre-existing `memories` table that predates the
/// column — every existing row is, by definition, never-verified, which is
/// exactly the column's implicit default (NULL) — and bumping the version.
fn migrate_v5_to_v6(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "last_verified_at", "TEXT")?;
    conn.execute("PRAGMA user_version = 6", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 6 -> 7 migration: rebuilds
/// `memories` and `insights` with `INTEGER PRIMARY KEY AUTOINCREMENT` in
/// place of a plain `INTEGER PRIMARY KEY`.
///
/// A plain `INTEGER PRIMARY KEY` is just an alias for SQLite's own ROWID,
/// which gets reused after a hard delete (`mach kb forget` /
/// `insight-forget`) once the table's max-id row is the one removed: the
/// next insert picks the smallest unused id, not `max(id) + 1`. Every
/// watermark in the reflection subsystem is an "id > last seen id"
/// comparison — `reflect_state.last_memory_id` against `memories`
/// (`memories_since`), and a theme's own id against `insights` via
/// `count_active_level1_created_after` — so a reused id lands *below* the
/// watermark and is silently invisible to every reflection pass forever.
/// `AUTOINCREMENT` fixes this: SQLite tracks the highest ROWID a table has
/// EVER held in the `sqlite_sequence` table, so the next insert always
/// exceeds it regardless of what has been deleted since.
///
/// SQLite has no `ALTER TABLE ... ADD AUTOINCREMENT`, so this is a full
/// rebuild, done the standard SQLite way: a new table with the column
/// declared correctly, copy every row across preserving its existing id,
/// drop the old table, rename the new one into place — all inside one
/// transaction so a database is never left caught between the two shapes.
/// `sqlite_sequence` ends up seeded from the highest id actually copied
/// (SQLite maintains that row itself as data is inserted, even when the id
/// is explicit rather than auto-assigned), so it starts at least as high as
/// any id either table has ever issued.
///
/// Also purges any `dedupe_seen`/`contradiction_seen` row citing a memory
/// id no longer present in `memories`: before this migration, a forgotten
/// (and, under the reused-rowid bug, possibly since-reissued) id could sit
/// in one of those tables and cause a future pair built from the reissued
/// id to be wrongly treated as "already judged" by the nightly dedupe or
/// contradiction pass.
/// `PRAGMA user_version`-gated, idempotent 7 -> 8 migration: the engagement-
/// gated reinforcement pass's `ingested_sessions` table (`mach kb
/// ingest-sessions`'s processed-set — see `is_session_ingested`/
/// `mark_session_ingested`). `init_schema`'s `CREATE TABLE IF NOT EXISTS`
/// already creates it on any database (fresh or pre-existing) before
/// `migrate` ever runs, so the only work left here is bumping the version.
fn migrate_v7_to_v8(conn: &Connection) -> Result<(), KbError> {
    conn.execute("PRAGMA user_version = 8", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 8 -> 9 migration: the
/// entities/relations graph layer. `init_schema`'s `CREATE TABLE IF NOT
/// EXISTS` already creates `entities` and `relations` (plus the
/// case-insensitive unique index on `entities.name`) on any database (fresh
/// or pre-existing) before `migrate` ever runs, and a fresh `memories` table
/// already has `graph_extracted_at`, so the only real work here is adding
/// that column to a pre-existing `memories` table that predates it — every
/// existing row is, by definition, not yet offered to the graph-extraction
/// pass, which is exactly the column's implicit default (NULL) — and
/// bumping the version.
fn migrate_v8_to_v9(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "graph_extracted_at", "TEXT")?;
    conn.execute("PRAGMA user_version = 9", [])?;
    Ok(())
}

fn existing_reflect_state_columns(conn: &Connection) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare("PRAGMA table_info(reflect_state)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// `PRAGMA user_version`-gated, idempotent 9 -> 10 migration: the graph
/// hygiene pass's `entity_merge_seen` table (`init_schema`'s `CREATE TABLE
/// IF NOT EXISTS` already creates it on any database, fresh or pre-existing,
/// before `migrate` ever runs) and `reflect_state.last_completed_at` (for
/// `mach kb health`'s "is reflect still running" check) — a fresh
/// `reflect_state` table already has the column, so the only real work here
/// is adding it to a pre-existing one that predates it; every existing row
/// simply has never recorded a completion yet, exactly the column's
/// implicit default (NULL).
fn migrate_v9_to_v10(conn: &Connection) -> Result<(), KbError> {
    let cols = existing_reflect_state_columns(conn)?;
    if !cols.iter().any(|c| c == "last_completed_at") {
        conn.execute("ALTER TABLE reflect_state ADD COLUMN last_completed_at TEXT", [])?;
    }
    conn.execute("PRAGMA user_version = 10", [])?;
    Ok(())
}

fn existing_ingested_sessions_columns(conn: &Connection) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare("PRAGMA table_info(ingested_sessions)")?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(cols)
}

/// `PRAGMA user_version`-gated, idempotent 10 -> 11 migration for `mach kb
/// improve`: seeds the `improve_state` singleton row (the table itself is
/// created by `init_schema`) and adds `ingested_sessions.skill_usage`, the
/// per-session skill-invocation/correction counts `ingest-sessions` records
/// so the improve pass can see whether an existing skill is working. A
/// fresh table already has the column; sessions ingested before this
/// migration simply have no usage recorded (NULL).
fn migrate_v10_to_v11(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS improve_state (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            last_run_at TEXT,
            last_memory_id INTEGER,
            last_relation_id INTEGER,
            last_completed_at TEXT
        );
        CREATE TABLE IF NOT EXISTS ingested_sessions (
            session_id TEXT PRIMARY KEY,
            ingested_at TEXT NOT NULL
        );",
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO improve_state (id, last_run_at, last_memory_id, last_relation_id) VALUES (1, NULL, NULL, NULL)",
        [],
    )?;
    let cols = existing_ingested_sessions_columns(conn)?;
    if !cols.iter().any(|c| c == "skill_usage") {
        conn.execute("ALTER TABLE ingested_sessions ADD COLUMN skill_usage TEXT", [])?;
    }
    conn.execute("PRAGMA user_version = 11", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 11 -> 12 migration: the
/// `session_progress` table behind `mach kb ingest-sessions --partial`
/// (mid-session checkpoint digests). Pure `CREATE TABLE IF NOT EXISTS`, so
/// it is safe on both a hand-built old database and a fresh one where
/// `init_schema` already made it.
fn migrate_v11_to_v12(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_progress (
            session_id TEXT PRIMARY KEY,
            last_line INTEGER NOT NULL,
            updated_at TEXT NOT NULL
        );",
    )?;
    conn.execute("PRAGMA user_version = 12", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 12 -> 13 migration: the
/// `memory_assoc` table -- Hebbian memory-to-memory association edges
/// (see `reinforce_assoc`). Pure `CREATE TABLE IF NOT EXISTS`.
fn migrate_v12_to_v13(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_assoc (
            a INTEGER NOT NULL,
            b INTEGER NOT NULL,
            count INTEGER NOT NULL,
            first_at TEXT NOT NULL,
            last_at TEXT NOT NULL,
            PRIMARY KEY (a, b)
        );",
    )?;
    conn.execute("PRAGMA user_version = 13", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 13 -> 14 migration: adds
/// `memories.basis` (see `Memory::basis`). A fresh table already has it;
/// every pre-existing row is left NULL -- "basis unknown" -- which is the
/// honest value for anything written before the split existed.
fn migrate_v13_to_v14(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "basis", "TEXT")?;
    conn.execute("PRAGMA user_version = 14", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 14 -> 15 migration: the FTS5
/// lexical index (`ensure_fts`, which `init_schema` already ran before
/// `migrate` got here) plus a one-off 'rebuild' so every row that predates
/// the index is indexed. A rebuild on an already-current index is a no-op
/// apart from the time it takes, so re-running is harmless. `ensure_fts` is
/// called again here because an earlier migration in the same run
/// (v6 -> v7's table rebuild) drops `memories` and with it the triggers.
fn migrate_v14_to_v15(conn: &Connection) -> Result<(), KbError> {
    ensure_fts(conn)?;
    conn.execute("INSERT INTO memories_fts(memories_fts) VALUES('rebuild')", [])?;
    conn.execute("PRAGMA user_version = 15", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 15 -> 16 migration: the
/// `memory_entities` mention table (created by `init_schema`) backfilled for
/// every existing row from two deterministic sources -- the entity graph's
/// own evidence pointers (a relation's evidence memory mentions both its
/// endpoints) and a word-boundary name scan of each memory against every
/// entity name (`name_mentioned`). No LLM. Re-running only re-inserts
/// already-present pairs (`INSERT OR IGNORE`).
fn migrate_v15_to_v16(conn: &Connection) -> Result<(), KbError> {
    let now = now_rfc3339();
    backfill_mentions(conn, &now)?;
    conn.execute("PRAGMA user_version = 16", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 16 -> 17 migration: the
/// `entity_cards` table (created by `init_schema`; nothing to backfill --
/// `mach kb reflect`'s card pass fills it in from mentions).
/// `PRAGMA user_version`-gated, idempotent 17 -> 18 migration: the FTS
/// index gains the `porter` stemmer. That changes how terms are stored, so
/// the index and its triggers are dropped and rebuilt rather than reused.
/// `PRAGMA user_version`-gated, idempotent 18 -> 19 migration: adds
/// `occurred_from`/`occurred_to` (when the fact's CONTENT happened, as
/// distinct from `created_at`, when it was written down) and backfills them
/// for existing rows by scanning the text for ISO dates. The distinction is
/// what makes a temporal question answerable: "what shipped on 2026-08-31"
/// is about an event, and every memory in the bank was written down on a
/// handful of ingest days.
/// `PRAGMA user_version`-gated, idempotent 19 -> 20 migration: the
/// `insight_dedupe_seen` table (created by `init_schema`), which records
/// insight pairs already judged so a DIFFERENT verdict is never re-paid for.
/// `PRAGMA user_version`-gated, idempotent 20 -> 21 migration: the
/// `relation_evidence` table, plus a fold of duplicate claim rows.
///
/// One relation row carried one evidence memory, so the same claim
/// extracted from two memories became two active rows. The bank held five
/// rows for "user works-on Helios". Now a claim is one row with N evidence
/// links (the same shape as `memory_entities`). The migration backfills
/// each existing row's own `evidence_memory_id` as its first link, then
/// folds duplicates: for each `(src, predicate, dst)` group the oldest row
/// survives, absorbs the others' evidence, and the rest are invalidated
/// with `superseded_by` pointing at the survivor -- never deleted, so the
/// audit trail holds.
fn migrate_v20_to_v21(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS relation_evidence (
            relation_id INTEGER NOT NULL,
            memory_id INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (relation_id, memory_id)
        );
        CREATE INDEX IF NOT EXISTS ix_relation_evidence_memory ON relation_evidence (memory_id);",
    )?;
    if !table_exists(conn, "relations")? {
        conn.execute("PRAGMA user_version = 21", [])?;
        return Ok(());
    }
    let now = now_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO relation_evidence (relation_id, memory_id, created_at)
         SELECT id, evidence_memory_id, ?1 FROM relations WHERE evidence_memory_id IS NOT NULL",
        params![now],
    )?;
    fold_duplicate_relations(conn, &now)?;
    conn.execute("PRAGMA user_version = 21", [])?;
    Ok(())
}

/// Schema 22: the raw-transcript index (see `crate::transcripts`).
///
/// Separate tables rather than more `memories`: a chunk is not a fact. It
/// has no basis, no strength, no decay and no provenance chain, it must
/// never reach the recall hook, and it is derived state -- droppable and
/// rebuildable from the transcripts on disk at any time.
fn migrate_v21_to_v22(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS transcript_files (
            path TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            mtime INTEGER NOT NULL,
            size INTEGER NOT NULL,
            chunks INTEGER NOT NULL,
            indexed_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS transcript_chunks (
            id INTEGER PRIMARY KEY,
            path TEXT NOT NULL,
            session_id TEXT NOT NULL,
            project TEXT NOT NULL,
            ts TEXT,
            text TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_transcript_chunks_path ON transcript_chunks (path);
        CREATE VIRTUAL TABLE IF NOT EXISTS transcript_chunks_fts USING fts5(
            text,
            content='transcript_chunks',
            content_rowid='id',
            tokenize='porter unicode61 remove_diacritics 2'
        );",
    )?;
    conn.execute("PRAGMA user_version = 22", [])?;
    Ok(())
}

/// Schema 23: embeddings on transcript passages, making transcript search
/// hybrid instead of lexical-only.
///
/// BM25 alone cannot answer a paraphrase. "Which clip space depth
/// convention was confirmed by hand" shares no token with the passage that
/// answers it (`GLM_FORCE_DEPTH_ZERO_TO_ONE`, `OriginIsTopLeft`), so the
/// eval-ask question for it failed even with the judge searching
/// transcripts twice. Memories have had both channels since the hybrid
/// work; passages were left on one.
fn migrate_v22_to_v23(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "transcript_chunks", "embedding", "BLOB")?;
    conn.execute("PRAGMA user_version = 23", [])?;
    Ok(())
}

/// One indexed transcript passage.
#[derive(Debug, Clone)]
pub struct TranscriptHit {
    pub id: i64,
    pub session_id: String,
    pub project: String,
    pub ts: Option<String>,
    pub text: String,
    /// BM25 relevance, higher is better (SQLite returns it negated).
    pub score: f32,
}

/// Whether this file is already indexed at exactly this mtime and size.
/// Cheap enough to call per file across a 3274-file sweep; it is what makes
/// a re-index cost only what changed.
pub fn transcript_file_current(conn: &Connection, path: &str, mtime: i64, size: i64) -> Result<bool, KbError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM transcript_files WHERE path = ?1 AND mtime = ?2 AND size = ?3",
            params![path, mtime, size],
            |r| r.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// Replaces every chunk of one transcript, and records the file as indexed.
///
/// Delete-then-insert rather than an append: a session file grows as the
/// conversation continues, so re-indexing one must not leave the previous
/// pass's chunks behind as duplicates.
pub fn replace_transcript_chunks(
    conn: &Connection,
    path: &str,
    session_id: &str,
    project: &str,
    mtime: i64,
    size: i64,
    chunks: &[crate::transcripts::Chunk],
    embeddings: &[Option<Vec<f32>>],
    now: &str,
) -> Result<usize, KbError> {
    conn.execute(
        "INSERT INTO transcript_chunks_fts(transcript_chunks_fts, rowid, text)
         SELECT 'delete', id, text FROM transcript_chunks WHERE path = ?1",
        params![path],
    )?;
    conn.execute("DELETE FROM transcript_chunks WHERE path = ?1", params![path])?;
    {
        let mut stmt = conn.prepare(
            "INSERT INTO transcript_chunks (path, session_id, project, ts, text, embedding) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut fts = conn.prepare("INSERT INTO transcript_chunks_fts(rowid, text) VALUES (?1, ?2)")?;
        for (i, c) in chunks.iter().enumerate() {
            let emb = embeddings.get(i).and_then(|e| e.as_ref()).map(|v| encode_embedding(v));
            stmt.execute(params![path, session_id, project, c.ts, c.text, emb])?;
            fts.execute(params![conn.last_insert_rowid(), c.text])?;
        }
    }
    conn.execute(
        "INSERT INTO transcript_files (path, session_id, project, mtime, size, chunks, indexed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(path) DO UPDATE SET mtime=excluded.mtime, size=excluded.size,
             chunks=excluded.chunks, indexed_at=excluded.indexed_at",
        params![path, session_id, project, mtime, size, chunks.len() as i64, now],
    )?;
    Ok(chunks.len())
}

/// BM25 search over indexed transcript passages.
///
/// Lexical only, deliberately: these are verbatim words, and the questions
/// that reach here are the ones the embedding layer already failed on.
pub fn search_transcripts(
    conn: &Connection,
    query: &str,
    q_emb: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<TranscriptHit>, KbError> {
    // `fts_query` returns None when the query holds nothing an FTS5 MATCH
    // can use (all punctuation, all stopwords) -- no query, no hits.
    // Lexical channel: BM25 over the FTS index, normalized against this
    // result set's own best match so it shares a scale with cosine.
    let mut scored: HashMap<i64, (TranscriptHit, f32, f32)> = HashMap::new();
    if let Some(cleaned) = fts_query(query) {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.session_id, c.project, c.ts, c.text, bm25(transcript_chunks_fts)
             FROM transcript_chunks_fts f JOIN transcript_chunks c ON c.id = f.rowid
             WHERE transcript_chunks_fts MATCH ?1 ORDER BY bm25(transcript_chunks_fts) LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![cleaned, (limit * 4) as i64], |r| {
            Ok((
                TranscriptHit {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    ts: r.get(3)?,
                    text: r.get(4)?,
                    score: 0.0,
                },
                -r.get::<_, f64>(5)? as f32,
            ))
        })?;
        let mut raw = Vec::new();
        for r in rows {
            raw.push(r?);
        }
        let best = raw.iter().map(|(_, b)| *b).fold(0.0f32, f32::max);
        for (hit, bm) in raw {
            let lex = if best > 0.0 { bm / best } else { 0.0 };
            scored.insert(hit.id, (hit, lex, 0.0));
        }
    }

    // Semantic channel: cosine over passage embeddings. A linear scan, same
    // rationale as `search_ranked` -- at personal scale this is a few
    // milliseconds and needs no index to maintain.
    if let Some(q) = q_emb {
        let mut stmt = conn.prepare(
            "SELECT id, session_id, project, ts, text, embedding FROM transcript_chunks WHERE embedding IS NOT NULL",
        )?;
        let mut sims: Vec<(f32, TranscriptHit)> = Vec::new();
        let rows = stmt.query_map([], |r| {
            let blob: Vec<u8> = r.get(5)?;
            Ok((
                TranscriptHit {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    project: r.get(2)?,
                    ts: r.get(3)?,
                    text: r.get(4)?,
                    score: 0.0,
                },
                blob,
            ))
        })?;
        for r in rows {
            let (hit, blob) = r?;
            let v = decode_embedding(&blob);
            let sim = cosine(q, &v);
            if sim > SIM_NOISE_FLOOR {
                sims.push((sim, hit));
            }
        }
        sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (sim, hit) in sims.into_iter().take(limit * 4) {
            scored.entry(hit.id).and_modify(|e| e.2 = sim).or_insert((hit, 0.0, sim));
        }
    }

    // Same fusion as memories: the stronger channel wins, lexical held at
    // LEXICAL_WEIGHT so an exact-token hit outranks a merely similar one.
    let mut out: Vec<TranscriptHit> = scored
        .into_values()
        .map(|(mut hit, lex, sim)| {
            hit.score = sim.max(LEXICAL_WEIGHT * lex);
            hit
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}

// ---------------------------------------------------------------------
// Code index (schema 32, `migrate_v31_to_v32`; see `crate::code_index`).
//
// Same rationale as `transcript_chunks`/`transcript_chunks_fts` above: a
// chunk is derived state parsed out of a git blob, not a fact, so it lives
// in its own tables rather than `memories` and is droppable/rebuildable
// from the indexed repo at any time. `code_chunks_fts` is external-content
// (`content='code_chunks'`) and kept in sync the same way
// `transcript_chunks_fts` is -- explicit `INSERT ... 'delete'` + insert
// around every write, no triggers.
// ---------------------------------------------------------------------

/// One directory's category decision in `code_scope`. `source` is
/// `"llm"` (the scope pass's own guess) or `"user"` (a manual override,
/// which `code_scope_set` never lets an `"llm"` write clobber).
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeRow {
    pub project_id: i64,
    pub dir: String,
    pub category: String,
    pub source: String,
    pub decided_at: String,
}

/// One row of `code_files`: a project-relative path's indexing state.
/// `blob` is the git blob sha this row was indexed at (not the file
/// content); `status` is one of `indexed`/`dirty`/`fallback`/`skipped`.
/// `summary_attempts` (schema 34) counts unparseable-summary attempts for
/// THIS blob -- `code_file_upsert` resets it to 0 whenever `blob` changes,
/// so it always reflects "how many times has the current blob's summary
/// call succeeded but produced nothing parseable" (see
/// `code_file_increment_summary_attempts`, `code_index::job::run_summary_pass`).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeFileRow {
    pub project_id: i64,
    pub path: String,
    pub blob: String,
    pub lang: Option<String>,
    pub lines: i64,
    pub status: String,
    pub indexed_at: String,
    pub summary_attempts: i64,
}

/// A chunk to insert via `replace_code_chunks`. No `id` (SQLite assigns
/// it) and no `project_id`/`path` (given once for the whole file being
/// replaced) -- just the per-chunk fields the chunker (a later task)
/// produces.
#[derive(Debug, Clone, Default)]
pub struct NewCodeChunk {
    pub symbol: Option<String>,
    pub kind: String,
    pub scope: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub text: String,
    pub header: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub content_hash: String,
}

/// One ranked hit from `search_code_chunks`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeHit {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub symbol: Option<String>,
    pub kind: String,
    pub scope: Option<String>,
    pub start_line: i64,
    pub end_line: i64,
    pub text: String,
    pub header: Option<String>,
    /// Fused relevance, higher is better -- see `search_code_chunks`.
    pub score: f32,
}

/// Every scope decision recorded for a project, one row per directory.
pub fn code_scope_get(conn: &Connection, project_id: i64) -> Result<Vec<ScopeRow>, KbError> {
    let mut stmt = conn
        .prepare("SELECT project_id, dir, category, source, decided_at FROM code_scope WHERE project_id = ?1 ORDER BY dir")?;
    let rows = stmt.query_map(params![project_id], |r| {
        Ok(ScopeRow { project_id: r.get(0)?, dir: r.get(1)?, category: r.get(2)?, source: r.get(3)?, decided_at: r.get(4)? })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Records a directory's category decision. A `"user"` row is a manual
/// override and is never overwritten by a `source == "llm"` write -- the
/// scope pass re-runs periodically (see the design doc) and must not
/// silently undo a decision the user made by hand.
pub fn code_scope_set(conn: &Connection, project_id: i64, dir: &str, category: &str, source: &str, now: &str) -> Result<(), KbError> {
    let existing_source: Option<String> = conn
        .query_row("SELECT source FROM code_scope WHERE project_id = ?1 AND dir = ?2", params![project_id, dir], |r| r.get(0))
        .optional()?;
    if existing_source.as_deref() == Some("user") && source == "llm" {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO code_scope (project_id, dir, category, source, decided_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(project_id, dir) DO UPDATE SET
             category = excluded.category, source = excluded.source, decided_at = excluded.decided_at",
        params![project_id, dir, category, source, now],
    )?;
    Ok(())
}

fn row_to_code_file(r: &rusqlite::Row) -> rusqlite::Result<CodeFileRow> {
    Ok(CodeFileRow {
        project_id: r.get(0)?,
        path: r.get(1)?,
        blob: r.get(2)?,
        lang: r.get(3)?,
        lines: r.get(4)?,
        status: r.get(5)?,
        indexed_at: r.get(6)?,
        summary_attempts: r.get(7)?,
    })
}

const CODE_FILE_COLUMNS: &str = "project_id, path, blob, lang, lines, status, indexed_at, summary_attempts";

/// One file's indexing state, if it has ever been indexed.
pub fn code_file_get(conn: &Connection, project_id: i64, path: &str) -> Result<Option<CodeFileRow>, KbError> {
    Ok(conn
        .query_row(
            &format!("SELECT {CODE_FILE_COLUMNS} FROM code_files WHERE project_id = ?1 AND path = ?2"),
            params![project_id, path],
            row_to_code_file,
        )
        .optional()?)
}

/// Inserts or updates one file's indexing state. `summary_attempts` starts
/// at 0 for a brand-new row and is reset to 0 whenever the blob actually
/// changes (a new blob means the previous blob's give-up count is
/// irrelevant -- the file gets a fresh 2 tries at a summary for its new
/// content); an upsert that leaves the blob unchanged (e.g. `mark_skipped`
/// re-recording the same skip) preserves whatever count was already there.
pub fn code_file_upsert(
    conn: &Connection,
    project_id: i64,
    path: &str,
    blob: &str,
    lang: Option<&str>,
    lines: i64,
    status: &str,
    now: &str,
) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO code_files (project_id, path, blob, lang, lines, status, indexed_at, summary_attempts)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
         ON CONFLICT(project_id, path) DO UPDATE SET
             blob = excluded.blob, lang = excluded.lang, lines = excluded.lines,
             status = excluded.status, indexed_at = excluded.indexed_at,
             summary_attempts = CASE WHEN code_files.blob = excluded.blob THEN code_files.summary_attempts ELSE 0 END",
        params![project_id, path, blob, lang, lines, status, now],
    )?;
    Ok(())
}

/// Records one more unparseable-summary attempt for `path`'s CURRENT blob
/// (a combined or summary-only call that succeeded but produced no usable
/// `SUMMARY:` line) and returns the new count -- `code_index::job` gives up
/// queuing further summary-only calls for a file once this reaches
/// `job::SUMMARY_GIVE_UP_ATTEMPTS`. A no-op returning 0 if `path` has no
/// `code_files` row (defensive; every real caller already holds one).
pub fn code_file_increment_summary_attempts(conn: &Connection, project_id: i64, path: &str) -> Result<i64, KbError> {
    conn.execute(
        "UPDATE code_files SET summary_attempts = summary_attempts + 1 WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    Ok(conn
        .query_row(
            "SELECT summary_attempts FROM code_files WHERE project_id = ?1 AND path = ?2",
            params![project_id, path],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}

/// Drops a file's indexing state along with every chunk parsed from it
/// (kept in sync with `code_chunks_fts`) -- the store-layer half of "a
/// deleted file's chunks are removed" (the other half, noticing the
/// deletion, is the nightly job's diff).
pub fn code_file_delete(conn: &Connection, project_id: i64, path: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_chunks_fts(code_chunks_fts, rowid, header, text, symbol)
         SELECT 'delete', id, header, text, symbol FROM code_chunks WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.execute("DELETE FROM code_chunks WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
    tx.execute("DELETE FROM code_files WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
    tx.commit()?;
    Ok(())
}

/// Updates only a file's status (e.g. marking it `dirty` after a failed
/// header/embed pass, without disturbing `indexed_at` or the blob it was
/// last successfully indexed at).
pub fn code_file_set_status(conn: &Connection, project_id: i64, path: &str, status: &str) -> Result<(), KbError> {
    conn.execute("UPDATE code_files SET status = ?1 WHERE project_id = ?2 AND path = ?3", params![status, project_id, path])?;
    Ok(())
}

/// Every file of a project currently at a given status, e.g. `"dirty"` for
/// the nightly job's retry list.
pub fn code_files_with_status(conn: &Connection, project_id: i64, status: &str) -> Result<Vec<CodeFileRow>, KbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CODE_FILE_COLUMNS} FROM code_files WHERE project_id = ?1 AND status = ?2 ORDER BY path"
    ))?;
    let rows = stmt.query_map(params![project_id, status], row_to_code_file)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Replaces every chunk of one file and returns the new rows' ids, in the
/// same order as `chunks` -- a later header/embed pass addresses a chunk
/// by this id (see `set_code_chunk_header`, `set_code_chunk_embedding`).
/// Delete-then-insert in one transaction, same shape as
/// `replace_transcript_chunks`: a re-chunk (the file changed) must not
/// leave the previous pass's rows behind as duplicates.
pub fn replace_code_chunks(conn: &Connection, project_id: i64, path: &str, chunks: &[NewCodeChunk]) -> Result<Vec<i64>, KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_chunks_fts(code_chunks_fts, rowid, header, text, symbol)
         SELECT 'delete', id, header, text, symbol FROM code_chunks WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.execute("DELETE FROM code_chunks WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
    let mut ids = Vec::with_capacity(chunks.len());
    {
        let mut stmt = tx.prepare(
            "INSERT INTO code_chunks (project_id, path, symbol, kind, scope, start_line, end_line, text, header, embedding, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        )?;
        let mut fts = tx.prepare("INSERT INTO code_chunks_fts(rowid, header, text, symbol) VALUES (?1, ?2, ?3, ?4)")?;
        for c in chunks {
            let emb = c.embedding.as_ref().map(|v| encode_embedding(v));
            stmt.execute(params![
                project_id,
                path,
                c.symbol,
                c.kind,
                c.scope,
                c.start_line,
                c.end_line,
                c.text,
                c.header,
                emb,
                c.content_hash
            ])?;
            let id = tx.last_insert_rowid();
            fts.execute(params![id, c.header, c.text, c.symbol])?;
            ids.push(id);
        }
    }
    tx.commit()?;
    Ok(ids)
}

/// Attaches a contextual header to one already-inserted chunk, keeping
/// `code_chunks_fts` in step (header is an indexed column, so this is a
/// delete-old-row + insert-new-row around the external-content table, not
/// a plain `UPDATE`).
pub fn set_code_chunk_header(conn: &Connection, id: i64, header: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_chunks_fts(code_chunks_fts, rowid, header, text, symbol)
         SELECT 'delete', id, header, text, symbol FROM code_chunks WHERE id = ?1",
        params![id],
    )?;
    tx.execute("UPDATE code_chunks SET header = ?1 WHERE id = ?2", params![header, id])?;
    tx.execute(
        "INSERT INTO code_chunks_fts(rowid, header, text, symbol) SELECT id, header, text, symbol FROM code_chunks WHERE id = ?1",
        params![id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Attaches an embedding to one already-inserted chunk. Not an FTS
/// column, so a plain `UPDATE`.
pub fn set_code_chunk_embedding(conn: &Connection, id: i64, embedding: &[f32]) -> Result<(), KbError> {
    conn.execute("UPDATE code_chunks SET embedding = ?1 WHERE id = ?2", params![encode_embedding(embedding), id])?;
    Ok(())
}

/// Chunks still waiting on an embedding, oldest-id-first, capped at
/// `limit` -- same shape and same reason as
/// `transcript_chunks_missing_embedding`: lets an embedder-down gap be
/// closed by a separate pass without a full re-chunk.
pub fn code_chunks_missing_embedding(conn: &Connection, limit: usize) -> Result<Vec<(i64, String)>, KbError> {
    let mut stmt = conn.prepare("SELECT id, text FROM code_chunks WHERE embedding IS NULL ORDER BY id ASC LIMIT ?1")?;
    let rows = stmt.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn row_to_code_hit(r: &rusqlite::Row) -> rusqlite::Result<CodeHit> {
    Ok(CodeHit {
        id: r.get(0)?,
        project_id: r.get(1)?,
        path: r.get(2)?,
        symbol: r.get(3)?,
        kind: r.get(4)?,
        scope: r.get(5)?,
        start_line: r.get(6)?,
        end_line: r.get(7)?,
        text: r.get(8)?,
        header: r.get(9)?,
        score: 0.0,
    })
}

const CODE_HIT_COLUMNS: &str = "id, project_id, path, symbol, kind, scope, start_line, end_line, text, header";

/// Hybrid search over indexed code chunks, optionally scoped to one
/// project (`None` searches every project).
///
/// Two channels, fused the same way `search_hybrid`'s `Fusion::Max`
/// branch and `search_transcripts` both fuse theirs: lexical and semantic
/// are scored independently, then combined as `max(sim, LEXICAL_WEIGHT *
/// lex)` -- the stronger channel wins, never summed, so a chunk that
/// matches both is not double-counted.
///
/// - **Lexical**: BM25 over `code_chunks_fts`, normalized against this
///   result set's own best match so it shares a scale with cosine (same
///   normalization `search_transcripts` uses). The `bm25()` column
///   weights are `(header=1.0, text=1.0, symbol=10.0)` -- heavily biased
///   toward the `symbol` column so a chunk whose *symbol name* matches the
///   query outranks one that merely mentions the same token in its body,
///   which is what makes an exact symbol-name query resolve to its
///   definition first.
/// - **Semantic**: cosine over `embedding`, floored at `SIM_NOISE_FLOOR`
///   (unrelated text still scores 0.4-0.55 under this embedder — see that
///   constant's doc comment), same brute-force linear scan as every other
///   cosine search in this store.
pub fn search_code_chunks(
    conn: &Connection,
    project_id: Option<i64>,
    query: &str,
    query_emb: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<CodeHit>, KbError> {
    let mut scored: HashMap<i64, (CodeHit, f32, f32)> = HashMap::new();

    if let Some(cleaned) = fts_query(query) {
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.project_id, c.path, c.symbol, c.kind, c.scope, c.start_line, c.end_line, c.text, c.header,
                    bm25(code_chunks_fts, 1.0, 1.0, 10.0)
             FROM code_chunks_fts f JOIN code_chunks c ON c.id = f.rowid
             WHERE code_chunks_fts MATCH ?1 AND (?2 IS NULL OR c.project_id = ?2)
             ORDER BY bm25(code_chunks_fts, 1.0, 1.0, 10.0) LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![cleaned, project_id, (limit * 4) as i64], |r| {
            Ok((row_to_code_hit(r)?, -r.get::<_, f64>(10)? as f32))
        })?;
        let mut raw = Vec::new();
        for r in rows {
            raw.push(r?);
        }
        let best = raw.iter().map(|(_, b)| *b).fold(0.0f32, f32::max);
        for (hit, bm) in raw {
            let lex = if best > 0.0 { bm / best } else { 0.0 };
            scored.insert(hit.id, (hit, lex, 0.0));
        }
    }

    if let Some(q) = query_emb {
        let mut stmt = conn.prepare(&format!(
            "SELECT {CODE_HIT_COLUMNS}, embedding FROM code_chunks WHERE embedding IS NOT NULL AND (?1 IS NULL OR project_id = ?1)"
        ))?;
        let rows = stmt.query_map(params![project_id], |r| Ok((row_to_code_hit(r)?, r.get::<_, Vec<u8>>(10)?)))?;
        let mut sims: Vec<(f32, CodeHit)> = Vec::new();
        for r in rows {
            let (hit, blob) = r?;
            let v = decode_embedding(&blob);
            let sim = cosine(q, &v);
            if sim > SIM_NOISE_FLOOR {
                sims.push((sim, hit));
            }
        }
        sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (sim, hit) in sims.into_iter().take(limit * 4) {
            scored.entry(hit.id).and_modify(|e| e.2 = sim).or_insert((hit, 0.0, sim));
        }
    }

    let mut out: Vec<CodeHit> = scored
        .into_values()
        .map(|(mut hit, lex, sim)| {
            hit.score = sim.max(LEXICAL_WEIGHT * lex);
            hit
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}

/// Records the commit a project's code index is now current with.
pub fn set_code_indexed_head(conn: &Connection, project_id: i64, sha: &str, now: &str) -> Result<(), KbError> {
    conn.execute("UPDATE projects SET code_indexed_head = ?1, code_indexed_at = ?2 WHERE id = ?3", params![sha, now, project_id])?;
    Ok(())
}

// ---------------------------------------------------------------------
// Code summaries (schema 33, `migrate_v32_to_v33`; see `crate::code_index`
// and the design doc's "Summaries (phase 2) — revised 2026-09-24" section).
//
// One row per (project_id, path) -- a file path for `level = "file"`,
// `"<dir>/"` for a module, `"/"` for the repo -- holding the LLM-produced
// summary text for that unit. Kept FTS-searchable the same explicit
// insert-and-delete way as `code_chunks_fts` above, off SQLite's implicit
// `rowid` rather than a declared id column (see `migrate_v32_to_v33`'s own
// doc comment for why that's safe here).
//
// Module and repo summaries are ALSO mirrored into `memories` as
// index-owned rows via `upsert_index_memory`, so plain session recall
// surfaces them; file summaries never are (see the design doc).
// ---------------------------------------------------------------------

/// One row of `code_summaries`.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeSummaryRow {
    pub project_id: i64,
    pub path: String,
    pub level: String,
    pub text: String,
    pub embedding: Option<Vec<f32>>,
    pub source_digest: String,
    pub stale: bool,
    pub stale_since: Option<String>,
    pub memory_id: Option<i64>,
    pub updated_at: String,
}

/// One ranked hit from `search_code_summaries`.
#[derive(Debug, Clone, PartialEq)]
pub struct SummaryHit {
    pub project_id: i64,
    pub path: String,
    pub level: String,
    pub text: String,
    pub stale: bool,
    /// Fused relevance, higher is better -- same fusion `search_code_chunks`
    /// uses.
    pub score: f32,
}

const CODE_SUMMARY_COLUMNS: &str =
    "project_id, path, level, text, embedding, source_digest, stale, stale_since, memory_id, updated_at";

fn row_to_code_summary(r: &rusqlite::Row) -> rusqlite::Result<CodeSummaryRow> {
    let blob: Option<Vec<u8>> = r.get(4)?;
    let stale_int: i64 = r.get(6)?;
    Ok(CodeSummaryRow {
        project_id: r.get(0)?,
        path: r.get(1)?,
        level: r.get(2)?,
        text: r.get(3)?,
        embedding: blob.map(|b| decode_embedding(&b)),
        source_digest: r.get(5)?,
        stale: stale_int != 0,
        stale_since: r.get(7)?,
        memory_id: r.get(8)?,
        updated_at: r.get(9)?,
    })
}

/// One project/path's summary, if it has ever been generated.
pub fn code_summary_get(conn: &Connection, project_id: i64, path: &str) -> Result<Option<CodeSummaryRow>, KbError> {
    Ok(conn
        .query_row(
            &format!("SELECT {CODE_SUMMARY_COLUMNS} FROM code_summaries WHERE project_id = ?1 AND path = ?2"),
            params![project_id, path],
            row_to_code_summary,
        )
        .optional()?)
}

/// Inserts or regenerates a file/module/repo summary, clearing `stale`
/// (`stale = 0`, `stale_since = NULL`) since fresh text just replaced
/// whatever was stale about the old one. Also clears any previous
/// `embedding` -- text and its embedding must never drift apart, so a
/// regenerated summary always needs a fresh embedding, which
/// `code_summaries_missing_embedding` then picks up the same way an
/// embedder-down gap is closed for `code_chunks`. `memory_id` is left
/// untouched: it is set separately, once the module/repo mirror memory
/// exists (see `set_code_summary_memory_id`, `upsert_index_memory`), not by
/// this call.
///
/// Delete-then-insert around `code_summaries_fts` in one transaction, same
/// external-content-table dance `replace_code_chunks`/`set_code_chunk_header`
/// use: the 'delete' command must run first, against whatever `text`/`path`
/// are still live in `code_summaries` before the upsert overwrites them.
pub fn code_summary_upsert(
    conn: &Connection,
    project_id: i64,
    path: &str,
    level: &str,
    text: &str,
    source_digest: &str,
    now: &str,
) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_summaries_fts(code_summaries_fts, rowid, text, path)
         SELECT 'delete', rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.execute(
        "INSERT INTO code_summaries (project_id, path, level, text, source_digest, stale, stale_since, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 0, NULL, ?6)
         ON CONFLICT(project_id, path) DO UPDATE SET
             level = excluded.level, text = excluded.text, source_digest = excluded.source_digest,
             embedding = NULL, stale = 0, stale_since = NULL, updated_at = excluded.updated_at",
        params![project_id, path, level, text, source_digest, now],
    )?;
    tx.execute(
        "INSERT INTO code_summaries_fts(rowid, text, path)
         SELECT rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.commit()?;
    Ok(())
}

/// Attaches an embedding to one already-inserted summary. Not an FTS
/// column, so a plain `UPDATE`.
pub fn set_code_summary_embedding(conn: &Connection, project_id: i64, path: &str, embedding: &[f32]) -> Result<(), KbError> {
    conn.execute(
        "UPDATE code_summaries SET embedding = ?1 WHERE project_id = ?2 AND path = ?3",
        params![encode_embedding(embedding), project_id, path],
    )?;
    Ok(())
}

/// Records which mirrored `memories` row (from `upsert_index_memory`)
/// corresponds to a module/repo summary. Never called for `level = "file"`
/// rows -- file summaries have no mirror (see the design doc).
pub fn set_code_summary_memory_id(conn: &Connection, project_id: i64, path: &str, memory_id: i64) -> Result<(), KbError> {
    conn.execute(
        "UPDATE code_summaries SET memory_id = ?1 WHERE project_id = ?2 AND path = ?3",
        params![memory_id, project_id, path],
    )?;
    Ok(())
}

/// Deletes one summary row along with its FTS entry -- e.g. a file/dir that
/// dropped out of scope, or was removed from the repo entirely. Does NOT
/// touch the mirrored `memories` row a module/repo summary may have
/// (`memory_id`); a later task's job code is responsible for superseding
/// that separately if the directory itself is gone, the same way
/// `code_file_delete` doesn't reach outside `code_files`/`code_chunks`.
pub fn code_summary_delete(conn: &Connection, project_id: i64, path: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_summaries_fts(code_summaries_fts, rowid, text, path)
         SELECT 'delete', rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.execute("DELETE FROM code_summaries WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
    tx.commit()?;
    Ok(())
}

/// Marks each of `paths` stale (`stale = 1`), setting `stale_since` only if
/// it is still NULL -- a summary already stale keeps the timestamp of when
/// it FIRST went stale, so this never resets the "how long has this been
/// stale" clock just because another child changed too (the design doc's
/// 7-day stale-regen-anyway rule depends on that first timestamp holding
/// still). Returns the number of rows actually touched.
pub fn mark_code_summaries_stale(conn: &Connection, project_id: i64, paths: &[String], now: &str) -> Result<usize, KbError> {
    if paths.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let mut updated = 0usize;
    {
        let mut stmt = tx.prepare(
            "UPDATE code_summaries SET stale = 1, stale_since = COALESCE(stale_since, ?1)
             WHERE project_id = ?2 AND path = ?3",
        )?;
        for path in paths {
            updated += stmt.execute(params![now, project_id, path])?;
        }
    }
    tx.commit()?;
    Ok(updated)
}

/// Every stale summary of a project, path ascending -- the nightly job's
/// own "what needs regenerating" list.
pub fn stale_code_summaries(conn: &Connection, project_id: i64) -> Result<Vec<CodeSummaryRow>, KbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {CODE_SUMMARY_COLUMNS} FROM code_summaries WHERE project_id = ?1 AND stale = 1 ORDER BY path"
    ))?;
    let rows = stmt.query_map(params![project_id], row_to_code_summary)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Summaries still waiting on an embedding, ordered `project_id, path`,
/// optionally scoped to one `project_id` -- same shape and same reason as
/// `code_chunks_missing_embedding`: lets an embedder-down gap be closed by a
/// separate pass without regenerating the summary text itself.
/// `(project_id, path, text)` since, unlike a chunk, a summary has no
/// surrogate integer id to address it by. The `project_id` filter (fix-list
/// item 3) is what lets `code_index::job::backfill_summary_embeddings` scope
/// a `--project`-limited run to that project alone, instead of a `limit`-
/// sized backfill spending its whole budget on another project's rows
/// first.
pub fn code_summaries_missing_embedding(conn: &Connection, project_id: Option<i64>, limit: usize) -> Result<Vec<(i64, String, String)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT project_id, path, text FROM code_summaries
         WHERE embedding IS NULL AND text != '' AND (?1 IS NULL OR project_id = ?1)
         ORDER BY project_id ASC, path ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project_id, limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Inserts an empty, already-stale placeholder `code_summaries` row for a
/// module/repo that has never been summarised -- recording `now` as its
/// `stale_since` (fix-list item 4). Without this, a module missing a row
/// entirely (as opposed to one whose real row later went stale) has no
/// timestamp for `code_index::summary::is_ready`'s 7-day override to
/// measure, so if its children never all become fresh (e.g. every one of
/// them gave up on its own summary, see `code_file_increment_summary_attempts`)
/// it would never regenerate at all. A no-op if a row already exists at
/// `path` (real or an earlier placeholder) -- `stale_since` must never move
/// once set, same invariant `mark_code_summaries_stale` protects. The next
/// real regeneration (`code_summary_upsert`) overwrites the placeholder
/// exactly like any other stale row.
pub fn code_summary_seed_placeholder(conn: &Connection, project_id: i64, path: &str, level: &str, now: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    let inserted = tx.execute(
        "INSERT INTO code_summaries (project_id, path, level, text, source_digest, stale, stale_since, updated_at)
         VALUES (?1, ?2, ?3, '', '', 1, ?4, ?4)
         ON CONFLICT(project_id, path) DO NOTHING",
        params![project_id, path, level, now],
    )?;
    if inserted > 0 {
        tx.execute(
            "INSERT INTO code_summaries_fts(rowid, text, path)
             SELECT rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
            params![project_id, path],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Clears `stale` (and `stale_since`) on one summary row and records
/// `source_digest` -- for a module/repo whose children digest is
/// unchanged, or that has nothing to summarise from (a placeholder), so no
/// regeneration call is needed. Text, embedding and `memory_id` untouched.
pub fn code_summary_clear_stale(conn: &Connection, project_id: i64, path: &str, source_digest: &str) -> Result<(), KbError> {
    conn.execute(
        "UPDATE code_summaries SET stale = 0, stale_since = NULL, source_digest = ?1 WHERE project_id = ?2 AND path = ?3",
        params![source_digest, project_id, path],
    )?;
    Ok(())
}

/// Turns an existing module/repo summary back into a placeholder: text and
/// digest cleared, embedding and `memory_id` dropped, marked stale (keeping
/// an existing `stale_since`), FTS entry rewritten -- so it regenerates
/// from its CURRENT children on a later run and meanwhile shows nothing.
/// Used when a descendant file turned out to be secret (final-review item
/// 13): the old text may have been generated from it. Returns the mirrored
/// memory id the row pointed at (the caller invalidates it), or `None` if
/// there is no row at `path`.
pub fn code_summary_placeholderize(conn: &Connection, project_id: i64, path: &str, now: &str) -> Result<Option<i64>, KbError> {
    let Some(row) = code_summary_get(conn, project_id, path)? else {
        return Ok(None);
    };
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_summaries_fts(code_summaries_fts, rowid, text, path)
         SELECT 'delete', rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.execute(
        "UPDATE code_summaries SET text = '', source_digest = '', embedding = NULL, memory_id = NULL,
             stale = 1, stale_since = COALESCE(stale_since, ?1), updated_at = ?1
         WHERE project_id = ?2 AND path = ?3",
        params![now, project_id, path],
    )?;
    tx.execute(
        "INSERT INTO code_summaries_fts(rowid, text, path)
         SELECT rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    tx.commit()?;
    Ok(row.memory_id)
}

fn row_to_summary_hit(r: &rusqlite::Row) -> rusqlite::Result<SummaryHit> {
    let stale_int: i64 = r.get(4)?;
    Ok(SummaryHit { project_id: r.get(0)?, path: r.get(1)?, level: r.get(2)?, text: r.get(3)?, stale: stale_int != 0, score: 0.0 })
}

const SUMMARY_HIT_COLUMNS: &str = "project_id, path, level, text, stale";

/// Hybrid search over generated code summaries, optionally scoped to one
/// project (`None` searches every project) -- same fusion
/// `search_code_chunks` uses: lexical (BM25 over `code_summaries_fts`,
/// normalized against this result set's own best match) and semantic
/// (cosine over `embedding`, floored at `SIM_NOISE_FLOOR`), combined as
/// `max(sim, LEXICAL_WEIGHT * lex)`. `code_summaries_fts`'s two columns
/// (`text`, `path`) are weighted evenly (`1.0, 1.0`) -- unlike
/// `search_code_chunks`'s heavy `symbol` weighting, a summary has no single
/// column that identifies it the way an exact symbol name identifies a
/// chunk, so there is no equivalent column to bias toward.
pub fn search_code_summaries(
    conn: &Connection,
    project_id: Option<i64>,
    query: &str,
    query_emb: Option<&[f32]>,
    limit: usize,
) -> Result<Vec<SummaryHit>, KbError> {
    let mut scored: HashMap<(i64, String), (SummaryHit, f32, f32)> = HashMap::new();

    if let Some(cleaned) = fts_query(query) {
        let mut stmt = conn.prepare(
            "SELECT c.project_id, c.path, c.level, c.text, c.stale, bm25(code_summaries_fts, 1.0, 1.0)
             FROM code_summaries_fts f JOIN code_summaries c ON c.rowid = f.rowid
             WHERE code_summaries_fts MATCH ?1 AND (?2 IS NULL OR c.project_id = ?2) AND c.text != ''
             ORDER BY bm25(code_summaries_fts, 1.0, 1.0) LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![cleaned, project_id, (limit * 4) as i64], |r| {
            Ok((row_to_summary_hit(r)?, -r.get::<_, f64>(5)? as f32))
        })?;
        let mut raw = Vec::new();
        for r in rows {
            raw.push(r?);
        }
        let best = raw.iter().map(|(_, b)| *b).fold(0.0f32, f32::max);
        for (hit, bm) in raw {
            let lex = if best > 0.0 { bm / best } else { 0.0 };
            scored.insert((hit.project_id, hit.path.clone()), (hit, lex, 0.0));
        }
    }

    if let Some(q) = query_emb {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_HIT_COLUMNS}, embedding FROM code_summaries WHERE embedding IS NOT NULL AND text != '' AND (?1 IS NULL OR project_id = ?1)"
        ))?;
        let rows = stmt.query_map(params![project_id], |r| Ok((row_to_summary_hit(r)?, r.get::<_, Vec<u8>>(5)?)))?;
        let mut sims: Vec<(f32, SummaryHit)> = Vec::new();
        for r in rows {
            let (hit, blob) = r?;
            let v = decode_embedding(&blob);
            let sim = cosine(q, &v);
            if sim > SIM_NOISE_FLOOR {
                sims.push((sim, hit));
            }
        }
        sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        for (sim, hit) in sims.into_iter().take(limit * 4) {
            let key = (hit.project_id, hit.path.clone());
            scored.entry(key).and_modify(|e| e.2 = sim).or_insert((hit, 0.0, sim));
        }
    }

    let mut out: Vec<SummaryHit> = scored
        .into_values()
        .map(|(mut hit, lex, sim)| {
            hit.score = sim.max(LEXICAL_WEIGHT * lex);
            hit
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    Ok(out)
}

// ---------------------------------------------------------------------
// Code symbol graph (schema 35, `migrate_v34_to_v35`; see
// `code_index::chunk::extract_refs`). `code_symbols` is one row per
// definition, `code_edges` one row per reference (call/include/import/
// inherits) -- both derived state, parsed out of a file's current text,
// droppable and rebuildable the same way `code_chunks` is. `code_history`
// (month-by-month repo history text) is declared here too but is Task 3's
// to write; only `code_history_get`/`code_history_upsert`/
// `code_history_set_memory_id`/`code_history_list` live here for now.
// ---------------------------------------------------------------------

/// A definition to insert via `replace_file_symbols`. No `id` (SQLite
/// assigns it) and no `project_id`/`path` (given once for the whole file
/// being replaced), matching `NewCodeChunk`'s own shape.
#[derive(Debug, Clone, Default)]
pub struct NewSymbol {
    pub name: String,
    pub qualified: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
}

/// A reference to insert via `replace_file_symbols`. `src_symbol_id` is
/// deliberately absent -- `replace_file_symbols` resolves it itself, from
/// `src_line` and the line ranges of the symbols inserted alongside it (the
/// brief's "enclosing definition by line range" rule) -- and
/// `dst_symbol_id` always starts NULL, resolved later by `resolve_edges`.
#[derive(Debug, Clone)]
pub struct NewEdge {
    pub src_line: i64,
    pub dst_name: String,
    /// One of `"calls"`, `"includes"`, `"imports"`, `"inherits"` --
    /// `code_edges.kind`'s own CHECK constraint enforces this at the SQL
    /// level, so a typo here surfaces immediately as an insert error
    /// rather than a silently-orphaned row.
    pub kind: String,
}

/// One row of `code_symbols`.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolRow {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub name: String,
    pub qualified: String,
    pub kind: String,
    pub start_line: i64,
    pub end_line: i64,
}

const SYMBOL_COLUMNS: &str = "id, project_id, path, name, qualified, kind, start_line, end_line";

fn row_to_symbol(r: &rusqlite::Row) -> rusqlite::Result<SymbolRow> {
    Ok(SymbolRow {
        id: r.get(0)?,
        project_id: r.get(1)?,
        path: r.get(2)?,
        name: r.get(3)?,
        qualified: r.get(4)?,
        kind: r.get(5)?,
        start_line: r.get(6)?,
        end_line: r.get(7)?,
    })
}

/// One row of `code_edges`.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRow {
    pub project_id: i64,
    pub src_symbol_id: Option<i64>,
    pub src_path: String,
    pub src_line: i64,
    /// The target exactly as written at the reference site (never
    /// rewritten by resolution -- an `includes` edge's resolved file lives
    /// in `dst_path`).
    pub dst_name: String,
    pub dst_symbol_id: Option<i64>,
    pub kind: String,
    /// `includes`/`imports` only: the unique tracked repo-relative path
    /// `dst_name` resolved to, once `resolve_edges*` found one.
    pub dst_path: Option<String>,
}

pub(crate) const EDGE_COLUMNS: &str = "project_id, src_symbol_id, src_path, src_line, dst_name, dst_symbol_id, kind, dst_path";

pub(crate) fn row_to_edge(r: &rusqlite::Row) -> rusqlite::Result<EdgeRow> {
    Ok(EdgeRow {
        project_id: r.get(0)?,
        src_symbol_id: r.get(1)?,
        src_path: r.get(2)?,
        src_line: r.get(3)?,
        dst_name: r.get(4)?,
        dst_symbol_id: r.get(5)?,
        kind: r.get(6)?,
        dst_path: r.get(7)?,
    })
}

/// Queues `value` (a symbol name, a source path, or a file path) for the
/// next incremental `resolve_edges_incremental` pass. `INSERT OR IGNORE`:
/// the queue is a set.
fn pend(conn: &Connection, project_id: i64, kind: &str, value: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT OR IGNORE INTO code_graph_pending (project_id, kind, value) VALUES (?1, ?2, ?3)",
        params![project_id, kind, value],
    )?;
    Ok(())
}

/// Queues every symbol name currently defined in `path` -- called just
/// before those symbols are deleted, since a removed definition can turn a
/// previously ambiguous name unique (and its incoming edges were just
/// nulled and need re-resolving).
fn pend_names_in_file(conn: &Connection, project_id: i64, path: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT OR IGNORE INTO code_graph_pending (project_id, kind, value)
         SELECT DISTINCT project_id, 'name', name FROM code_symbols WHERE project_id = ?1 AND path = ?2",
        params![project_id, path],
    )?;
    Ok(())
}

/// Nulls `dst_symbol_id` on every edge pointing at one of `path`'s symbols
/// (they are about to be deleted). Served by `ix_code_edges_dst_symbol`
/// (`project_id, dst_symbol_id`) -- see the EXPLAIN test
/// `null_out_incoming_edges_uses_the_dst_symbol_index`; without it this
/// was a full scan of the project's edges per replaced file.
const NULL_INCOMING_SQL: &str = "UPDATE code_edges SET dst_symbol_id = NULL
     WHERE project_id = ?1 AND dst_symbol_id IN (SELECT id FROM code_symbols WHERE project_id = ?1 AND path = ?2)";

/// Replaces every symbol and outgoing edge of one file in a single
/// transaction, returning the new symbols' ids in the same order as
/// `symbols` (same contract as `replace_code_chunks`). "Outgoing" edges are
/// every `code_edges` row with `src_path = path`, regardless of whether
/// they ended up resolved to a symbol -- a file's edges are keyed by where
/// the reference is written, not by whether it resolved.
///
/// Order of operations, all inside one transaction so a re-chunk (the file
/// changed) never leaves a half-updated graph visible to a concurrent
/// reader: queue the file's old symbol names for re-resolution; null out
/// `dst_symbol_id` on any OTHER file's edges that point at one of this
/// file's about-to-be-deleted symbols (otherwise they'd dangle); delete this
/// file's old symbols and outgoing edges; insert the new symbols, capturing
/// each new id alongside its line range; insert the new edges, resolving
/// each one's `src_symbol_id` to whichever just-inserted symbol's
/// `[start_line, end_line]` most tightly contains `src_line` -- `None` if no
/// symbol in this file encloses that line; finally queue the new names, the
/// path as a changed source, and the path as a changed file (an include
/// target). `dst_symbol_id`/`dst_path` always start NULL; `resolve_edges*`
/// fills them in later.
pub fn replace_file_symbols(conn: &Connection, project_id: i64, path: &str, symbols: &[NewSymbol], edges: &[NewEdge]) -> Result<Vec<i64>, KbError> {
    replace_file_symbols_inner(conn, project_id, path, None, symbols, edges)
}

/// [`replace_file_symbols`], also recording `blob` as the file's
/// `code_files.refs_blob` in the same transaction -- what every indexing
/// call site uses, so the symbol backfill (`refs_blob IS NULL OR refs_blob
/// != blob`) never re-selects a file whose current blob was already
/// extracted, even one that yielded zero symbols. A no-op on `refs_blob`
/// when `path` has no `code_files` row yet.
pub fn replace_file_symbols_for_blob(
    conn: &Connection,
    project_id: i64,
    path: &str,
    blob: &str,
    symbols: &[NewSymbol],
    edges: &[NewEdge],
) -> Result<Vec<i64>, KbError> {
    replace_file_symbols_inner(conn, project_id, path, Some(blob), symbols, edges)
}

fn replace_file_symbols_inner(
    conn: &Connection,
    project_id: i64,
    path: &str,
    blob: Option<&str>,
    symbols: &[NewSymbol],
    edges: &[NewEdge],
) -> Result<Vec<i64>, KbError> {
    let tx = conn.unchecked_transaction()?;
    pend_names_in_file(&tx, project_id, path)?;
    tx.execute(NULL_INCOMING_SQL, params![project_id, path])?;
    tx.execute("DELETE FROM code_edges WHERE project_id = ?1 AND src_path = ?2", params![project_id, path])?;
    tx.execute("DELETE FROM code_symbols WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;

    let mut ids = Vec::with_capacity(symbols.len());
    let mut ranges: Vec<(i64, i64, i64)> = Vec::with_capacity(symbols.len());
    {
        let mut stmt = tx.prepare(
            "INSERT INTO code_symbols (project_id, path, name, qualified, kind, start_line, end_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for s in symbols {
            stmt.execute(params![project_id, path, s.name, s.qualified, s.kind, s.start_line, s.end_line])?;
            let id = tx.last_insert_rowid();
            ids.push(id);
            ranges.push((id, s.start_line, s.end_line));
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO code_edges (project_id, src_symbol_id, src_path, src_line, dst_name, dst_symbol_id, kind, dst_path)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL)",
        )?;
        for e in edges {
            let src_symbol_id = ranges
                .iter()
                .filter(|(_, start, end)| *start <= e.src_line && e.src_line <= *end)
                .min_by_key(|(_, start, end)| end - start)
                .map(|(id, _, _)| *id);
            stmt.execute(params![project_id, src_symbol_id, path, e.src_line, e.dst_name, e.kind])?;
        }
    }
    pend_names_in_file(&tx, project_id, path)?;
    pend(&tx, project_id, "src", path)?;
    pend(&tx, project_id, "file", path)?;
    if let Some(b) = blob {
        tx.execute("UPDATE code_files SET refs_blob = ?1 WHERE project_id = ?2 AND path = ?3", params![b, project_id, path])?;
    }
    tx.commit()?;
    Ok(ids)
}

/// Deletes every symbol and outgoing edge of a file (a file removed from
/// the repo, or recategorized out of scope) -- same queue-null-delete order
/// as `replace_file_symbols`, so incoming edges from OTHER files never
/// dangle on a `dst_symbol_id` that no longer exists; their `dst_name`
/// (and everything else about them) is left untouched, so a later
/// `resolve_edges*` can re-resolve them if a same-named symbol reappears
/// elsewhere. Include edges whose `dst_path` was this file are nulled too
/// (the file is gone as a target) and the path is queued as a changed file.
pub fn delete_file_symbols(conn: &Connection, project_id: i64, path: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    pend_names_in_file(&tx, project_id, path)?;
    tx.execute(NULL_INCOMING_SQL, params![project_id, path])?;
    tx.execute("DELETE FROM code_edges WHERE project_id = ?1 AND src_path = ?2", params![project_id, path])?;
    tx.execute("DELETE FROM code_symbols WHERE project_id = ?1 AND path = ?2", params![project_id, path])?;
    tx.execute("UPDATE code_edges SET dst_path = NULL WHERE project_id = ?1 AND dst_path = ?2", params![project_id, path])?;
    pend(&tx, project_id, "file", path)?;
    tx.commit()?;
    Ok(())
}

/// Moves every `code_symbols`/`code_edges` row of `from` to `to` in
/// place -- the rename-with-unchanged-blob case (mirrors
/// `code_index::job::move_code_rows`'s handling of `code_files`/
/// `code_chunks`). No id, name or line-range changes: only the path
/// columns move, so every existing `dst_symbol_id` resolution (this file's
/// symbols as someone else's callee) survives the rename untouched. An
/// include edge that resolved to `from` is un-resolved (its written
/// `dst_name` may or may not still match `to`) and both paths are queued as
/// changed files, so the next pass re-resolves it against the new layout.
pub fn move_file_symbols(conn: &Connection, project_id: i64, from: &str, to: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("UPDATE code_symbols SET path = ?1 WHERE project_id = ?2 AND path = ?3", params![to, project_id, from])?;
    tx.execute("UPDATE code_edges SET src_path = ?1 WHERE project_id = ?2 AND src_path = ?3", params![to, project_id, from])?;
    tx.execute("UPDATE code_edges SET dst_path = NULL WHERE project_id = ?1 AND dst_path = ?2", params![project_id, from])?;
    pend(&tx, project_id, "file", from)?;
    pend(&tx, project_id, "file", to)?;
    tx.commit()?;
    Ok(())
}

/// The text after the last `"::"` or `"."` in `dst_name` (whichever comes
/// later), or the whole string if it has neither. Generic/template
/// arguments are stripped at extraction (`chunk::strip_generic_args`), so
/// a `::` inside `<...>` can no longer be mistaken for the last path
/// separator here.
pub(crate) fn last_path_segment(dst_name: &str) -> &str {
    let cc = dst_name.rfind("::").map(|i| i + 2);
    let dot = dst_name.rfind('.').map(|i| i + 1);
    match cc.max(dot) {
        Some(i) => &dst_name[i..],
        None => dst_name,
    }
}

/// `dst_name` with the path prefixes that only say "relative to here"
/// removed (`::x`, `crate::`/`self::`/`super::` chains) and `Self::x`
/// reduced to the bare `x` (the enclosing impl's type isn't known at the
/// call site) -- so such calls resolve the way the old leaf fallback let
/// them, while `std::find` stays qualified and never falls back to a
/// project `find`.
fn normalize_call_target(dst_name: &str) -> &str {
    let mut s = dst_name.strip_prefix("::").unwrap_or(dst_name);
    loop {
        if let Some(rest) = s.strip_prefix("crate::").or_else(|| s.strip_prefix("self::")).or_else(|| s.strip_prefix("super::")) {
            s = rest;
            continue;
        }
        if let Some(rest) = s.strip_prefix("Self::") {
            if !rest.contains("::") {
                return rest;
            }
        }
        return s;
    }
}

/// Whether repo-relative `path` is (or ends with a `/`-bounded suffix
/// equal to) `target` -- `target` being an `#include`/import's own written
/// form, e.g. `"local/thing.h"` or `"thing.h"`. Deliberately boundary-aware
/// (never a bare substring match): `"foo.h".ends_with("h.h")` would
/// otherwise false-positive, and a suffix match must land on a path
/// separator, not the middle of a component.
fn path_matches_include_target(path: &str, target: &str) -> bool {
    path == target || path.ends_with(&format!("/{target}"))
}

/// The last `/`-separated component of a path or include target.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What one `resolve_edges*` pass did: `examined` edges were candidates
/// (unresolved and in scope of the pass), `resolved` of them got a
/// `dst_symbol_id` (calls/inherits) or a `dst_path` (includes/imports).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveStats {
    pub examined: usize,
    pub resolved: usize,
}

/// Full, project-wide resolution of every unresolved edge; returns how many
/// were resolved. Also clears the project's `code_graph_pending` queue (a
/// full pass covers everything queued). Used on a project's first index
/// run and whenever the symbol backfill ran; nightly incremental runs use
/// [`resolve_edges_incremental`]. Idempotent: resolved edges are excluded
/// by the `dst_symbol_id IS NULL` / `dst_path IS NULL` filters, so a second
/// pass returns 0.
///
/// Rules, applied set-based in one transaction (see `resolve_pending`):
/// - `calls`/`inherits`: exactly one project symbol whose `qualified`
///   equals `dst_name` (after `normalize_call_target`); else a leaf-name
///   fallback -- for an UNQUALIFIED `dst_name`, exactly one symbol whose
///   `name` equals it; for a qualified one, exactly one symbol whose `name`
///   is the leaf AND whose `qualified` equals or ends with the full
///   `dst_name` on a `::`/`.` boundary (`std::find` never resolves to a
///   project `find`; `picking::add_event` does resolve to
///   `helios::picking::add_event`). Ambiguous (0 or 2+) stays NULL.
/// - `includes`/`imports`: `dst_symbol_id` stays NULL always (these
///   resolve to a FILE); if exactly one tracked `code_files.path` matches
///   `dst_name` per `path_matches_include_target` (candidates found via a
///   basename -> paths map, not a scan per edge), `dst_path` is set to it.
///   `dst_name` is never rewritten.
pub fn resolve_edges(conn: &Connection, project_id: i64) -> Result<usize, KbError> {
    Ok(resolve_edges_all(conn, project_id)?.resolved)
}

/// [`resolve_edges`] returning the full [`ResolveStats`] (`examined` = every
/// unresolved edge of the project).
pub fn resolve_edges_all(conn: &Connection, project_id: i64) -> Result<ResolveStats, KbError> {
    resolve_pending(conn, project_id, None)
}

/// Incremental resolution: only unresolved edges whose source file was
/// replaced since the last pass, whose target leaf name matches a symbol
/// name added or removed since then, or (for includes/imports) whose
/// target basename matches a file added/moved/removed since then -- all
/// read from `code_graph_pending`, which the symbol mutations fill in the
/// same transaction as their own writes. An empty queue examines nothing
/// and returns `ResolveStats::default()` without scanning `code_edges` at
/// all. Clears the queue.
pub fn resolve_edges_incremental(conn: &Connection, project_id: i64) -> Result<ResolveStats, KbError> {
    let mut names: HashSet<String> = HashSet::new();
    let mut srcs: HashSet<String> = HashSet::new();
    let mut file_bases: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT kind, value FROM code_graph_pending WHERE project_id = ?1")?;
        let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (kind, value) = r?;
            match kind.as_str() {
                "name" => {
                    names.insert(value);
                }
                "src" => {
                    srcs.insert(value);
                }
                _ => {
                    file_bases.insert(basename(&value).to_string());
                }
            }
        }
    }
    if names.is_empty() && srcs.is_empty() && file_bases.is_empty() {
        return Ok(ResolveStats::default());
    }
    resolve_pending(conn, project_id, Some(&PendingFilter { names, srcs, file_bases }))
}

/// Which unresolved edges an incremental pass considers.
struct PendingFilter {
    names: HashSet<String>,
    srcs: HashSet<String>,
    file_bases: HashSet<String>,
}

fn resolve_pending(conn: &Connection, project_id: i64, filter: Option<&PendingFilter>) -> Result<ResolveStats, KbError> {
    let tx = conn.unchecked_transaction()?;
    let mut stats = ResolveStats::default();

    // -- calls / inherits: candidate edges into a temp table ------------
    tx.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS kb_resolve_calls (
             rid INTEGER PRIMARY KEY, dst_name TEXT NOT NULL, leaf TEXT NOT NULL, qualified_form INTEGER NOT NULL);
         DELETE FROM temp.kb_resolve_calls;",
    )?;
    {
        let mut sel = tx.prepare(
            "SELECT rowid, src_path, dst_name FROM code_edges
             WHERE project_id = ?1 AND dst_symbol_id IS NULL AND kind IN ('calls','inherits')",
        )?;
        let mut ins = tx.prepare("INSERT INTO temp.kb_resolve_calls (rid, dst_name, leaf, qualified_form) VALUES (?1, ?2, ?3, ?4)")?;
        let rows = sel.query_map(params![project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for r in rows {
            let (rid, src_path, dst_name) = r?;
            let target = normalize_call_target(&dst_name);
            let leaf = last_path_segment(target);
            if let Some(f) = filter {
                if !f.srcs.contains(&src_path) && !f.names.contains(leaf) {
                    continue;
                }
            }
            let qualified_form = i64::from(leaf.len() != target.len());
            ins.execute(params![rid, target, leaf, qualified_form])?;
            stats.examined += 1;
        }
    }
    // Pass 1: unique exact `qualified` match (ix_code_symbols_qualified).
    stats.resolved += tx.execute(
        "UPDATE code_edges SET dst_symbol_id = r.sid
         FROM (SELECT p.rid AS rid, MIN(s.id) AS sid
               FROM temp.kb_resolve_calls p
               JOIN code_symbols s ON s.project_id = ?1 AND s.qualified = p.dst_name
               GROUP BY p.rid HAVING COUNT(*) = 1) AS r
         WHERE code_edges.rowid = r.rid",
        params![project_id],
    )?;
    // Pass 2: leaf-name fallback (ix_code_symbols_name), boundary-checked
    // for a qualified `dst_name`; ambiguity counts every candidate.
    stats.resolved += tx.execute(
        "UPDATE code_edges SET dst_symbol_id = r.sid
         FROM (SELECT p.rid AS rid, MIN(s.id) AS sid
               FROM temp.kb_resolve_calls p
               JOIN code_symbols s ON s.project_id = ?1 AND s.name = p.leaf
               WHERE p.qualified_form = 0
                  OR s.qualified = p.dst_name
                  OR substr(s.qualified, -(length(p.dst_name) + 2)) = '::' || p.dst_name
                  OR substr(s.qualified, -(length(p.dst_name) + 1)) = '.' || p.dst_name
               GROUP BY p.rid HAVING COUNT(*) = 1) AS r
         WHERE code_edges.rowid = r.rid AND code_edges.dst_symbol_id IS NULL",
        params![project_id],
    )?;
    tx.execute("DELETE FROM temp.kb_resolve_calls", [])?;

    // -- includes / imports: basename -> paths map ------------------------
    let mut pending_files: Vec<(i64, String)> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT rowid, src_path, dst_name FROM code_edges
             WHERE project_id = ?1 AND dst_path IS NULL AND kind IN ('includes','imports')",
        )?;
        let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        for r in rows {
            let (rid, src_path, dst_name) = r?;
            if let Some(f) = filter {
                if !f.srcs.contains(&src_path) && !f.file_bases.contains(basename(&dst_name)) {
                    continue;
                }
            }
            pending_files.push((rid, dst_name));
        }
    }
    stats.examined += pending_files.len();
    if !pending_files.is_empty() {
        let mut by_base: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut stmt = tx.prepare("SELECT path FROM code_files WHERE project_id = ?1")?;
            let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
            for r in rows {
                let p = r?;
                by_base.entry(basename(&p).to_string()).or_default().push(p);
            }
        }
        let mut upd = tx.prepare("UPDATE code_edges SET dst_path = ?1 WHERE rowid = ?2")?;
        for (rid, dst_name) in pending_files {
            let Some(cands) = by_base.get(basename(&dst_name)) else { continue };
            let mut matches = cands.iter().filter(|p| path_matches_include_target(p, &dst_name));
            if let (Some(only), None) = (matches.next(), matches.next()) {
                upd.execute(params![only, rid])?;
                stats.resolved += 1;
            }
        }
    }

    tx.execute("DELETE FROM code_graph_pending WHERE project_id = ?1", params![project_id])?;
    tx.commit()?;
    Ok(stats)
}

/// Definitions named `name` in a project: an exact `name` match if any
/// exist, else every symbol whose `qualified` ends with `"::" + name` or
/// `"." + name` (a caller who only knows the leaf name still finds a
/// scoped definition). Never both -- an exact-name hit is always the more
/// specific answer, so the suffix fallback only runs when that first query
/// comes back empty.
pub fn symbol_definitions(conn: &Connection, project_id: i64, name: &str) -> Result<Vec<SymbolRow>, KbError> {
    let mut stmt = conn.prepare(&format!("SELECT {SYMBOL_COLUMNS} FROM code_symbols WHERE project_id = ?1 AND name = ?2 ORDER BY path, start_line"))?;
    let rows = stmt.query_map(params![project_id, name], row_to_symbol)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    if !out.is_empty() {
        return Ok(out);
    }

    let suffix_cc = format!("%::{name}");
    let suffix_dot = format!("%.{name}");
    let mut stmt = conn.prepare(&format!(
        "SELECT {SYMBOL_COLUMNS} FROM code_symbols WHERE project_id = ?1 AND (qualified LIKE ?2 OR qualified LIKE ?3) ORDER BY path, start_line"
    ))?;
    let rows = stmt.query_map(params![project_id, suffix_cc, suffix_dot], row_to_symbol)?;
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// How `callers_of` is asked to identify its target symbol: a resolved
/// `code_symbols.id`, or a bare name matched against `code_edges.dst_name`
/// directly (so a caller can ask "who calls `foo`" even before/without
/// `resolve_edges` ever running).
#[derive(Debug, Clone, Copy)]
pub enum SymbolLookup<'a> {
    Id(i64),
    Name(&'a str),
}

/// Every `calls` edge targeting `target`, `src_path`/`src_line` ascending,
/// capped at `limit`. `SymbolLookup::Id` matches the resolved
/// `dst_symbol_id`; `SymbolLookup::Name` matches the literal `dst_name`
/// regardless of resolution state (resolving never rewrites `dst_name` for
/// `calls`/`inherits`, only sets `dst_symbol_id` alongside it -- see
/// `resolve_edges`).
pub fn callers_of(conn: &Connection, project_id: i64, target: SymbolLookup, limit: usize) -> Result<Vec<EdgeRow>, KbError> {
    let mut out = Vec::new();
    match target {
        SymbolLookup::Id(id) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EDGE_COLUMNS} FROM code_edges WHERE project_id = ?1 AND kind = 'calls' AND dst_symbol_id = ?2
                 ORDER BY src_path, src_line LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![project_id, id, limit as i64], row_to_edge)?;
            for r in rows {
                out.push(r?);
            }
        }
        SymbolLookup::Name(name) => {
            let mut stmt = conn.prepare(&format!(
                "SELECT {EDGE_COLUMNS} FROM code_edges WHERE project_id = ?1 AND kind = 'calls' AND dst_name = ?2
                 ORDER BY src_path, src_line LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![project_id, name, limit as i64], row_to_edge)?;
            for r in rows {
                out.push(r?);
            }
        }
    }
    Ok(out)
}

/// Every `calls` edge whose enclosing definition (`src_symbol_id`) is
/// `symbol_id`, `src_line` ascending, capped at `limit` -- the reverse of
/// `callers_of`. Unlike callers, callees are only ever asked of a specific
/// resolved symbol (there's no "by name" sense of "what does `foo`
/// call" when several definitions share that name).
pub fn callees_of(conn: &Connection, project_id: i64, symbol_id: i64, limit: usize) -> Result<Vec<EdgeRow>, KbError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {EDGE_COLUMNS} FROM code_edges WHERE project_id = ?1 AND kind = 'calls' AND src_symbol_id = ?2 ORDER BY src_line LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![project_id, symbol_id, limit as i64], row_to_edge)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// One row of `code_history`: one project's one calendar month
/// (`"YYYY-MM"`) of commit-log-derived narrative text -- see the
/// constraints doc's history-memory rules and Task 3's job. `memory_id` is
/// the mirrored `memories` row's id once one exists (set by
/// `code_history_set_memory_id`, never by `code_history_upsert` itself,
/// same split as `code_summaries.memory_id`/`set_code_summary_memory_id`).
#[derive(Debug, Clone, PartialEq)]
pub struct CodeHistoryRow {
    pub project_id: i64,
    pub month: String,
    pub text: String,
    pub commits: i64,
    pub last_sha: String,
    pub digest: String,
    pub memory_id: Option<i64>,
    pub updated_at: String,
    /// Newline-separated top-churn paths of the month (schema v35
    /// fix-wave column; NULL on a row written before it existed).
    pub files: Option<String>,
}

const CODE_HISTORY_COLUMNS: &str = "project_id, month, text, commits, last_sha, digest, memory_id, updated_at, files";

fn row_to_code_history(r: &rusqlite::Row) -> rusqlite::Result<CodeHistoryRow> {
    Ok(CodeHistoryRow {
        project_id: r.get(0)?,
        month: r.get(1)?,
        text: r.get(2)?,
        commits: r.get(3)?,
        last_sha: r.get(4)?,
        digest: r.get(5)?,
        memory_id: r.get(6)?,
        updated_at: r.get(7)?,
        files: r.get(8)?,
    })
}

/// One project/month's history row, if it has ever been generated.
pub fn code_history_get(conn: &Connection, project_id: i64, month: &str) -> Result<Option<CodeHistoryRow>, KbError> {
    Ok(conn
        .query_row(
            &format!("SELECT {CODE_HISTORY_COLUMNS} FROM code_history WHERE project_id = ?1 AND month = ?2"),
            params![project_id, month],
            row_to_code_history,
        )
        .optional()?)
}

/// Inserts or regenerates one project/month's history row. `memory_id` is
/// deliberately left alone on conflict (excluded from the `DO UPDATE SET`
/// list) -- regenerating the same month's text (the only thing allowed to
/// supersede a history memory, per the constraints doc) must keep pointing
/// at the same mirrored `memories` row so Task 3 can update it in place
/// rather than mint a new one.
///
/// `text` is the month's SUMMARY (the model's reply), never the raw log.
/// The history job passes an empty `digest` here and writes the real one
/// last via [`code_history_set_digest`], once the mirror is in place -- a
/// run that dies in between leaves a digest that never matches, so the
/// month regenerates instead of being skipped with a missing mirror.
#[allow(clippy::too_many_arguments)]
pub fn code_history_upsert(
    conn: &Connection,
    project_id: i64,
    month: &str,
    text: &str,
    commits: i64,
    last_sha: &str,
    digest: &str,
    files: Option<&str>,
    now: &str,
) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO code_history (project_id, month, text, commits, last_sha, digest, memory_id, updated_at, files)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)
         ON CONFLICT(project_id, month) DO UPDATE SET
             text = excluded.text, commits = excluded.commits, last_sha = excluded.last_sha,
             digest = excluded.digest, updated_at = excluded.updated_at, files = excluded.files",
        params![project_id, month, text, commits, last_sha, digest, now, files],
    )?;
    Ok(())
}

/// Records a month's digest -- the history job's LAST write for a month.
pub fn code_history_set_digest(conn: &Connection, project_id: i64, month: &str, digest: &str) -> Result<(), KbError> {
    conn.execute("UPDATE code_history SET digest = ?1 WHERE project_id = ?2 AND month = ?3", params![digest, project_id, month])?;
    Ok(())
}

/// Deletes one month's history row (a month no longer reachable from
/// `HEAD`, e.g. after a history rewrite). The caller invalidates its mirror.
pub fn code_history_delete(conn: &Connection, project_id: i64, month: &str) -> Result<(), KbError> {
    conn.execute("DELETE FROM code_history WHERE project_id = ?1 AND month = ?2", params![project_id, month])?;
    Ok(())
}

/// Records which mirrored `memories` row corresponds to a month's history
/// text -- same split as `set_code_summary_memory_id`.
pub fn code_history_set_memory_id(conn: &Connection, project_id: i64, month: &str, memory_id: i64) -> Result<(), KbError> {
    conn.execute("UPDATE code_history SET memory_id = ?1 WHERE project_id = ?2 AND month = ?3", params![memory_id, project_id, month])?;
    Ok(())
}

/// Every history row of a project, month ascending.
pub fn code_history_list(conn: &Connection, project_id: i64) -> Result<Vec<CodeHistoryRow>, KbError> {
    let mut stmt = conn.prepare(&format!("SELECT {CODE_HISTORY_COLUMNS} FROM code_history WHERE project_id = ?1 ORDER BY month"))?;
    let rows = stmt.query_map(params![project_id], row_to_code_history)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Prefix every index-owned `memories.source` carries (`code-index:...`,
/// e.g. `code-index:<project>:<dir>/`) -- shared by `supersession_guard`'s
/// "index-owned" block, `upsert_index_memory`, and the reflect insight
/// working set's exclusion filter (`cli::reflect_working_set`). Also used,
/// pre-phase-2, by `code_index::scope`'s vendored-directory memories,
/// which is why this is a prefix check (`starts_with`) and not an exact
/// match against one fixed string.
pub const INDEX_OWNED_SOURCE_PREFIX: &str = "code-index:";

/// Second prefix that must be treated exactly like `INDEX_OWNED_SOURCE_PREFIX`
/// everywhere: the monthly commit-history mirror the history job
/// (`code_index::history`) writes, `code-history:<project>:<YYYY-MM>`. Kept
/// as its own constant (not folded into `INDEX_OWNED_SOURCE_PREFIX`) because
/// the two rows come from different producers with different regeneration
/// rules -- a history row is superseded only by the history job re-running
/// the same month, never by `upsert_index_memory` -- but every *filter* that
/// must exclude index-owned rows has to exclude both prefixes identically.
/// `is_index_owned_source`/`is_index_owned`/`not_index_owned_sql` are the one
/// place that combination is spelled out; every guard below goes through one
/// of them instead of repeating the two literals.
pub const CODE_HISTORY_SOURCE_PREFIX: &str = "code-history:";

/// Whether `source` (a raw `memories.source` value, already unwrapped from
/// `Option`) is owned by the code index or the code-history job -- starts
/// with `INDEX_OWNED_SOURCE_PREFIX` ("code-index:") or
/// `CODE_HISTORY_SOURCE_PREFIX` ("code-history:"). The `&str` twin of
/// `is_index_owned`, for call sites that only have the source string (e.g.
/// `Option<&str>` from a lighter row type than `Memory`).
pub fn is_index_owned_source(source: &str) -> bool {
    source.starts_with(INDEX_OWNED_SOURCE_PREFIX) || source.starts_with(CODE_HISTORY_SOURCE_PREFIX)
}

/// Whether `m` is an index-owned row (`source` starts with
/// `INDEX_OWNED_SOURCE_PREFIX` or `CODE_HISTORY_SOURCE_PREFIX`). Every
/// automatic pass that pools memories for judging (dedupe, contradiction,
/// strength review, graph extraction, supersession audit, improve evidence)
/// filters these out, and `supersession_guard` blocks on either side being
/// one.
pub fn is_index_owned(m: &Memory) -> bool {
    m.source.as_deref().is_some_and(is_index_owned_source)
}

/// SQL boolean fragment excluding both index- and history-owned rows, to be
/// spliced into a `WHERE` clause via `format!`. `col` is the source-column
/// reference already in scope at the call site (`"source"`, `"m.source"`,
/// `"s.source"`, ...) -- callers differ on aliasing, so this takes the
/// column expression as a parameter rather than being a single fixed
/// string. Mirrors `is_index_owned_source` exactly; every raw-SQL filter
/// that used to hardcode `NOT LIKE 'code-index:%'` calls this instead, so a
/// third owned prefix, if one is ever added, only has to change here and in
/// `is_index_owned_source`.
pub fn not_index_owned_sql(col: &str) -> String {
    format!("({col} IS NULL OR ({col} NOT LIKE '{INDEX_OWNED_SOURCE_PREFIX}%' AND {col} NOT LIKE '{CODE_HISTORY_SOURCE_PREFIX}%'))")
}

/// Writes (or regenerates) a module/repo summary's mirror row in
/// `memories`, per the design doc: "Module and repo summaries are ALSO
/// written as memories ... They are index-owned: inserted with
/// `reflected_at` set, excluded from every automatic supersession path
/// ... and replaced by the index itself via `store::supersede` when
/// regenerated."
///
/// `source` is the exact index-owned source string (e.g.
/// `code-index:<project>:<dir>/`, `code-index:<project>:/` for the repo).
/// Every ACTIVE memory with that exact source (normally one; more if an
/// older generation was restored) is superseded (`store::supersede`) in
/// favor of a freshly inserted row -- never edited in place -- so the
/// audit trail (`mach kb list --superseded`) shows the summary's history
/// the same way any other supersession does. If none exists (first run, or
/// the previous one was already superseded by something else), this is a
/// plain insert.
///
/// The new row always gets: `basis = "derived"` (`BASIS_DERIVED` -- this is
/// code output, not a session claim), `importance = 6`, `reviewed = true`
/// (a deterministic summary needs no human review gate), `project` set to
/// `project`, and `reflected_at` set to `now` immediately -- an index-owned
/// memory must never sit in the reflect insight stage's unreflected
/// backlog; the reflect working set additionally excludes it by source
/// prefix as a second line of defense (see `cli::reflect_working_set`).
///
/// Returns the new memory's id.
pub fn upsert_index_memory(
    conn: &Connection,
    project: &str,
    source: &str,
    content: &str,
    embedding: Option<&[f32]>,
    now: &str,
) -> Result<i64, KbError> {
    upsert_index_memory_inner(conn, None, project, source, content, embedding, now)
}

/// [`upsert_index_memory`] for a module/repo summary's mirror: also records
/// the new memory id on the `code_summaries` row (`project_id`, `path`) in
/// the SAME transaction, so a crash can never leave the summary pointing at
/// a superseded memory (or at none while an active mirror exists).
#[allow(clippy::too_many_arguments)]
pub fn upsert_index_memory_for_summary(
    conn: &Connection,
    project_id: i64,
    path: &str,
    project: &str,
    source: &str,
    content: &str,
    embedding: Option<&[f32]>,
    now: &str,
) -> Result<i64, KbError> {
    upsert_index_memory_inner(conn, Some((project_id, path)), project, source, content, embedding, now)
}

fn upsert_index_memory_inner(
    conn: &Connection,
    summary_key: Option<(i64, &str)>,
    project: &str,
    source: &str,
    content: &str,
    embedding: Option<&[f32]>,
    now: &str,
) -> Result<i64, KbError> {
    let tx = conn.unchecked_transaction()?;
    // EVERY active row with this source, not just the newest: a restored
    // (and so pinned) older generation would otherwise stay active forever
    // beside the new one. The index owns these rows outright, so a pin
    // (which guards against *automatic judges*) does not apply here.
    let existing_ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM memories WHERE source = ?1 AND invalidated_at IS NULL ORDER BY id")?;
        let ids = stmt.query_map(params![source], |r| r.get(0))?.collect::<Result<Vec<i64>, _>>()?;
        ids
    };

    let stability = 6i64 as f64 * 7.0;
    let blob = embedding.map(encode_embedding);
    tx.execute(
        "INSERT INTO memories
            (content, source, project, created_at, reviewed, embedding, importance, stability, valid_from, basis, reflected_at)
         VALUES (?1, ?2, ?3, ?4, 1, ?5, 6, ?6, ?4, ?7, ?4)",
        params![content, source, project, now, blob, stability, BASIS_DERIVED],
    )?;
    let new_id = tx.last_insert_rowid();
    // No mention linking (final-review item 7): an index row is derived
    // prose about code, and linking it would pull it into the entity graph
    // (cards, graph hygiene, evidence-death) that exists for session facts.
    // Occurrence dating is still applied, as for any other row.
    let dates = iso_dates_in(content);
    if let (Some(from), Some(to)) = (dates.first(), dates.last()) {
        let _ = tx.execute("UPDATE memories SET occurred_from = ?1, occurred_to = ?2 WHERE id = ?3", params![from, to, new_id]);
    }

    for old_id in existing_ids {
        supersede(&tx, old_id, new_id, now)?;
    }
    if let Some((project_id, path)) = summary_key {
        set_code_summary_memory_id(&tx, project_id, path, new_id)?;
    }
    tx.commit()?;
    Ok(new_id)
}

/// Tombstones (no successor) every ACTIVE memory whose `source` is exactly
/// `source` -- the orphan-cleanup counterpart of `upsert_index_memory`'s
/// "supersede all by source": a module that no longer exists must not
/// leave a restored older generation of its mirror active. Returns how
/// many rows were invalidated.
pub fn invalidate_index_memories_by_source(conn: &Connection, source: &str, now: &str) -> Result<usize, KbError> {
    let n = conn.execute(
        "UPDATE memories SET invalidated_at = ?1 WHERE source = ?2 AND invalidated_at IS NULL",
        params![now, source],
    )?;
    Ok(n)
}

/// Schema 24: judged name-to-entity verdicts, which are both a cache and
/// an alias table.
///
/// `ENTITY_RESOLUTION_SIM_THRESHOLD` is 0.85 and the highest real entity
/// pair similarity in this bank is 0.8453, so resolution never once matched
/// an existing entity by embedding: every extraction minted a new row.
/// 442 entities for 326 memories, 60% of them with a single mention,
/// `Remos` beside `Remos Space`, `zenithd` beside `Zenith daemon`. An
/// entity graph that exists to connect memories was mostly dead ends.
///
/// Lowering the threshold outright was the wrong fix -- resolution has no
/// judge, so it would silently fuse "GitHub" into "GitHub Actions" with
/// nothing to catch it. Instead the near-miss band gets judged once and the
/// answer is remembered, so a SAME verdict makes that name resolve
/// instantly forever after and a DIFFERENT verdict is never re-asked.
fn migrate_v23_to_v24(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS entity_alias_seen (
            name TEXT NOT NULL,
            entity_id INTEGER NOT NULL,
            same INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (name, entity_id)
        );
        CREATE INDEX IF NOT EXISTS ix_entity_alias_same ON entity_alias_seen (name, same);",
    )?;
    conn.execute("PRAGMA user_version = 24", [])?;
    Ok(())
}

/// The entity a name has already been judged to BE, if any.
///
/// Checked before embedding, so a name resolved once costs no call and no
/// embed on every later extraction that mentions it.
pub fn alias_target(conn: &Connection, name: &str) -> Result<Option<i64>, KbError> {
    Ok(conn
        .query_row(
            "SELECT a.entity_id FROM entity_alias_seen a JOIN entities e ON e.id = a.entity_id
             WHERE a.name = ?1 AND a.same = 1 LIMIT 1",
            params![name.trim().to_lowercase()],
            |r| r.get(0),
        )
        .optional()?)
}

/// A previously judged verdict for this exact (name, entity) pair.
pub fn alias_verdict(conn: &Connection, name: &str, entity_id: i64) -> Result<Option<bool>, KbError> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT same FROM entity_alias_seen WHERE name = ?1 AND entity_id = ?2",
            params![name.trim().to_lowercase(), entity_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.map(|n| n == 1))
}

/// Records one judged (name, entity) verdict.
pub fn mark_alias_verdict(conn: &Connection, name: &str, entity_id: i64, same: bool, now: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT OR REPLACE INTO entity_alias_seen (name, entity_id, same, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name.trim().to_lowercase(), entity_id, same as i64, now],
    )?;
    Ok(())
}

/// Schema 25: the project registry.
///
/// Identity was `basename(cwd)`, so a rename orphaned every
/// `project-index:<old>` memory and letter case alone already orphaned
/// three projects on the live bank. A fingerprint makes rename detection an
/// exact lookup instead of a guess, and the normalized name is what
/// memories are tagged with.
fn migrate_v24_to_v25(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS projects (
            id              INTEGER PRIMARY KEY,
            fingerprint     TEXT NOT NULL UNIQUE,
            name            TEXT NOT NULL,
            root_path       TEXT NOT NULL,
            indexed_head    TEXT,
            indexed_commits INTEGER,
            indexed_at      TEXT,
            card            TEXT,
            card_built_at   TEXT,
            created_at      TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_projects_name ON projects (name);",
    )?;
    conn.execute("PRAGMA user_version = 25", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 25 -> 26 migration: the judge
/// log. One row per `ReflectLlm` call made by reflect, graph audit and
/// ingest (see `reflect::LoggedLlm`): pass tag, model, the exact prompt,
/// the raw reply (NULL on failure) or the error (NULL on success), and
/// latency. Verdicts are deliberately not parsed here — the `*_seen`
/// tables keep only negatives and `superseded_by` has no cause, so this
/// raw log is the only record that yields balanced labels, and re-parsing
/// later with `reflect::parse_*` keeps it exact across parser changes.
fn migrate_v25_to_v26(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS judge_log (
            id         INTEGER PRIMARY KEY,
            created_at TEXT NOT NULL,
            pass       TEXT NOT NULL,
            model      TEXT NOT NULL,
            prompt     TEXT NOT NULL,
            reply      TEXT,
            error      TEXT,
            latency_ms INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_judge_log_pass ON judge_log (pass, created_at);",
    )?;
    conn.execute("PRAGMA user_version = 26", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 26 -> 27 migration: durable
/// per-session engagement verdicts. `ingest_sessions_impl`'s engagement
/// pass already judges ENGAGED vs SHOWN per memory id injected into a
/// session (`ingest::parse_engagement_verdicts`), but previously only kept
/// running `access_count`/`stability` touches on the engaged rows -- the
/// per-session verdict itself, and every SHOWN-but-not-engaged id, was
/// discarded once applied. Worse, the recall-log JSONL that named which
/// ids were shown to a given session is pruned 14 days after ingestion
/// (`should_prune_recall_log`), so nothing durable was left to compute
/// recall precision from. One row per (session, memory) the judge actually
/// returned a verdict for: `engaged` is 0/1, `judged_at` is the ingest
/// run's `now`. `mach kb recall-stats` aggregates this table.
fn migrate_v26_to_v27(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS recall_engagement (
            session_id TEXT NOT NULL,
            memory_id  INTEGER NOT NULL,
            engaged    INTEGER NOT NULL,
            judged_at  TEXT NOT NULL,
            PRIMARY KEY (session_id, memory_id)
        );",
    )?;
    conn.execute("PRAGMA user_version = 27", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 27 -> 28 migration:
/// `memories.pinned_at`.
///
/// `mach kb restore <id>` used to only clear a tombstone, with nothing
/// recorded to tell the nightly reflect pass the row had just been judged
/// by a human. The very next dedupe/contradiction pass re-asked the same
/// pair, got the same verdict, and tombstoned the row again — four
/// restored rows were re-tombstoned within hours of being restored. A
/// pinned row (`restore` sets `pinned_at`; `mach kb unpin` clears it) is
/// never touched by an automatic supersession path: `store::supersede`
/// and `store::merge_and_supersede` themselves stay pin-blind (manual
/// `mach kb supersede` must keep working unguarded), but every automatic
/// caller checks `is_pinned` first and records the pair as judged instead
/// of tombstoning, so it isn't re-asked every run either.
fn migrate_v27_to_v28(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "pinned_at", "TEXT")?;
    conn.execute("PRAGMA user_version = 28", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 28 -> 29 migration:
/// `supersession_audit`.
///
/// Automatic supersession (dedupe's Keep branch, contradiction's
/// Conflict/ConflictRetro, `apply_verdict`'s Supersede) has tombstoned 164
/// rows in the live bank, and spot-checking found several that are lossy:
/// the old row carried a distinct fact -- often a dated one -- that the
/// successor doesn't restate, sometimes several hops down a chain (a row
/// tombstoned into a summary that was itself later tombstoned into a more
/// generic summary). `mach kb audit-supersessions` judges every tombstone
/// against its direct successor and restores the ones that lost
/// information; this table is its durable record, one row per audited hop,
/// keyed on the tombstoned row's own id (`old_id`) so a multi-hop chain
/// (A->B->C) gets one independently-keyed row per hop instead of the
/// second hop colliding with the first. A row already in this table is
/// skipped on the next run unless `--reaudit` -- same "don't re-pay for a
/// verdict" rule as `dedupe_seen`/`insight_dedupe_seen`, except here both
/// OK and LOSSY verdicts are recorded (a pair the judge never addressed at
/// all is not, so it's retried next run, matching every other batched
/// judge in this codebase).
fn migrate_v28_to_v29(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS supersession_audit (
            old_id     INTEGER PRIMARY KEY,
            new_id     INTEGER NOT NULL,
            verdict    TEXT NOT NULL,
            reason     TEXT NOT NULL,
            audited_at TEXT NOT NULL
        );",
    )?;
    conn.execute("PRAGMA user_version = 29", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 29 -> 30 migration:
/// `memories.reflected_at`, replacing the single-watermark
/// `reflect_state.last_memory_id`/`should_advance_watermark` gate with
/// per-memory progress.
///
/// The watermark required an entire `mach kb reflect` run — insight stage,
/// dedupe, contradiction, curation, strength review, graph extraction,
/// graph hygiene, dormancy, insight dedupe, entity cards, meta — to finish
/// with zero `claude` call failures anywhere before it moved at all. Given
/// per-pass error rates of 13-55% (`judge_log`, see the reflection-usage
/// evidence report), that happened on 0 of 43 runs over 7 days, so it froze
/// at `last_memory_id = 196` on 2026-09-08 while the bank kept growing past
/// id 1600 — 1300+ memories the insight stage had never once examined, all
/// invisible to `memories_since`. `reflected_at` tracks the insight stage's
/// own progress per row instead: set only on the exact rows its own
/// working set examined, only when that stage itself (not some unrelated
/// pass) succeeded — see `reflect::should_mark_reflected` and
/// `cli::run_insight_stage`.
///
/// Backfilled so the fix doesn't immediately treat the pre-migration
/// watermark's own progress as unexamined backlog: every id at or below
/// the frozen `reflect_state.last_memory_id` is marked reflected as of that
/// state's own `last_run_at` (falling back to `now` when no run has ever
/// completed). Everything above the old watermark — the very backlog this
/// migration exists to unstick — is left `NULL`, i.e. due.
fn migrate_v29_to_v30(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "reflected_at", "TEXT")?;
    backfill_reflected_at(conn)?;
    conn.execute("PRAGMA user_version = 30", [])?;
    Ok(())
}

/// `PRAGMA user_version`-gated, idempotent 30 -> 31 migration:
/// `insights.revised_at` and `insights.prev_text`.
///
/// Re-verification (`cli::run_insight_stage`'s Step 4) used to have exactly
/// one response to an insight its evidence now contradicts: flag it,
/// forever, at whatever wording it was minted with — the session-start
/// mental model (`mental_model`/`cmd_model`) then keeps rendering that
/// stale belief until a human runs `mach kb insight-forget`. These two
/// columns let it do better: a `REVISE` verdict from the new revise/drop/
/// keep judge (`reflect::build_revise_prompt`/`parse_revise`) rewrites
/// `text` in place via `revise_insight`, stamping `revised_at` and stashing
/// the wording it replaced in `prev_text` (one hop back, same convention as
/// `Memory::superseded_by`'s predecessor chain) so the correction has an
/// audit trail instead of silently overwriting history. Both columns are
/// additive and default `NULL` — an insight never revised looks exactly as
/// it did before this migration.
fn migrate_v30_to_v31(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "insights", "revised_at", "TEXT")?;
    add_column_if_missing(conn, "insights", "prev_text", "TEXT")?;
    conn.execute("PRAGMA user_version = 31", [])?;
    Ok(())
}

/// Schema 32: the code index (`crate::code_index`; design doc
/// `docs/superpowers/specs/2026-09-23-code-index-design.md`). Additive
/// only: two columns on `projects` recording the last commit the index is
/// current with, plus the per-directory scope table, per-file indexing
/// state, and the chunks themselves with an FTS5 index over them -- the
/// code analogue of `transcript_chunks`/`transcript_chunks_fts` (schema
/// 22-23) above, and kept in sync the same explicit-insert-and-delete way,
/// no triggers (see the "Code index" section preceding `migrate_v23_to_v24`).
fn migrate_v31_to_v32(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "projects", "code_indexed_head", "TEXT")?;
    add_column_if_missing(conn, "projects", "code_indexed_at", "TEXT")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_scope (
            project_id INTEGER NOT NULL,
            dir TEXT NOT NULL,
            category TEXT NOT NULL,
            source TEXT NOT NULL CHECK (source IN ('llm','user')),
            decided_at TEXT NOT NULL,
            PRIMARY KEY (project_id, dir)
        );
        CREATE TABLE IF NOT EXISTS code_files (
            project_id INTEGER NOT NULL,
            path TEXT NOT NULL,
            blob TEXT NOT NULL,
            lang TEXT,
            lines INTEGER NOT NULL,
            status TEXT NOT NULL CHECK (status IN ('indexed','dirty','fallback','skipped')),
            indexed_at TEXT NOT NULL,
            PRIMARY KEY (project_id, path)
        );
        CREATE TABLE IF NOT EXISTS code_chunks (
            id INTEGER PRIMARY KEY,
            project_id INTEGER NOT NULL,
            path TEXT NOT NULL,
            symbol TEXT,
            kind TEXT NOT NULL,
            scope TEXT,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL,
            text TEXT NOT NULL,
            header TEXT,
            embedding BLOB,
            content_hash TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_code_chunks_file ON code_chunks (project_id, path);
        CREATE VIRTUAL TABLE IF NOT EXISTS code_chunks_fts USING fts5(
            header, text, symbol, content='code_chunks', content_rowid='id'
        );",
    )?;
    conn.execute("PRAGMA user_version = 32", [])?;
    Ok(())
}

/// Schema 33: code-index summaries (phase 2; design doc's "Summaries
/// (phase 2) — revised 2026-09-24" section, `crate::code_index`).
/// Additive only: one new table, `code_summaries`, holding file/module/repo
/// summary text for a project, plus an FTS5 index over it, kept in sync the
/// same explicit-insert-and-delete way as `code_chunks_fts` (no triggers).
///
/// Unlike `code_chunks`, this table has no surrogate integer id -- its
/// natural key IS the app-level identity (`project_id`, `path`: a file path
/// for `level = 'file'`, `"dir/"` for a module, `"/"` for the repo), so
/// `code_summaries_fts` indexes off SQLite's implicit `rowid` instead of a
/// declared column, exactly the way any ordinary rowid table can be an
/// FTS5 `content` table without naming that column itself.
///
/// `stale`/`stale_since` track a summary whose inputs (a child file/module,
/// or the file's own blob) changed since it was last generated -- see
/// `mark_code_summaries_stale`. `memory_id` is set only for module/repo
/// summaries, which are additionally mirrored into `memories` as
/// index-owned rows (`upsert_index_memory`); file summaries never get one
/// (per the design doc, "File summaries never become memories").
fn migrate_v32_to_v33(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_summaries (
            project_id INTEGER NOT NULL,
            path TEXT NOT NULL,
            level TEXT NOT NULL CHECK (level IN ('file','module','repo')),
            text TEXT NOT NULL,
            embedding BLOB,
            source_digest TEXT NOT NULL,
            stale INTEGER NOT NULL DEFAULT 0,
            stale_since TEXT,
            memory_id INTEGER,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (project_id, path)
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS code_summaries_fts USING fts5(
            text, path, content='code_summaries', content_rowid='rowid'
        );",
    )?;
    conn.execute("PRAGMA user_version = 33", [])?;
    Ok(())
}

/// Schema 34: fix-list items 1 and 4 (phase 2 pre-run fixes, see
/// `docs/superpowers/plans/2026-09-24-code-index-phase2.md`'s fix wave).
/// Additive only: `code_files.summary_attempts` counts unparseable-summary
/// attempts for a file's CURRENT blob (`code_file_upsert` resets it to 0
/// whenever the blob changes) so `code_index::job::run_summary_pass` can
/// stop queuing a file after `SUMMARY_GIVE_UP_ATTEMPTS` (2) straight
/// successes-with-no-`SUMMARY:`-line, without retrying it forever (fix 1).
/// No corresponding column is needed on `code_summaries` for fix 4's
/// "first seen" tracking -- a module/repo that has never had a row uses a
/// placeholder row instead (`code_summary_seed_placeholder`), reusing the
/// existing `stale`/`stale_since` columns rather than adding a new one.
fn migrate_v33_to_v34(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "code_files", "summary_attempts", "INTEGER NOT NULL DEFAULT 0")?;
    conn.execute("PRAGMA user_version = 34", [])?;
    Ok(())
}

/// Schema 35: phase 3 of the code index -- the symbol graph (design doc's
/// task-1 brief, `docs/superpowers/plans/2026-09-24-code-index-phase3.md`).
/// Additive only, three new tables:
///
/// - `code_symbols`: one row per definition `code_index::chunk::extract_refs`
///   finds (it reuses `chunk_file`'s own parse and scope logic -- see that
///   function's doc comment -- so there is exactly one grammar/scope pass
///   for both chunking and the symbol graph, not two drifting apart).
///   `qualified` is `scope` + the language's separator + `name`, matching
///   what a human would write to reference the symbol from outside its
///   scope (`Foo::bar`, `mod.Class.method`).
/// - `code_edges`: one row per reference (call/include/import/inherits)
///   the same extraction pass finds. `src_symbol_id` is resolved
///   immediately by `replace_file_symbols` (the enclosing definition by
///   line range, within the same file being replaced); `dst_symbol_id`
///   starts NULL and is resolved separately, after a batch of files
///   reindexes, by `resolve_edges` -- a callee can live in a file this
///   pass never touched. `kind`'s CHECK constraint is the same enum the
///   brief specifies; nothing else in this schema enforces vocabulary this
///   tightly, but a stray edge kind would silently break every consumer
///   that matches on the four literal strings.
/// - `code_history`: created here so the schema is ready, but not written
///   to until Task 3's monthly history job -- see `code_history_get`/
///   `code_history_upsert`/`code_history_list` below.
///
/// Amended in place by the phase-3 final-review fix wave (v35 was never
/// live outside scratch copies), so it stays idempotent and additive:
/// `migrate` re-runs it on every open of a v35 database (`version <= 35`),
/// and every addition below is `IF NOT EXISTS` / `add_column_if_missing`,
/// so a scratch copy created by the first v35 cut upgrades in place.
/// - indexes `code_symbols(project_id, qualified)`, `code_edges(project_id,
///   dst_symbol_id)`, `code_edges(project_id, src_symbol_id)`,
///   `code_edges(project_id, dst_path)` -- `resolve_edges`'s qualified
///   lookup, the null-out-incoming-edges update on replace/delete, the
///   callees query, and the include-target null-out on delete/move.
/// - `code_files.refs_blob`: the blob `extract_refs` last ran over for this
///   path (set by `replace_file_symbols_for_blob`). The symbol backfill
///   selects `refs_blob IS NULL OR refs_blob != blob`, so a file that
///   legitimately yields zero symbols is backfilled exactly once.
/// - `code_edges.dst_path`: the resolved repo-relative path of an
///   `includes`/`imports` edge; `dst_name` stays exactly as written.
/// - `code_history.files`: newline-separated top-churn paths of the month,
///   rendered by the ask `history` tool as `path@<last7>`.
/// - `code_graph_pending`: names / source paths / file paths touched since
///   the last `resolve_edges*` pass, written inside the same transaction as
///   the symbol/edge mutation -- what makes resolution incremental (and
///   crash-safe: a run that dies before resolving leaves the work queued).
fn migrate_v34_to_v35(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS code_symbols (
            id INTEGER PRIMARY KEY,
            project_id INTEGER NOT NULL,
            path TEXT NOT NULL,
            name TEXT NOT NULL,
            qualified TEXT NOT NULL,
            kind TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_code_symbols_name ON code_symbols (project_id, name);
        CREATE INDEX IF NOT EXISTS ix_code_symbols_file ON code_symbols (project_id, path);
        CREATE TABLE IF NOT EXISTS code_edges (
            project_id INTEGER NOT NULL,
            src_symbol_id INTEGER,
            src_path TEXT NOT NULL,
            src_line INTEGER NOT NULL,
            dst_name TEXT NOT NULL,
            dst_symbol_id INTEGER,
            kind TEXT NOT NULL CHECK (kind IN ('calls','includes','imports','inherits'))
        );
        CREATE INDEX IF NOT EXISTS ix_code_edges_dst ON code_edges (project_id, dst_name);
        CREATE INDEX IF NOT EXISTS ix_code_edges_src ON code_edges (project_id, src_path);
        CREATE TABLE IF NOT EXISTS code_history (
            project_id INTEGER NOT NULL,
            month TEXT NOT NULL,
            text TEXT NOT NULL,
            commits INTEGER NOT NULL,
            last_sha TEXT NOT NULL,
            digest TEXT NOT NULL,
            memory_id INTEGER,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (project_id, month)
        );
        CREATE INDEX IF NOT EXISTS ix_code_symbols_qualified ON code_symbols (project_id, qualified);
        CREATE INDEX IF NOT EXISTS ix_code_edges_dst_symbol ON code_edges (project_id, dst_symbol_id);
        CREATE INDEX IF NOT EXISTS ix_code_edges_src_symbol ON code_edges (project_id, src_symbol_id);
        CREATE TABLE IF NOT EXISTS code_graph_pending (
            project_id INTEGER NOT NULL,
            kind TEXT NOT NULL CHECK (kind IN ('name','src','file')),
            value TEXT NOT NULL,
            PRIMARY KEY (project_id, kind, value)
        ) WITHOUT ROWID;",
    )?;
    add_column_if_missing(conn, "code_files", "refs_blob", "TEXT")?;
    add_column_if_missing(conn, "code_edges", "dst_path", "TEXT")?;
    add_column_if_missing(conn, "code_history", "files", "TEXT")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS ix_code_edges_dst_path ON code_edges (project_id, dst_path);")?;
    conn.execute("PRAGMA user_version = 35", [])?;
    Ok(())
}

/// Backfill helper for `migrate_v29_to_v30` (also exposed for tests, same
/// convention as `backfill_occurrence_from_text`): marks every memory with
/// `id <= reflect_state.last_memory_id` reflected as of
/// `reflect_state.last_run_at` (or `now` when that's unset, i.e. a database
/// that never completed a run). A no-op (returns `Ok(0)`) when the
/// watermark was never set — a fresh database has no pre-migration progress
/// to preserve, so leaving every row `NULL` is already correct. Idempotent:
/// only touches rows still `NULL`, so re-running it (or running it against
/// a database where `reflected_at` already has values from normal use)
/// never clobbers a real timestamp with the migration's own fallback.
pub fn backfill_reflected_at(conn: &Connection) -> Result<usize, KbError> {
    let state = get_reflect_state(conn)?;
    let last_id = match state.last_memory_id {
        Some(id) if id > 0 => id,
        _ => return Ok(0),
    };
    let ts = state.last_run_at.unwrap_or_else(now_rfc3339);
    Ok(conn.execute(
        "UPDATE memories SET reflected_at = ?1 WHERE id <= ?2 AND reflected_at IS NULL",
        params![ts, last_id],
    )?)
}

/// One registered project.
#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub id: i64,
    pub fingerprint: String,
    pub name: String,
    pub root_path: String,
    pub indexed_head: Option<String>,
    pub indexed_commits: Option<i64>,
    pub indexed_at: Option<String>,
    pub card: Option<String>,
    pub card_built_at: Option<String>,
}

fn row_to_project(r: &rusqlite::Row) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get("id")?,
        fingerprint: r.get("fingerprint")?,
        name: r.get("name")?,
        root_path: r.get("root_path")?,
        indexed_head: r.get("indexed_head")?,
        indexed_commits: r.get("indexed_commits")?,
        indexed_at: r.get("indexed_at")?,
        card: r.get("card")?,
        card_built_at: r.get("card_built_at")?,
    })
}

pub fn get_project_by_fingerprint(conn: &Connection, fingerprint: &str) -> Result<Option<ProjectRow>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM projects WHERE fingerprint = ?1")?;
    let mut rows = stmt.query_map(params![fingerprint], |r| row_to_project(r))?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Memories that belong to a project, either by the `project` column or by
/// the `project-index:<name>` source string. Returns a real error rather
/// than silently defaulting to 0 on a query failure -- `mach kb projects
/// refresh` used to treat any SQL error here as "no memories", silently
/// skipping the project instead of registering it.
pub fn count_project_memories(conn: &Connection, name: &str) -> Result<i64, KbError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE project = ?1 OR source = ?2",
        params![name, format!("project-index:{}", name)],
        |r| r.get(0),
    )?)
}

/// Looks a project up by its normalized name, using `ix_projects_name`
/// instead of pulling every row and scanning linearly -- the pattern four
/// call sites used before this existed.
pub fn get_project_by_name(conn: &Connection, name: &str) -> Result<Option<ProjectRow>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM projects WHERE name = ?1 LIMIT 1")?;
    let mut rows = stmt.query_map(params![name], |r| row_to_project(r))?;
    match rows.next() {
        Some(r) => Ok(Some(r?)),
        None => Ok(None),
    }
}

/// Resolves a filesystem path to the most specific registered project whose
/// `root_path` contains it -- the path equals the root, or the root is a
/// directory prefix of it (`path == root || path.starts_with(root + "/")`,
/// never a raw string prefix: `/ab` must not match root `/a`). When more
/// than one registered root contains the path (a project nested inside
/// another's tree), the longest root wins as the more specific match.
///
/// Table-scans `projects` rather than a `LIKE`/`GLOB` query: the registry
/// is a handful of rows (one per project this user has ever indexed), so a
/// linear scan comparing real path components is simpler and safer than an
/// SQL pattern that would need its own escaping for `_`/`%` in a path.
pub fn project_for_path(conn: &Connection, path: &str) -> Result<Option<ProjectRow>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM projects")?;
    let rows = stmt.query_map([], |r| row_to_project(r))?;
    let mut best: Option<ProjectRow> = None;
    for row in rows {
        let row = row?;
        let root = row.root_path.trim_end_matches('/');
        if root.is_empty() {
            continue;
        }
        let matches = path == root || path.starts_with(&format!("{}/", root));
        if !matches {
            continue;
        }
        let better = match &best {
            Some(b) => root.len() > b.root_path.trim_end_matches('/').len(),
            None => true,
        };
        if better {
            best = Some(row);
        }
    }
    Ok(best)
}

pub fn list_projects(conn: &Connection) -> Result<Vec<ProjectRow>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM projects ORDER BY name ASC")?;
    let rows = stmt.query_map([], |r| row_to_project(r))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Registers or updates a project. Returns its id and, when the same
/// fingerprint was already registered under a different name, that previous
/// name — which is exactly a rename (or a case change) and the caller's
/// signal to re-tag.
pub fn upsert_project(
    conn: &Connection,
    fingerprint: &str,
    name: &str,
    root_path: &str,
    now: &str,
) -> Result<(i64, Option<String>), KbError> {
    if let Some(existing) = get_project_by_fingerprint(conn, fingerprint)? {
        let renamed = if existing.name != name { Some(existing.name.clone()) } else { None };
        conn.execute(
            "UPDATE projects SET name = ?1, root_path = ?2 WHERE id = ?3",
            params![name, root_path, existing.id],
        )?;
        return Ok((existing.id, renamed));
    }
    conn.execute(
        "INSERT INTO projects (fingerprint, name, root_path, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![fingerprint, name, root_path, now],
    )?;
    Ok((conn.last_insert_rowid(), None))
}

/// Moves every reference to a project from `old` to `new`.
///
/// Both writes are required: `project` drives recall's project-anchored
/// expansion, and the `project-index:<name>` source string is what the
/// index-project skill filters on to pull the current index. Leaving either
/// stale orphans the index in a different way. Returns rows changed.
pub fn retag_project(conn: &Connection, old: &str, new: &str) -> Result<usize, KbError> {
    let mut moved = conn.execute(
        "UPDATE memories SET project = ?1 WHERE project = ?2",
        params![new, old],
    )?;
    moved += conn.execute(
        "UPDATE memories SET source = ?1 WHERE source = ?2",
        params![format!("project-index:{}", new), format!("project-index:{}", old)],
    )?;
    Ok(moved)
}

/// Replaces a project's derivable card. Rebuilding a card is not indexing,
/// so this deliberately does not touch the interpretive watermark.
///
/// Does not touch `card_built_at`: confirmed dead in the final review
/// (written, never read anywhere) and deliberately left unwritten rather
/// than populated -- the column stays in the schema (migrations here are
/// additive only) but nothing sets it any more.
pub fn set_project_card(conn: &Connection, id: i64, card: &str) -> Result<(), KbError> {
    conn.execute("UPDATE projects SET card = ?1 WHERE id = ?2", params![card, id])?;
    Ok(())
}

/// Deletes a project's registry row -- the tracking entry only. Memories
/// tagged `project = <name>` or `source = project-index:<name>` are
/// knowledge, not registry state, and are left exactly as they are; the
/// caller is expected to report how many of them remain so a `forget` reads
/// as "the drift tracker stopped watching this directory", not "the index
/// is gone". Returns whether a row was actually removed.
pub fn forget_project(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let n = conn.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// Records that the interpretive index ran, resetting the drift watermark.
///
/// Does not touch `indexed_head`: confirmed dead in the final review
/// (written, never read anywhere) and deliberately left unwritten rather
/// than populated -- same treatment as `card_built_at` in
/// `set_project_card`, for the same reason.
pub fn mark_project_indexed(conn: &Connection, id: i64, commits: Option<i64>, now: &str) -> Result<(), KbError> {
    conn.execute(
        "UPDATE projects SET indexed_commits = ?1, indexed_at = ?2 WHERE id = ?3",
        params![commits, now, id],
    )?;
    Ok(())
}

/// Same as `mark_project_indexed`, but refuses when the project is
/// git-backed and its current commit count could not be determined (root
/// moved, deleted, or otherwise unreadable). Writing a NULL commit count in
/// that case would permanently disable commit-based drift for the project
/// while leaving no visible sign anything went wrong -- `projects list`
/// would report it freshly indexed. Returns whether the watermark was
/// actually written; on `false`, the row (including `indexed_at`) is left
/// exactly as it was.
pub fn mark_project_indexed_if_readable(
    conn: &Connection,
    id: i64,
    is_git: bool,
    commits: Option<i64>,
    now: &str,
) -> Result<bool, KbError> {
    if is_git && commits.is_none() {
        return Ok(false);
    }
    mark_project_indexed(conn, id, commits, now)?;
    Ok(true)
}

/// Passages still missing an embedding, oldest first.
///
/// Needed because embeddings arrived after the index did (schema 23). A
/// plain re-index cannot fix those rows: `transcript_file_current` skips a
/// file whose mtime and size are unchanged, so every already-indexed file
/// would be passed over and its NULL embeddings would persist forever.
/// `--all` would work but re-reads 962MB to recompute what is already
/// correct. This targets exactly the gap.
pub fn transcript_chunks_missing_embedding(conn: &Connection, cap: usize) -> Result<Vec<(i64, String)>, KbError> {
    let mut stmt =
        conn.prepare("SELECT id, text FROM transcript_chunks WHERE embedding IS NULL ORDER BY id ASC LIMIT ?1")?;
    let rows = stmt.query_map(params![cap as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Attaches an embedding to one already-indexed passage.
pub fn set_transcript_chunk_embedding(conn: &Connection, id: i64, embedding: &[f32]) -> Result<(), KbError> {
    conn.execute(
        "UPDATE transcript_chunks SET embedding = ?1 WHERE id = ?2",
        params![encode_embedding(embedding), id],
    )?;
    Ok(())
}

/// Counts of what the transcript index currently holds.
pub fn transcript_index_stats(conn: &Connection) -> Result<(i64, i64), KbError> {
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM transcript_files", [], |r| r.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM transcript_chunks", [], |r| r.get(0))?;
    Ok((files, chunks))
}

/// Folds active relation rows that state the same claim into one, moving
/// their evidence onto the survivor. Returns how many rows were folded
/// away. Deterministic, no LLM; safe to re-run.
pub fn fold_duplicate_relations(conn: &Connection, now: &str) -> Result<usize, KbError> {
    let mut stmt = conn.prepare(
        "SELECT id, src, predicate, dst FROM relations WHERE invalidated_at IS NULL ORDER BY id ASC",
    )?;
    let rows: Vec<(i64, i64, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut survivor: HashMap<(i64, String, i64), i64> = HashMap::new();
    let mut folded = 0usize;
    for (id, src, predicate, dst) in rows {
        let key = (src, predicate.to_lowercase(), dst);
        match survivor.get(&key) {
            Some(&keep) => {
                conn.execute(
                    "INSERT OR IGNORE INTO relation_evidence (relation_id, memory_id, created_at)
                     SELECT ?1, memory_id, created_at FROM relation_evidence WHERE relation_id = ?2",
                    params![keep, id],
                )?;
                conn.execute("DELETE FROM relation_evidence WHERE relation_id = ?1", params![id])?;
                conn.execute(
                    "UPDATE relations SET invalidated_at = ?1, superseded_by = ?2 WHERE id = ?3",
                    params![now, keep, id],
                )?;
                folded += 1;
            }
            None => {
                survivor.insert(key, id);
            }
        }
    }
    Ok(folded)
}

/// Records that memory `memory_id` is evidence for relation `relation_id`.
pub fn add_relation_evidence(conn: &Connection, relation_id: i64, memory_id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO relation_evidence (relation_id, memory_id, created_at) VALUES (?1, ?2, ?3)",
        params![relation_id, memory_id, now],
    )?;
    Ok(n > 0)
}

/// Every memory backing a relation, oldest link first.
pub fn relation_evidence(conn: &Connection, relation_id: i64) -> Result<Vec<i64>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT memory_id FROM relation_evidence WHERE relation_id = ?1 ORDER BY created_at ASC, memory_id ASC",
    )?;
    let rows = stmt.query_map(params![relation_id], |r| r.get::<_, i64>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// The ACTIVE relation stating exactly this claim, if one exists. Lets
/// extraction attach evidence to a known claim instead of inserting a
/// second row for it.
pub fn active_relation_for_claim(
    conn: &Connection,
    src: i64,
    predicate: &str,
    dst: i64,
) -> Result<Option<i64>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM relations
         WHERE invalidated_at IS NULL AND src = ?1 AND dst = ?3 AND predicate = ?2 COLLATE NOCASE
         ORDER BY id ASC LIMIT 1",
    )?;
    Ok(stmt.query_row(params![src, predicate, dst], |r| r.get(0)).optional()?)
}

fn migrate_v19_to_v20(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS insight_dedupe_seen (
            id_a INTEGER NOT NULL,
            id_b INTEGER NOT NULL,
            PRIMARY KEY (id_a, id_b)
        );",
    )?;
    conn.execute("PRAGMA user_version = 20", [])?;
    Ok(())
}

fn migrate_v18_to_v19(conn: &Connection) -> Result<(), KbError> {
    add_column_if_missing(conn, "memories", "occurred_from", "TEXT")?;
    add_column_if_missing(conn, "memories", "occurred_to", "TEXT")?;
    backfill_occurrence_from_text(conn)?;
    conn.execute("PRAGMA user_version = 19", [])?;
    Ok(())
}

/// Fills `occurred_from`/`occurred_to` from ISO dates written in the text,
/// for rows that have none. A memory saying "as of 2026-09-07" or "shipped
/// 2026-08-31" is dated by its own content; the earliest and latest date it
/// mentions bound the occurrence. Rows with no date in the text are left
/// NULL rather than defaulted to `created_at`, because "I do not know when
/// this happened" and "this happened the day it was written" are different
/// claims and only one of them is true.
pub fn backfill_occurrence_from_text(conn: &Connection) -> Result<usize, KbError> {
    let mut stmt = conn.prepare(
        "SELECT id, content FROM memories WHERE occurred_from IS NULL AND occurred_to IS NULL",
    )?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut n = 0;
    for (id, content) in rows {
        let dates = iso_dates_in(&content);
        if dates.is_empty() {
            continue;
        }
        let from = dates.first().unwrap();
        let to = dates.last().unwrap();
        conn.execute(
            "UPDATE memories SET occurred_from = ?1, occurred_to = ?2 WHERE id = ?3",
            params![from, to, id],
        )?;
        n += 1;
    }
    Ok(n)
}

/// Every `YYYY-MM-DD` in `text`, sorted and deduped. Deliberately strict:
/// a looser matcher would read version numbers, IP addresses and NORAD ids
/// as dates.
///
/// A candidate span is rejected unless it sits on a real boundary on both
/// sides: the byte immediately before must be start-of-text or NOT
/// alphanumeric/`-`/`_`/`/`, and the byte immediately after must be
/// end-of-text or NOT alphanumeric/`-`/`_`/`/` — otherwise a date-shaped
/// span embedded in an identifier or path reads as a date it isn't
/// (`"TICKET-2026-01-15-hotfix"`, `".../reports/2026-01-15/summary"`,
/// `"req-2026-01-15-abcxyz"`). The one exception: a following `T` then a
/// digit (`"2026-01-15T10:00:00Z"`, an ISO timestamp) still counts — that
/// really is a date, just with a time attached.
pub fn iso_dates_in(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let digits = |i: usize, n: usize| -> bool {
        i + n <= b.len() && b[i..i + n].iter().all(|c| c.is_ascii_digit())
    };
    let is_word_byte = |c: u8| c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'/';
    let mut i = 0usize;
    while i + 10 <= b.len() {
        if digits(i, 4) && b[i + 4] == b'-' && digits(i + 5, 2) && b[i + 7] == b'-' && digits(i + 8, 2) {
            // reject a longer identifier/path running into either side
            // (e.g. "12026-01-01", "TICKET-2026-01-15-hotfix") -- unless
            // what follows is really an ISO timestamp's "T10:00:00Z".
            let left_ok = i == 0 || !is_word_byte(b[i - 1]);
            let is_timestamp = i + 11 < b.len() && b[i + 10] == b'T' && b[i + 11].is_ascii_digit();
            let right_ok = i + 10 == b.len() || is_timestamp || !is_word_byte(b[i + 10]);
            if left_ok && right_ok {
                let d = &text[i..i + 10];
                let month: u32 = text[i + 5..i + 7].parse().unwrap_or(0);
                let day: u32 = text[i + 8..i + 10].parse().unwrap_or(0);
                if (1..=12).contains(&month) && (1..=31).contains(&day) && !out.iter().any(|x| x == d) {
                    out.push(d.to_string());
                }
                i += 10;
                continue;
            }
        }
        i += 1;
    }
    out.sort();
    out
}

fn migrate_v17_to_v18(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "DROP TRIGGER IF EXISTS memories_fts_ai;
         DROP TRIGGER IF EXISTS memories_fts_ad;
         DROP TRIGGER IF EXISTS memories_fts_au;
         DROP TABLE IF EXISTS memories_fts;",
    )?;
    ensure_fts(conn)?;
    conn.execute("INSERT INTO memories_fts(memories_fts) VALUES('rebuild')", [])?;
    conn.execute("PRAGMA user_version = 18", [])?;
    Ok(())
}

fn migrate_v16_to_v17(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS entity_cards (
            entity_id INTEGER PRIMARY KEY,
            text TEXT NOT NULL,
            source_ids TEXT NOT NULL,
            embedding BLOB,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            built_watermark INTEGER NOT NULL,
            mention_count INTEGER NOT NULL
        );",
    )?;
    conn.execute("PRAGMA user_version = 17", [])?;
    Ok(())
}

/// True when `table` exists in this database. Migrations that read a table
/// another migration (or `init_schema`) normally provides must tolerate its
/// absence: a hand-built older-era database in a test, or a repair run on a
/// partial file, legitimately lacks it.
fn table_exists(conn: &Connection, table: &str) -> Result<bool, KbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
        params![table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// The v16 backfill body, also reusable as a repair (`mach kb graph
/// relink`, if ever wanted). Returns the number of mention rows inserted.
/// Each source is skipped when its table is absent, so this is safe on a
/// partial database.
pub fn backfill_mentions(conn: &Connection, now: &str) -> Result<usize, KbError> {
    let mut n = 0usize;
    if !table_exists(conn, "memory_entities")? || !table_exists(conn, "memories")? || !table_exists(conn, "entities")? {
        return Ok(0);
    }
    // (a) evidence pointers: every active relation's evidence memory mentions src and dst
    if table_exists(conn, "relations")? {
        let mut stmt =
            conn.prepare("SELECT evidence_memory_id, src, dst FROM relations WHERE evidence_memory_id IS NOT NULL")?;
        let rows: Vec<(i64, i64, i64)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
        for (mid, src, dst) in rows {
            // A dangling evidence pointer (memory or entity already gone) must
            // not abort the backfill -- the FK-less schema allows it.
            if get(conn, mid)?.is_none() {
                continue;
            }
            for eid in [src, dst] {
                if get_entity(conn, eid)?.is_some() {
                    n += link_mention(conn, mid, eid, MENTION_SOURCE_BACKFILL, now)? as usize;
                }
            }
        }
    }
    // (b) name scan of every memory against every entity name
    let entities = all_entities(conn)?;
    let mut stmt = conn.prepare("SELECT id, content FROM memories")?;
    let mems: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    for (mid, content) in &mems {
        for e in &entities {
            if name_mentioned(content, &e.name) {
                n += link_mention(conn, *mid, e.id, MENTION_SOURCE_BACKFILL, now)? as usize;
            }
        }
    }
    Ok(n)
}

fn migrate_v6_to_v7(conn: &Connection) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "CREATE TABLE memories_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            source TEXT,
            project TEXT,
            created_at TEXT NOT NULL,
            reviewed INTEGER NOT NULL DEFAULT 1,
            embedding BLOB,
            importance INTEGER NOT NULL DEFAULT 5,
            stability REAL,
            access_count INTEGER NOT NULL DEFAULT 0,
            first_accessed_at TEXT,
            last_accessed_at TEXT,
            valid_from TEXT,
            invalidated_at TEXT,
            superseded_by INTEGER,
            dormant_at TEXT,
            last_verified_at TEXT
        );
        INSERT INTO memories_new
            (id, content, source, project, created_at, reviewed, embedding, importance, stability,
             access_count, first_accessed_at, last_accessed_at, valid_from, invalidated_at,
             superseded_by, dormant_at, last_verified_at)
        SELECT
            id, content, source, project, created_at, reviewed, embedding, importance, stability,
            access_count, first_accessed_at, last_accessed_at, valid_from, invalidated_at,
            superseded_by, dormant_at, last_verified_at
        FROM memories ORDER BY id;
        DROP TABLE memories;
        ALTER TABLE memories_new RENAME TO memories;

        CREATE TABLE insights_new (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            confidence REAL NOT NULL DEFAULT 0.5,
            source_ids TEXT NOT NULL,
            embedding BLOB,
            invalidated_at TEXT,
            flagged_at TEXT,
            last_verified_at TEXT,
            level INTEGER NOT NULL DEFAULT 1
        );
        INSERT INTO insights_new
            (id, text, created_at, confidence, source_ids, embedding, invalidated_at, flagged_at,
             last_verified_at, level)
        SELECT
            id, text, created_at, confidence, source_ids, embedding, invalidated_at, flagged_at,
            last_verified_at, level
        FROM insights ORDER BY id;
        DROP TABLE insights;
        ALTER TABLE insights_new RENAME TO insights;

        DELETE FROM dedupe_seen
            WHERE id_a NOT IN (SELECT id FROM memories) OR id_b NOT IN (SELECT id FROM memories);
        DELETE FROM contradiction_seen
            WHERE id_a NOT IN (SELECT id FROM memories) OR id_b NOT IN (SELECT id FROM memories);",
    )?;
    tx.execute("PRAGMA user_version = 7", [])?;
    tx.commit()?;
    Ok(())
}

/// Runs every migration step whose version gate hasn't been cleared yet.
/// Runs on every `open`, but the version gate makes every call after the
/// first one a single cheap `PRAGMA` read.
/// Every column `Memory` reads, with its `ALTER TABLE ADD COLUMN`
/// declaration. Applied by `ensure_memory_columns` BEFORE any version-gated
/// step runs.
///
/// Why up front rather than each in its own migration: several migrations
/// read memory ROWS (the v16 mention backfill, the v19 occurrence
/// backfill), and reading a row goes through `row_to_memory`, which needs
/// every column the current struct declares. A column added at v19 is
/// therefore already required at v16, so column creation cannot be ordered
/// by version at all. Idempotent, so this is safe to run on every open.
const MEMORY_COLUMNS: &[(&str, &str)] = &[
    ("dormant_at", "TEXT"),
    ("last_verified_at", "TEXT"),
    ("graph_extracted_at", "TEXT"),
    ("basis", "TEXT"),
    ("occurred_from", "TEXT"),
    ("occurred_to", "TEXT"),
    ("pinned_at", "TEXT"),
];

/// The schema version a fully migrated database lands on. Tests assert
/// against this rather than a literal: every schema addition used to
/// require hunting down a dozen hard-coded version numbers across the
/// migration tests, which is busywork that also invites getting one wrong.
pub const SCHEMA_VERSION: i64 = 35;

fn ensure_memory_columns(conn: &Connection) -> Result<(), KbError> {
    if !table_exists(conn, "memories")? {
        return Ok(());
    }
    for (name, decl) in MEMORY_COLUMNS {
        add_column_if_missing(conn, "memories", name, decl)?;
    }
    Ok(())
}

fn migrate(conn: &Connection) -> Result<(), KbError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    // Columns first: a later migration's column can be needed by an earlier
    // migration's row reads (see `MEMORY_COLUMNS`).
    ensure_memory_columns(conn)?;
    if version < 1 {
        migrate_v0_to_v1(conn)?;
    }
    if version < 2 {
        migrate_v1_to_v2(conn)?;
    }
    if version < 3 {
        migrate_v2_to_v3(conn)?;
    }
    if version < 4 {
        migrate_v3_to_v4(conn)?;
    }
    if version < 5 {
        migrate_v4_to_v5(conn)?;
    }
    if version < 6 {
        migrate_v5_to_v6(conn)?;
    }
    if version < 7 {
        migrate_v6_to_v7(conn)?;
    }
    if version < 8 {
        migrate_v7_to_v8(conn)?;
    }
    if version < 9 {
        migrate_v8_to_v9(conn)?;
    }
    if version < 10 {
        migrate_v9_to_v10(conn)?;
    }
    if version < 11 {
        migrate_v10_to_v11(conn)?;
    }
    if version < 12 {
        migrate_v11_to_v12(conn)?;
    }
    if version < 13 {
        migrate_v12_to_v13(conn)?;
    }
    if version < 14 {
        migrate_v13_to_v14(conn)?;
    }
    if version < 15 {
        migrate_v14_to_v15(conn)?;
    }
    if version < 16 {
        migrate_v15_to_v16(conn)?;
    }
    if version < 17 {
        migrate_v16_to_v17(conn)?;
    }
    if version < 18 {
        migrate_v17_to_v18(conn)?;
    }
    if version < 19 {
        migrate_v18_to_v19(conn)?;
    }
    if version < 20 {
        migrate_v19_to_v20(conn)?;
    }
    if version < 21 {
        migrate_v20_to_v21(conn)?;
    }
    if version < 22 {
        migrate_v21_to_v22(conn)?;
    }
    if version < 23 {
        migrate_v22_to_v23(conn)?;
    }
    if version < 24 {
        migrate_v23_to_v24(conn)?;
    }
    if version < 25 {
        migrate_v24_to_v25(conn)?;
    }
    if version < 26 {
        migrate_v25_to_v26(conn)?;
    }
    if version < 27 {
        migrate_v26_to_v27(conn)?;
    }
    if version < 28 {
        migrate_v27_to_v28(conn)?;
    }
    if version < 29 {
        migrate_v28_to_v29(conn)?;
    }
    if version < 30 {
        migrate_v29_to_v30(conn)?;
    }
    if version < 31 {
        migrate_v30_to_v31(conn)?;
    }
    if version < 32 {
        migrate_v31_to_v32(conn)?;
    }
    if version < 33 {
        migrate_v32_to_v33(conn)?;
    }
    if version < 34 {
        migrate_v33_to_v34(conn)?;
    }
    // `<=`, not `<`: v35 was amended in place (final-review fix wave) and
    // is idempotent, so a database already at 35 re-runs it to pick up the
    // added columns/indexes/table. Cheap: every statement is IF NOT EXISTS
    // or an ALTER that no-ops on "duplicate column name".
    if version <= 35 {
        migrate_v34_to_v35(conn)?;
    }
    Ok(())
}

/// Opens the default store, creating `~/.local/share/mach` and the schema
/// if needed.
pub fn open() -> Result<Connection, KbError> {
    open_with_path(&db_path()?)
}

/// Opens a store at an arbitrary path (creating parent dirs and schema),
/// so tests and tools can point at a scratch file — or `:memory:` — instead
/// of the real database.
pub fn open_with_path(path: &Path) -> Result<Connection, KbError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    // Phase 3 adds concurrent writers (hooks running alongside interactive
    // CLI use), so busy connections must wait on the lock instead of
    // immediately erroring, and readers must not block a concurrent writer.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA busy_timeout=3000;")?;
    init_schema(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Encodes an embedding vector as a little-endian f32 BLOB.
pub fn encode_embedding(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for f in v {
        buf.extend_from_slice(&f.to_le_bytes());
    }
    buf
}

/// Decodes a little-endian f32 BLOB back into a vector. Malformed (wrong
/// length) blobs decode to an empty vector rather than panicking.
pub fn decode_embedding(bytes: &[u8]) -> Vec<f32> {
    if bytes.len() % 4 != 0 {
        return Vec::new();
    }
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Cosine similarity in [-1, 1]; 0.0 if either vector has zero magnitude or
/// they differ in length.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// Howard Hinnant's days-from-civil / civil-from-days algorithm (public
// domain) — used instead of pulling in a date/time crate for timestamp
// columns and their arithmetic.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64; // [0, 399]
    let mp = if m > 2 { m as i64 - 3 } else { m as i64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

// `pub(crate)` (not just `fn`) so cli.rs's own tests can backdate a row's
// `created_at` the same way store.rs's own tests do (e.g. the curation-pass
// settling-period tests) without duplicating this civil-date math.
pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn now_rfc3339_from_secs(secs: u64) -> String {
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

/// The current instant as an RFC3339 UTC timestamp, in the same format
/// `created_at`/`last_accessed_at`/etc. are stored in. Exposed (rather than
/// called internally everywhere) so callers that need a fake clock for
/// ranking/reinforcement — tests, and eventually any other caller — can
/// compute one "now" up front and thread it through explicitly.
pub fn now_rfc3339() -> String {
    now_rfc3339_from_secs(now_secs())
}

/// Parses a `YYYY-MM-DDTHH:MM:SSZ` timestamp (the only format this store
/// writes) into Unix seconds. Returns `None` on anything that doesn't match.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some(days * 86400 + h * 3600 + mi * 60 + se)
}

/// Whole (fractional) days between two Unix-second instants, floored at 0
/// so clock skew or a "now" earlier than the stored instant never produces
/// a negative age.
pub fn days_between(now: i64, then: i64) -> f64 {
    let diff = (now - then) as f64 / 86400.0;
    if diff < 0.0 {
        0.0
    } else {
        diff
    }
}

/// MemoryBank/FSRS-style recency: `exp(-days_since_last_access / stability)`.
/// `last_accessed_at` falls back to `created_at` when the row has never
/// been touched.
fn compute_recency(m: &Memory, now: i64) -> f32 {
    let last = m.last_accessed_at.as_deref().unwrap_or(&m.created_at);
    let last_secs = parse_rfc3339(last).unwrap_or(now);
    let days = days_between(now, last_secs);
    let stability = m.effective_stability().max(0.01);
    (-days / stability).exp() as f32
}

/// ACT-R optimized base-level activation (Petrov 2006 approximation),
/// normalized to (0, 1) with a sigmoid: `B = ln(n/(1-d)) - d*ln(L)`,
/// `d = 0.5`. `n = 0` (never accessed) short-circuits to strength `0` since
/// the log form is undefined at `n = 0`.
fn compute_strength(m: &Memory, now: i64) -> f32 {
    let n = m.access_count;
    if n <= 0 {
        return 0.0;
    }
    let first = m.first_accessed_at.as_deref().unwrap_or(&m.created_at);
    let first_secs = parse_rfc3339(first).unwrap_or(now);
    let l = days_between(now, first_secs).max(0.01);
    let d = 0.5f64;
    let n_f = n as f64;
    let b = (n_f / (1.0 - d)).ln() - d * l.ln();
    (1.0 / (1.0 + (-b).exp())) as f32
}

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<Memory> {
    let reviewed_int: i64 = row.get("reviewed")?;
    let blob: Option<Vec<u8>> = row.get("embedding")?;
    let created_at: String = row.get("created_at")?;
    let valid_from: Option<String> = row.get("valid_from")?;
    Ok(Memory {
        id: row.get("id")?,
        content: row.get("content")?,
        source: row.get("source")?,
        project: row.get("project")?,
        reviewed: reviewed_int != 0,
        embedding: blob.map(|b| decode_embedding(&b)),
        importance: row.get("importance")?,
        stability: row.get("stability")?,
        access_count: row.get("access_count")?,
        first_accessed_at: row.get("first_accessed_at")?,
        last_accessed_at: row.get("last_accessed_at")?,
        valid_from: valid_from.unwrap_or_else(|| created_at.clone()),
        invalidated_at: row.get("invalidated_at")?,
        superseded_by: row.get("superseded_by")?,
        dormant_at: row.get("dormant_at")?,
        last_verified_at: row.get("last_verified_at")?,
        graph_extracted_at: row.get("graph_extracted_at")?,
        basis: row.get("basis")?,
        occurred_from: row.get("occurred_from")?,
        occurred_to: row.get("occurred_to")?,
        pinned_at: row.get("pinned_at")?,
        created_at,
    })
}

/// Inserts a new memory (with the current time as `created_at`/`valid_from`
/// and initial `stability = importance * 7.0` days) and returns its id.
pub fn insert(
    conn: &Connection,
    content: &str,
    source: Option<&str>,
    project: Option<&str>,
    reviewed: bool,
    embedding: Option<&[f32]>,
    importance: i64,
) -> Result<i64, KbError> {
    insert_with_basis(conn, content, source, project, reviewed, embedding, importance, None)
}

/// The two values `Memory::basis` may hold. Anything else is rejected at
/// the write boundary so the column never accumulates free text.
pub const BASIS_STATED: &str = "stated";
pub const BASIS_INFERRED: &str = "inferred";
/// The agent's own conduct: what Claude proposed, built, got wrong, and how
/// the user responded. Hindsight's experience network as a third basis
/// rather than a fourth table -- "I did this and watched it land" is a
/// distinct GROUND for holding a belief, alongside "the user said it" and
/// "I deduced it", so it belongs on the same axis.
pub const BASIS_EXPERIENCE: &str = "experience";
/// Written entirely by code, never from a session: the code index's own
/// file/module/repo summaries, mirrored into `memories` for module/repo
/// levels by `upsert_index_memory`. Neither "the user said it" nor "I
/// deduced it from behavior" nor "I watched myself do it" -- it's derived
/// mechanically from parsed source, so it gets a basis distinct from all
/// three human-facing ones.
pub const BASIS_DERIVED: &str = "derived";

pub fn is_valid_basis(b: &str) -> bool {
    b == BASIS_STATED || b == BASIS_INFERRED || b == BASIS_EXPERIENCE || b == BASIS_DERIVED
}

/// `insert` plus an explicit `basis` (see `Memory::basis`). `None` leaves
/// the column NULL. An unknown basis string is an error, not silently
/// stored.
#[allow(clippy::too_many_arguments)]
pub fn insert_with_basis(
    conn: &Connection,
    content: &str,
    source: Option<&str>,
    project: Option<&str>,
    reviewed: bool,
    embedding: Option<&[f32]>,
    importance: i64,
    basis: Option<&str>,
) -> Result<i64, KbError> {
    if let Some(b) = basis {
        if !is_valid_basis(b) {
            return Err(KbError::Other(format!("invalid basis {:?} (expected stated|inferred|experience|derived)", b)));
        }
    }
    let created_at = now_rfc3339();
    let stability = importance as f64 * 7.0;
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO memories
            (content, source, project, created_at, reviewed, embedding, importance, stability, valid_from, basis)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?4, ?9)",
        params![content, source, project, created_at, reviewed as i64, blob, importance, stability, basis],
    )?;
    let id = conn.last_insert_rowid();
    // Cheap deterministic mention links at write time (every entity name the
    // text contains). The graph-extraction pass adds the LLM-resolved ones
    // later; both land in the same table. Never fatal to the insert.
    let _ = link_mentions_by_name_scan(conn, id, content, &created_at);
    // Occurrence dates the text states itself, same rule as the v19
    // backfill. An explicit `set_occurrence` call (from a digest's `when:`
    // tag) overwrites this.
    let dates = iso_dates_in(content);
    if let (Some(from), Some(to)) = (dates.first(), dates.last()) {
        let _ = conn.execute(
            "UPDATE memories SET occurred_from = ?1, occurred_to = ?2 WHERE id = ?3",
            params![from, to, id],
        );
    }
    Ok(id)
}

/// Records when a memory's content happened. Both ends inclusive; pass the
/// same date twice for a single day. Validated as `YYYY-MM-DD` so the
/// column stays comparable with plain string ordering, the way every other
/// date in this schema is.
pub fn set_occurrence(conn: &Connection, id: i64, from: &str, to: &str) -> Result<(), KbError> {
    for d in [from, to] {
        if iso_dates_in(d).len() != 1 || d.len() != 10 {
            return Err(KbError::Other(format!("occurrence must be YYYY-MM-DD (got {:?})", d)));
        }
    }
    let (from, to) = if from <= to { (from, to) } else { (to, from) };
    conn.execute(
        "UPDATE memories SET occurred_from = ?1, occurred_to = ?2 WHERE id = ?3",
        params![from, to, id],
    )?;
    Ok(())
}

/// Sets `basis` on an existing row (used by `mach kb add`, whose insert
/// runs inside the classifier's `apply_verdict` and so cannot pass it at
/// insert time). Same validation as `insert_with_basis`.
pub fn set_basis(conn: &Connection, id: i64, basis: &str) -> Result<(), KbError> {
    if !is_valid_basis(basis) {
        return Err(KbError::Other(format!("invalid basis {:?} (expected stated|inferred|experience|derived)", basis)));
    }
    conn.execute("UPDATE memories SET basis = ?1 WHERE id = ?2", params![basis, id])?;
    Ok(())
}

/// Most recent memories first, optionally capped to `limit` rows.
/// Tombstoned (superseded) rows are excluded unless `superseded_only` is
/// set, in which case *only* tombstoned rows are returned — the audit view
/// (`mach kb list --superseded`). Dormant rows are excluded from the default
/// view the same way tombstoned ones are — see `list_dormant` for their own
/// audit view (`mach kb list --dormant`), a separate axis from this flag.
pub fn list(conn: &Connection, limit: Option<usize>, superseded_only: bool) -> Result<Vec<Memory>, KbError> {
    let where_clause = if superseded_only {
        "WHERE invalidated_at IS NOT NULL"
    } else {
        "WHERE invalidated_at IS NULL AND dormant_at IS NULL"
    };
    let sql = match limit {
        Some(n) => format!("SELECT * FROM memories {} ORDER BY id DESC LIMIT {}", where_clause, n),
        None => format!("SELECT * FROM memories {} ORDER BY id DESC", where_clause),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Dormant memories only, most recent first, optionally capped — the audit
/// view `mach kb list --dormant`. A separate function (rather than a third
/// state of `list`'s `superseded_only` flag) since dormancy and supersession
/// are independent axes: a row can in principle be both.
pub fn list_dormant(conn: &Connection, limit: Option<usize>) -> Result<Vec<Memory>, KbError> {
    let sql = match limit {
        Some(n) => format!("SELECT * FROM memories WHERE dormant_at IS NOT NULL ORDER BY id DESC LIMIT {}", n),
        None => "SELECT * FROM memories WHERE dormant_at IS NOT NULL ORDER BY id DESC".to_string(),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Pinned memories only (see `restore`, which sets `pinned_at`), most
/// recent first, optionally capped — the audit view `mach kb list
/// --pinned`. A separate function from `list`'s `superseded_only` flag,
/// same reasoning as `list_dormant`: pin state is an independent axis a
/// row can carry alongside any tombstone/dormancy state (a pinned row
/// stays pinned even if a human later re-supersedes it by hand — see
/// `supersede`'s own doc comment — so this view is not restricted to
/// active rows).
pub fn list_pinned(conn: &Connection, limit: Option<usize>) -> Result<Vec<Memory>, KbError> {
    let sql = match limit {
        Some(n) => format!("SELECT * FROM memories WHERE pinned_at IS NOT NULL ORDER BY id DESC LIMIT {}", n),
        None => "SELECT * FROM memories WHERE pinned_at IS NOT NULL ORDER BY id DESC".to_string(),
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// All `reviewed = 0` rows (auto-extracted candidates awaiting `mach kb
/// review`), oldest first so review works through them in insertion order.
/// `mach kb reflect`'s curation pass (`curation_candidates`, below) is the
/// organic path that clears this queue on its own; `mach kb review` remains
/// available as an optional, immediate human override over the same rows.
pub fn unreviewed(conn: &Connection) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM memories WHERE reviewed = 0 ORDER BY id ASC")?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Age in whole (fractional) days of `created_at`, relative to `now` — both
/// RFC3339 strings in this store's own format. Exposed for `mach kb
/// reflect`'s curation pass, which shows a candidate's age to the judge;
/// mirrors the age math `memory_qualifies_for_dormancy` already does
/// internally.
pub fn age_days(created_at: &str, now: &str) -> f64 {
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    let created_secs = parse_rfc3339(created_at).unwrap_or(now_secs);
    days_between(now_secs, created_secs)
}

/// Curation-pass candidates for `mach kb reflect`: ACTIVE (not dormant, not
/// superseded) unreviewed rows at least `min_age_days` old — a settling
/// period so engagement evidence (session judgments touching a row) has a
/// chance to accumulate before a model has to judge on content alone — and
/// oldest-first among those, capped at `cap` per run so a large backlog
/// drains in a stable order across runs rather than being reshuffled. A row
/// still inside its settling period is simply left for a later run, same as
/// `dedupe_candidate_pairs`/`contradiction_candidate_pairs`'s own caps.
pub fn curation_candidates(conn: &Connection, now: &str, min_age_days: f64, cap: usize) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM memories WHERE reviewed = 0 AND dormant_at IS NULL AND invalidated_at IS NULL \
         ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        let m = r?;
        if age_days(&m.created_at, now) >= min_age_days {
            out.push(m);
        }
    }
    out.truncate(cap);
    Ok(out)
}

/// Distinct, non-empty `project` values across every memory (any state,
/// reviewed or not) — topic anchoring for `mach note`'s classifier prompt,
/// so a repeat topic reuses its existing name rather than drifting into a
/// near-duplicate kebab-case variant.
pub fn distinct_projects(conn: &Connection) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT project FROM memories WHERE project IS NOT NULL AND TRIM(project) != '' ORDER BY project",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Memory>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM memories WHERE id = ?1")?;
    Ok(stmt.query_row(params![id], row_to_memory).optional()?)
}

/// Deletes a memory by id (hard delete — `mach kb forget` stays permanent,
/// unlike supersession's tombstoning). Returns whether a row existed.
/// Deletes one memory row (`mach kb forget <id>` and the dormancy-driven
/// paths that call it). Row-only: a note's attached image under
/// `~/.local/share/mach/kb-images/` (see `note::store_image`) is never
/// touched here, by design — the file is content-addressed and may be
/// referenced by other rows, and forgetting the fact that mentions an image
/// shouldn't destroy evidence the image ever existed. Orphaned image
/// cleanup, if ever wanted, is a separate manual sweep, not a side effect
/// of forgetting.
///
/// Also purges any `dedupe_seen`/`contradiction_seen` row citing `id` — a
/// pair "already judged" against a memory that no longer exists must not
/// silently suppress a fresh judgment if some future id (post-AUTOINCREMENT,
/// this can no longer be `id` itself, but the cleanup is unconditional
/// regardless) ever needs to be compared against the survivor again. Both
/// deletes run in the same transaction as the memory delete so a crash
/// mid-way never leaves a stale pair behind pointing at a row that's
/// already gone.
pub fn delete(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let tx = conn.unchecked_transaction()?;
    // Graph references first: `relations.evidence_memory_id` carries a real
    // `REFERENCES memories(id)`, so deleting a cited memory used to fail
    // outright with "FOREIGN KEY constraint failed" -- measured on this
    // machine, 214 of 738 active memories (29%) were undeletable, and the
    // selection was backwards, failing on the OLDER well-integrated rows
    // that graph extraction had had time to reach.
    //
    // The design this collides with is `relations_with_dead_evidence`,
    // whose doc names "hard-deleted (`mach kb forget`)" as one of the
    // states it detects -- it expects a dangling id the FK forbids. Rather
    // than drop the constraint, do here, deterministically, what the
    // hygiene pass would have done later: an edge that still has other
    // evidence is repointed at a survivor, and an edge whose only evidence
    // this was gets tombstoned. Same end state, no dangling reference, and
    // no edge is left silently orphaned with a NULL pointer that
    // `relations_with_dead_evidence` would never look at again.
    //
    // Guarded on the tables existing: `delete` is reachable from migration
    // paths that run at schema versions predating the graph layer (the
    // v6->v7 AUTOINCREMENT test deletes a row on a `relations`-less
    // database), and an unguarded query there fails with "no such table".
    let now = now_rfc3339();
    let has_graph = table_exists(&tx, "relations")? && table_exists(&tx, "relation_evidence")?;
    let citing: Vec<i64> = if has_graph {
        let mut stmt =
            tx.prepare("SELECT id FROM relations WHERE evidence_memory_id = ?1")?;
        let rows = stmt.query_map(params![id], |r| r.get(0))?;
        rows.collect::<Result<Vec<i64>, _>>()?
    } else {
        Vec::new()
    };
    if has_graph {
        tx.execute("DELETE FROM relation_evidence WHERE memory_id = ?1", params![id])?;
    }
    for relation_id in citing {
        let survivor: Option<i64> = {
            let mut stmt = tx.prepare(
                "SELECT memory_id FROM relation_evidence
                 WHERE relation_id = ?1 ORDER BY created_at ASC, memory_id ASC LIMIT 1",
            )?;
            stmt.query_row(params![relation_id], |r| r.get(0)).optional()?
        };
        match survivor {
            Some(m) => {
                tx.execute(
                    "UPDATE relations SET evidence_memory_id = ?1 WHERE id = ?2",
                    params![m, relation_id],
                )?;
            }
            None => {
                tx.execute(
                    "UPDATE relations SET evidence_memory_id = NULL, invalidated_at = ?1
                     WHERE id = ?2 AND invalidated_at IS NULL",
                    params![now, relation_id],
                )?;
                // Already-tombstoned edges still need the pointer cleared,
                // or the delete below trips the constraint anyway.
                tx.execute(
                    "UPDATE relations SET evidence_memory_id = NULL WHERE id = ?1",
                    params![relation_id],
                )?;
            }
        }
    }
    let n = tx.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    if n > 0 {
        tx.execute("DELETE FROM dedupe_seen WHERE id_a = ?1 OR id_b = ?1", params![id])?;
        tx.execute("DELETE FROM contradiction_seen WHERE id_a = ?1 OR id_b = ?1", params![id])?;
    }
    tx.commit()?;
    Ok(n > 0)
}

pub fn set_reviewed(conn: &Connection, id: i64, reviewed: bool) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET reviewed = ?1 WHERE id = ?2",
        params![reviewed as i64, id],
    )?;
    Ok(n > 0)
}

/// Sets a memory's `importance` directly (1-10, though this never enforces
/// the range itself — callers own that). Used by `mach kb reflect`'s
/// curation pass to pin a DEMOTEd row's importance down to
/// `reflect::CURATION_DEMOTE_IMPORTANCE`; unlike `forget`, never deletes —
/// organic decay (dormancy) finishes the job later.
pub fn set_importance(conn: &Connection, id: i64, importance: i64) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE memories SET importance = ?1 WHERE id = ?2", params![importance, id])?;
    Ok(n > 0)
}

/// Updates a memory's content and, if a fresh embedding is supplied,
/// replaces its vector too (used by `mach kb review`'s edit action — when
/// re-embedding fails, pass `None` to keep the old vector rather than
/// discard it).
pub fn update_content(
    conn: &Connection,
    id: i64,
    content: &str,
    embedding: Option<&[f32]>,
) -> Result<bool, KbError> {
    let n = match embedding {
        Some(v) => {
            let blob = encode_embedding(v);
            conn.execute(
                "UPDATE memories SET content = ?1, embedding = ?2 WHERE id = ?3",
                params![content, blob, id],
            )?
        }
        None => conn.execute(
            "UPDATE memories SET content = ?1 WHERE id = ?2",
            params![content, id],
        )?,
    };
    Ok(n > 0)
}

/// Tombstones `old_id` in favor of `new_id`: sets `invalidated_at = now`
/// and `superseded_by = new_id`. Never deletes — the row stays as an audit
/// trail, visible via `mach kb list --superseded`. Returns `false` (no-op)
/// if `old_id` doesn't exist or is already tombstoned.
pub fn supersede(conn: &Connection, old_id: i64, new_id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET invalidated_at = ?1, superseded_by = ?2
         WHERE id = ?3 AND invalidated_at IS NULL",
        params![now, new_id, old_id],
    )?;
    Ok(n > 0)
}

/// Undoes a supersession: clears `invalidated_at` and `superseded_by` so
/// the row is active again. Returns `false` (no-op) if `id` doesn't exist
/// or is not tombstoned.
///
/// This exists because the contradiction judge can be wrong in a way that
/// destroys information rather than tidying it. Two memories that each
/// scope themselves to a different date ("As of 2026-09-07, the bank held
/// 150 memories" against "As of 2026-09-09, it holds 739") are not a stale
/// claim and its correction — they are two readings of a changing
/// quantity, and the older row is the only record of what was true then.
/// Tombstoning it makes the bank unable to answer the temporal questions
/// it exists to answer. `build_contradiction_pass_prompt` now tells the
/// judge to return BOTH_HOLD for that shape, but judgments already applied
/// need a way back, and `forget` is the wrong tool: it deletes.
///
/// Also pins the row (`pinned_at = now`): a human just looked at this pair
/// and decided the tombstone was wrong, so the row must not be silently
/// re-judged and re-tombstoned by the very next automatic pass. Every
/// automatic supersession path (`run_dedupe_pass`'s Keep branch,
/// `apply_contradiction_verdict`'s Conflict/ConflictRetro,
/// `apply_verdict`'s Supersede) checks `is_pinned` first and skips a
/// pinned row (`apply_verdict`'s Update never tombstones to begin with, so
/// pin status doesn't come into it there). `mach kb unpin <id>` clears the
/// pin; manual `mach kb supersede` is unaffected either way — it is a
/// human decision, not one this guard second-guesses.
pub fn restore(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET invalidated_at = NULL, superseded_by = NULL, pinned_at = ?1
         WHERE id = ?2 AND invalidated_at IS NOT NULL",
        params![now, id],
    )?;
    Ok(n > 0)
}

/// Tombstones `id` on its own, `superseded_by` left NULL -- for a derived
/// row that has no successor to name, only a removal. So far the one
/// caller is `code_index::summary`'s orphan cleanup: a module/repo
/// summary's mirrored memory (`code_summaries.memory_id`, written by
/// `upsert_index_memory`) when that module's directory no longer has any
/// indexed file -- there is nothing new replacing it, so `supersede`
/// (which always names a winner) doesn't fit. Modeled on
/// `invalidate_relation`, the same shape one level up for `relations`.
/// Never deletes; the row stays as an audit trail like every other
/// tombstone in this store. Returns `false` (no-op) if `id` doesn't exist
/// or is already invalidated.
pub fn invalidate_memory(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE memories SET invalidated_at = ?1 WHERE id = ?2 AND invalidated_at IS NULL", params![now, id])?;
    Ok(n > 0)
}

/// Whether `id` is currently pinned (set by `restore`, cleared by `unpin`).
/// A missing id reports `false` rather than erroring — every caller of
/// this is a fail-safe skip check ("don't tombstone this automatically"),
/// and a row that doesn't exist can't be tombstoned anyway.
pub fn is_pinned(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let pinned: Option<Option<String>> = conn
        .query_row("SELECT pinned_at FROM memories WHERE id = ?1", params![id], |r| r.get::<_, Option<String>>(0))
        .optional()?;
    Ok(pinned.flatten().is_some())
}

/// Clears a pin set by `restore` (`mach kb unpin <id>`). Returns `false`
/// (no-op) if `id` doesn't exist or isn't pinned.
pub fn unpin(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE memories SET pinned_at = NULL WHERE id = ?1 AND pinned_at IS NOT NULL", params![id])?;
    Ok(n > 0)
}

// --- supersession audit: was a tombstone lossy? ---

/// One tombstoned memory joined to its direct (one-hop) successor — the
/// raw candidate set for `mach kb audit-supersessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionCandidate {
    pub old_id: i64,
    pub old_content: String,
    pub new_id: i64,
    pub new_content: String,
}

/// Every tombstoned memory (`superseded_by IS NOT NULL`) joined to its
/// direct successor, oldest tombstoned row first (`created_at`, then `id`
/// to break ties) — the working set `mach kb audit-supersessions` chunks
/// into LLM batches. A chain (A -> B -> C) yields one row per hop (A's row
/// names B as its successor; B's own row, since B is itself tombstoned,
/// names C), never a row skipping straight from A to C — each hop is
/// judged on its own merits, per the task's own chains rule.
///
/// A `superseded_by` that no longer resolves to a live row (shouldn't
/// happen — `supersede`/`restore` only ever point it at a real id, and
/// nothing deletes rows) is skipped: there is nothing to compare the old
/// content against.
pub fn supersession_candidates(conn: &Connection) -> Result<Vec<SupersessionCandidate>, KbError> {
    let sql = format!(
        "SELECT m.id, m.content, s.id, s.content
         FROM memories m
         JOIN memories s ON s.id = m.superseded_by
         WHERE m.superseded_by IS NOT NULL
           AND {}
           AND {}
         ORDER BY m.created_at ASC, m.id ASC",
        not_index_owned_sql("m.source"),
        not_index_owned_sql("s.source"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        Ok(SupersessionCandidate { old_id: r.get(0)?, old_content: r.get(1)?, new_id: r.get(2)?, new_content: r.get(3)? })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// `old_id -> new_id` for every pair already recorded in
/// `supersession_audit` — a candidate is skipped by a normal `mach kb
/// audit-supersessions` run (`--reaudit` re-examines it anyway) only when
/// its CURRENT `new_id` (from `supersession_candidates`'s live join)
/// equals the recorded one. A bare "was `old_id` ever audited" set is not
/// enough: if a restored row is later manually re-superseded to a
/// different winner (`A -> B` audited, `A` restored, then a human runs
/// `mach kb supersede A D`), the new `A -> D` hop is a fresh decision the
/// old `A -> B` verdict says nothing about, and must be judged again, not
/// silently skipped as "already audited".
pub fn supersession_audited_pairs(conn: &Connection) -> Result<HashMap<i64, i64>, KbError> {
    let mut stmt = conn.prepare("SELECT old_id, new_id FROM supersession_audit")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = HashMap::new();
    for r in rows {
        let (old_id, new_id) = r?;
        out.insert(old_id, new_id);
    }
    Ok(out)
}

/// One row of `mach kb audit-supersessions`'s durable record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupersessionAuditRow {
    pub old_id: i64,
    pub new_id: i64,
    pub verdict: String,
    pub reason: String,
    pub audited_at: String,
}

/// Records one audited hop (`old_id -> new_id`, `verdict` is `"OK"` or
/// `"LOSSY"`, `reason` the judge's short explanation). Upserts on `old_id`
/// so `--reaudit` overwrites a prior verdict rather than erroring on the
/// primary key. A pair the judge never addressed at all must NOT be passed
/// here — the caller leaves it unrecorded so it is retried next run,
/// mirroring every other batched judge in this module.
pub fn record_supersession_audit(
    conn: &Connection,
    old_id: i64,
    new_id: i64,
    verdict: &str,
    reason: &str,
    now: &str,
) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO supersession_audit (old_id, new_id, verdict, reason, audited_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(old_id) DO UPDATE SET
            new_id = excluded.new_id,
            verdict = excluded.verdict,
            reason = excluded.reason,
            audited_at = excluded.audited_at",
        params![old_id, new_id, verdict, reason, now],
    )?;
    Ok(())
}

/// Fetches one `old_id`'s audit row, if it has been judged.
pub fn get_supersession_audit(conn: &Connection, old_id: i64) -> Result<Option<SupersessionAuditRow>, KbError> {
    let row = conn
        .query_row(
            "SELECT old_id, new_id, verdict, reason, audited_at FROM supersession_audit WHERE old_id = ?1",
            params![old_id],
            |r| {
                Ok(SupersessionAuditRow {
                    old_id: r.get(0)?,
                    new_id: r.get(1)?,
                    verdict: r.get(2)?,
                    reason: r.get(3)?,
                    audited_at: r.get(4)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Every `LOSSY`-verdict row on record, oldest audited first — the repair
/// queue `mach kb audit-supersessions --apply` restores from. Deliberately
/// not scoped to "this run": a dry run may have recorded a LOSSY verdict
/// that no `--apply` invocation has acted on yet (or an earlier `--apply`
/// run that was interrupted before reaching every row), and those rows
/// stay just as actionable as one judged in the current run. The caller
/// restores a row only when its CURRENT `superseded_by` still equals this
/// row's recorded `new_id` — a row this table calls LOSSY but that's
/// already been restored (by a prior `--apply` run, or `mach kb restore`
/// by hand) needs no repeat work, and one a human has since manually
/// re-superseded to a different winner (`mach kb supersede <old> <new>`)
/// must never be restored on the strength of a verdict recorded against
/// the earlier hop.
pub fn supersession_audit_lossy_rows(conn: &Connection) -> Result<Vec<SupersessionAuditRow>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT old_id, new_id, verdict, reason, audited_at FROM supersession_audit
         WHERE verdict = 'LOSSY' ORDER BY audited_at ASC, old_id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SupersessionAuditRow { old_id: r.get(0)?, new_id: r.get(1)?, verdict: r.get(2)?, reason: r.get(3)?, audited_at: r.get(4)? })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Reinforcement (`mach kb search --touch`): for each id, bumps
/// `access_count`, resets `last_accessed_at` to `now`, sets
/// `first_accessed_at` if this is the first touch, and grows `stability`
/// by an interval-aware fraction of the flat 30% gain — the spacing
/// effect: `stability * (1 + 0.3 * min(1, elapsed_days / stability))`,
/// where `elapsed_days` is days since `last_accessed_at` (or since
/// `created_at` for a row that has never been touched), still capped at
/// 365 days. Two touches moments apart therefore barely move stability;
/// a touch after a gap at least as long as the current stability earns
/// the full ×1.3, same as the old flat rule. One transaction for the
/// whole batch; each row's current `stability`/`importance`/
/// `last_accessed_at`/`created_at` is read before its update so the gain
/// is computed per row rather than in one blanket SQL expression.
pub fn touch(conn: &Connection, ids: &[i64], now: &str) -> Result<(), KbError> {
    if ids.is_empty() {
        return Ok(());
    }
    let now_secs = parse_rfc3339(now).unwrap_or_else(|| now_secs() as i64);
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        let row: Option<(Option<f64>, i64, Option<String>, String)> = tx
            .query_row(
                "SELECT stability, importance, last_accessed_at, created_at FROM memories WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((stability, importance, last_accessed_at, created_at)) = row else {
            continue;
        };
        let base = stability.unwrap_or(importance as f64 * 7.0);
        let last = last_accessed_at.as_deref().unwrap_or(&created_at);
        let last_secs = parse_rfc3339(last).unwrap_or(now_secs);
        let elapsed_days = days_between(now_secs, last_secs);
        let ratio = if base > 0.0 { (elapsed_days / base).min(1.0) } else { 1.0 };
        let new_stability = (base * (1.0 + 0.3 * ratio)).min(365.0);
        tx.execute(
            "UPDATE memories SET
                access_count = access_count + 1,
                last_accessed_at = ?1,
                first_accessed_at = COALESCE(first_accessed_at, ?1),
                stability = ?2
             WHERE id = ?3",
            params![now, new_stability, id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Marks an active memory dormant (the nightly `mach kb reflect` dormancy
/// pass) — never a delete, and never touches anything else about the row.
/// No-op (returns `false`) if the row doesn't exist or is already dormant.
pub fn set_dormant(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET dormant_at = ?1 WHERE id = ?2 AND dormant_at IS NULL",
        params![now, id],
    )?;
    Ok(n > 0)
}

/// Wakes a dormant memory (`mach kb wake <id>`): clears `dormant_at` and
/// resets `last_accessed_at` to `now` so it doesn't immediately qualify to
/// go dormant again on the very next reflect pass. No-op (returns `false`)
/// if the row doesn't exist or isn't currently dormant.
pub fn wake(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET dormant_at = NULL, last_accessed_at = ?1 WHERE id = ?2 AND dormant_at IS NOT NULL",
        params![now, id],
    )?;
    Ok(n > 0)
}

/// Nightly dormancy thresholds for `mach kb reflect`'s forgetting pass.
pub const DORMANCY_MIN_AGE_DAYS: f64 = 90.0;
pub const DORMANCY_MIN_UNTOUCHED_STALE_DAYS: f64 = 60.0;
pub const DORMANCY_MAX_IMPORTANCE: i64 = 5;

/// Whether an active memory qualifies to go dormant on tonight's `mach kb
/// reflect` pass. ALL of: created more than `DORMANCY_MIN_AGE_DAYS` ago;
/// never touched (`access_count == 0`) or not touched in over
/// `DORMANCY_MIN_UNTOUCHED_STALE_DAYS` days; `importance <=
/// DORMANCY_MAX_IMPORTANCE`; and not cited by any active insight or theme
/// (`cited`, computed by the caller against `cited_memory_ids`).
///
/// Reviewed and unreviewed rows follow this exact same criteria — memory is
/// organic, so an auto-captured row earns its way to dormancy (or doesn't)
/// on the same age/engagement/importance/citation terms as anything else,
/// never on an absolute "never curated" clock. (An earlier version of this
/// rule forced any unreviewed row past 30 days into dormancy regardless of
/// everything else; that absolute rule is gone — `mach kb reflect`'s
/// curation pass now judges the unreviewed queue on its merits instead, the
/// same way this function judges everyone else.)
///
/// A tombstoned or already-dormant row never qualifies (the caller's own
/// candidate pool — `active_memories_for_dormancy` — already excludes both,
/// this is just a defensive second guard for a direct call).
pub fn memory_qualifies_for_dormancy(m: &Memory, cited: bool, now: &str) -> bool {
    if m.is_dormant() || m.is_superseded() {
        return false;
    }
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    let created_secs = parse_rfc3339(&m.created_at).unwrap_or(now_secs);
    let age_days = days_between(now_secs, created_secs);

    if age_days <= DORMANCY_MIN_AGE_DAYS {
        return false;
    }
    let stale_or_untouched = match &m.last_accessed_at {
        None => true,
        Some(last) => {
            m.access_count == 0 || {
                let last_secs = parse_rfc3339(last).unwrap_or(now_secs);
                days_between(now_secs, last_secs) > DORMANCY_MIN_UNTOUCHED_STALE_DAYS
            }
        }
    };
    if !stale_or_untouched {
        return false;
    }
    if m.importance > DORMANCY_MAX_IMPORTANCE {
        return false;
    }
    if cited {
        return false;
    }
    true
}

/// Every non-tombstoned, non-dormant memory (reviewed and unreviewed alike)
/// — the pool the nightly dormancy pass in `mach kb reflect` evaluates each
/// run against `memory_qualifies_for_dormancy`.
pub fn active_memories_for_dormancy(conn: &Connection) -> Result<Vec<Memory>, KbError> {
    candidates(conn, true, false)
}

/// Every raw memory id cited (in `source_ids`) by any active insight or
/// theme — the dormancy pass's "not cited" criterion. Plain numeric tokens
/// only; an `i<id>` insight-reference token simply fails to parse as an
/// `i64` and is skipped, exactly as intended (a theme's own citation of an
/// insight is not a memory citation).
pub fn cited_memory_ids(conn: &Connection) -> Result<std::collections::HashSet<i64>, KbError> {
    let mut stmt = conn.prepare("SELECT source_ids FROM insights WHERE invalidated_at IS NULL")?;
    let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
    let mut set = std::collections::HashSet::new();
    for json in rows {
        let ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
        for s in ids {
            if let Ok(id) = s.parse::<i64>() {
                set.insert(id);
            }
        }
    }
    Ok(set)
}

/// Candidate rows for a search: reviewed-only unless `include_all`, and
/// active-only (not tombstoned) unless `include_superseded`. Dormant rows
/// are excluded unconditionally — like a tombstoned row, a dormant one
/// never belongs in a search/recall/reflection working set; `mach kb list
/// --dormant` (not this function) is how they're ever surfaced again.
fn candidates(conn: &Connection, include_all: bool, include_superseded: bool) -> Result<Vec<Memory>, KbError> {
    let mut clauses = vec!["dormant_at IS NULL"];
    if !include_all {
        clauses.push("reviewed = 1");
    }
    if !include_superseded {
        clauses.push("invalidated_at IS NULL");
    }
    let sql = if clauses.is_empty() {
        "SELECT * FROM memories".to_string()
    } else {
        format!("SELECT * FROM memories WHERE {}", clauses.join(" AND "))
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// One ranked search hit: the blended `score` plus its three components,
/// so callers (JSON output, tests) can see how it was built.
pub struct RankedHit {
    pub memory: Memory,
    pub score: f32,
    pub sim: f32,
    pub recency: f32,
    pub strength: f32,
    pub superseded: bool,
    // IDF-weighted term coverage from the FTS5 index for the query's exact
    // tokens (see `lexical_scores`): 1.0 when the row matched every query
    // term's IDF mass, not normalized against the best hit in the result
    // set; 0.0 when the row matched no token (or the search ran without
    // query text). See `search_hybrid`.
    pub lexical: f32,
}

/// Confidence penalty applied to an unreviewed (`reviewed = 0`) row's score
/// in both search paths below. Memory is organic: an auto-captured fact
/// that nobody has looked at yet is still real evidence and belongs in
/// recall by default, not gated behind `mach kb review` — but it earns
/// slightly less confidence than something a human (or a deliberate `mach
/// kb add`) has actually vouched for. `mach kb reflect`'s curation pass is
/// what removes this penalty over time (by setting `reviewed = 1`); `mach
/// kb review` remains available as an optional, immediate human override of
/// the same thing.
pub const UNREVIEWED_SEARCH_PENALTY: f32 = 0.85;

/// Ranked top-N search: `final = 0.70*sim + 0.20*recency + 0.10*strength`,
/// then `× UNREVIEWED_SEARCH_PENALTY` for any unreviewed row. Tombstoned
/// rows are excluded unless `include_superseded`, in which case they're
/// included but scored at 10% of the blend (still marked `superseded` in
/// the result so callers can label them); the two discounts stack (a
/// superseded *and* unreviewed row is rarer still, and scored accordingly).
///
/// Unreviewed rows are included by default (`reviewed_only = false`) — the
/// organic default: auto-captured, un-curated facts still surface in
/// recall, just with the above confidence penalty. Pass `reviewed_only =
/// true` (`mach kb search --reviewed-only`) for the old exclusive
/// behavior.
///
/// Applies `limit` first, then `min_score` — "post-limit, post-threshold"
/// — so `--touch` (which reinforces exactly the rows this function
/// returns) only reinforces what a caller's own threshold judged worth
/// showing, not every row that merely made the top-N cut.
#[allow(clippy::too_many_arguments)]
pub fn search_ranked(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<RankedHit>, KbError> {
    rank_with_lexical(
        conn,
        query_embedding,
        &HashMap::new(),
        None,
        limit,
        reviewed_only,
        include_superseded,
        min_score,
        now,
    )
}

/// How the retrieval channels are combined. `max` takes the strongest
/// single channel; `rrf` uses Reciprocal Rank Fusion, which scores by RANK
/// rather than raw score and so rewards a memory that several channels
/// agree on. Selected by `MACH_KB_FUSION`, default below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fusion {
    Max,
    Rrf,
}

/// RRF's rank-smoothing constant, `1 / (k + rank)`. 60 is the value from
/// the original Cormack et al. formulation and what Hindsight uses.
pub const RRF_K: f32 = 60.0;

pub fn fusion_mode() -> Fusion {
    static F: std::sync::OnceLock<Fusion> = std::sync::OnceLock::new();
    *F.get_or_init(|| match env::var("MACH_KB_FUSION").unwrap_or_default().to_lowercase().as_str() {
        "rrf" => Fusion::Rrf,
        "max" => Fusion::Max,
        _ => DEFAULT_FUSION,
    })
}

/// `max` measured better than `rrf` on this bank (28-question harness, at
/// every score floor tried): 75% versus 68%, and RRF injected 60% more
/// characters to get the worse number. RRF discards magnitude by design,
/// and on a 250-memory bank the magnitude IS the signal -- a rank-1 lexical
/// match on one common term earns the same reciprocal weight as a rank-1
/// semantic match at 0.8 cosine, so noise is promoted to parity with real
/// answers. Rank fusion is a large-corpus technique: it needs many strong
/// candidates per channel for ranks to be informative. Kept selectable
/// (`MACH_KB_FUSION=rrf`) so the comparison stays re-runnable as the bank
/// grows, since this conclusion should flip at some size.
pub const DEFAULT_FUSION: Fusion = Fusion::Max;

/// Reciprocal-rank contribution of a 0-based rank.
pub fn rrf_contrib(rank: usize) -> f32 {
    1.0 / (RRF_K + rank as f32 + 1.0)
}

/// Weight of a perfect temporal match (the memory's occurrence range
/// overlaps the date range the query asked about) relative to a perfect
/// cosine match. Below the lexical weight: a date is strong evidence of
/// relevance but a weaker one than the query's own words, since many
/// unrelated things happen on the same day.
pub const TEMPORAL_FLOOR: f32 = 0.5;

/// How much a query-range overlap lifts a candidate's topical score under
/// `Fusion::Max`. A fraction, not a channel score: see the fusion site for
/// why the date multiplies relevance rather than competing with it.
pub const TEMPORAL_BONUS: f32 = 0.35;

/// Relevance a date-only question gets from occurrence alone.
///
/// Measured against the 33-question harness: at 0.75 (the old
/// `TEMPORAL_WEIGHT`) the floor beats most topical scores and the 27-way
/// "yesterday" tie comes straight back, costing temporal_relative 5/5 ->
/// 4/5 and 300 extra chars per injection. At 0.5 it stays below anything
/// topically relevant while still clearing the hooks' 0.45 threshold, so
/// "what happened on 2026-08-31" is answerable and "what did I decide about
/// umoja admin access yesterday" is still ordered by the umoja part.

/// A calendar date range, both ends inclusive, as `YYYY-MM-DD`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateRange {
    pub from: String,
    pub to: String,
}

impl DateRange {
    pub fn overlaps(&self, from: &str, to: &str) -> bool {
        // String comparison is date comparison for zero-padded ISO dates.
        from <= self.to.as_str() && to >= self.from.as_str()
    }
}

/// The date range a query is asking about, or `None` when it names no time.
///
/// Rule-based on purpose (Hindsight runs the same heuristic path first and
/// only falls back to a seq2seq model for the leftovers): an explicit ISO
/// date, or a relative expression resolved against `today`. Everything else
/// yields `None`, which simply means the temporal channel sits out -- a
/// wrong date range is worse than no date range, because it would boost
/// whatever happened on some unrelated day.
pub fn query_date_range(query: &str, today: &str) -> Option<DateRange> {
    let q = query.to_lowercase();
    // An explicit ISO date (or two) wins: it is unambiguous.
    let explicit = iso_dates_in(query);
    if let (Some(a), Some(b)) = (explicit.first(), explicit.last()) {
        return Some(DateRange { from: a.clone(), to: b.clone() });
    }
    let today_secs = parse_rfc3339(&format!("{}T12:00:00Z", today))?;
    let day = 86400i64;
    let shift = |days: i64| -> String {
        now_rfc3339_from_secs((today_secs + days * day).max(0) as u64)[..10].to_string()
    };
    let single = |d: String| Some(DateRange { from: d.clone(), to: d });

    if q.contains("today") {
        return single(shift(0));
    }
    if q.contains("yesterday") {
        return single(shift(-1));
    }
    if q.contains("last week") || q.contains("past week") || q.contains("this week") {
        return Some(DateRange { from: shift(-7), to: shift(0) });
    }
    if q.contains("last month") || q.contains("past month") {
        return Some(DateRange { from: shift(-31), to: shift(0) });
    }
    // "in june", "in june 2026", "june 2026"
    const MONTHS: [&str; 12] = [
        "january", "february", "march", "april", "may", "june", "july", "august", "september", "october",
        "november", "december",
    ];
    for (i, name) in MONTHS.iter().enumerate() {
        if !q.contains(name) {
            continue;
        }
        let month = i + 1;
        // A year written next to the month, else the current one.
        let year: i64 = q
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|t| t.parse::<i64>().ok())
            .find(|y| (2000..=2100).contains(y))
            .unwrap_or_else(|| today[..4].parse().unwrap_or(2026));
        let last_day = match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            _ => {
                if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                    29
                } else {
                    28
                }
            }
        };
        return Some(DateRange {
            from: format!("{:04}-{:02}-01", year, month),
            to: format!("{:04}-{:02}-{:02}", year, month, last_day),
        });
    }
    None
}

/// Weight of a perfect lexical match relative to a perfect cosine match.
/// Below 1.0 on purpose: an exact-token hit is strong evidence but the
/// embedding still knows about paraphrase; a row that matches both gets
/// whichever is higher, never the sum (so nothing is double counted).
pub const LEXICAL_WEIGHT: f32 = 0.9;

/// Cosine similarity that two UNRELATED English texts already score under
/// `nomic-embed-text`. Measured on this bank with `mach kb eval`: questions
/// the bank genuinely cannot answer ("what is the capital of Mongolia")
/// still peak at 0.38-0.55 cosine, while answerable ones run 0.49-0.80.
/// Raw cosine therefore has no usable zero, and a score floor applied to it
/// cannot tell "nothing relevant" from "weakly relevant" -- before this,
/// recall injected memories for every single unanswerable question.
pub const SIM_NOISE_FLOOR: f32 = 0.40;

/// Ranking blend weights: `score = W_SIM*sim + W_RECENCY*recency +
/// W_STRENGTH*strength`. Overridable per process via `MACH_KB_W_SIM`,
/// `MACH_KB_W_RECENCY` and `MACH_KB_W_STRENGTH` so `mach kb eval` can sweep
/// them against the question set instead of them being guessed once and
/// never revisited. Read once per process.
/// Defaults measured with `mach kb eval` (28 questions) after `normalize_sim`
/// gave the similarity term a real zero. The old 0.70/0.20/0.10 was set
/// against RAW cosine, whose useful range was roughly 0.5-0.8; normalizing
/// widened that range to 0-1 and so halved recency's relative influence
/// requirement -- at the old weights a merely recent memory outranked an
/// older one that actually answered the question, and multi-session recall
/// fell from 83% to 50%. Sweep results at min_score 0.30:
///   0.70/0.20/0.10 -> 71%   0.85/0.10/0.05 -> 75%   1.00/0.00/0.00 -> 79%
/// Pure similarity scores best on the harness and is still NOT chosen: no
/// question in the set depends on recency or engagement, so a zero there
/// optimizes the metric by deleting a signal the metric cannot see. These
/// weights keep decay and reinforcement in the blend at the smallest weight
/// that does not cost measured accuracy.
pub const DEFAULT_W_SIM: f32 = 0.85;
pub const DEFAULT_W_RECENCY: f32 = 0.10;
pub const DEFAULT_W_STRENGTH: f32 = 0.05;

pub fn ranking_weights() -> (f32, f32, f32) {
    static W: std::sync::OnceLock<(f32, f32, f32)> = std::sync::OnceLock::new();
    *W.get_or_init(|| {
        let read = |k: &str, d: f32| env::var(k).ok().and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        (
            read("MACH_KB_W_SIM", DEFAULT_W_SIM),
            read("MACH_KB_W_RECENCY", DEFAULT_W_RECENCY),
            read("MACH_KB_W_STRENGTH", DEFAULT_W_STRENGTH),
        )
    })
}

/// Rescales raw cosine so `SIM_NOISE_FLOOR` maps to 0 and 1.0 stays 1.0,
/// making the similarity term mean "how far above chance is this" -- which
/// is what a threshold needs it to mean.
pub fn normalize_sim(raw: f32) -> f32 {
    ((raw - SIM_NOISE_FLOOR) / (1.0 - SIM_NOISE_FLOOR)).clamp(0.0, 1.0)
}

/// How many lexical candidates to keep, ordered by term coverage. Bounds
/// what the ranking scan carries, not which rows are considered -- coverage
/// is computed over every row matching any query term.
pub const LEXICAL_CANDIDATES: usize = 200;

/// `search_ranked` plus the FTS5 lexical channel (Honcho-style hybrid):
/// the query's exact tokens are looked up in `memories_fts` and scored as
/// IDF-weighted term coverage (see `lexical_scores`) -- 1.0 means the row
/// matched every query term's IDF mass, not "best hit in this result set"
/// -- and each row's similarity term becomes `max(cosine, LEXICAL_WEIGHT *
/// lexical)`. The recency/strength/penalty blend is unchanged, so scores
/// stay on the same scale the hooks threshold against (`kb-recall.py`'s
/// 0.45). A row with no
/// embedding can now surface on a lexical hit alone; a row with neither is
/// still excluded. Any FTS failure (bad syntax that slipped past
/// `fts_query`, index missing) degrades to plain cosine, never an error.
///
/// `date_anchor`, when `Some`, is the "today" `query_date_range` resolves
/// relative-date terms against, in place of `now`'s own date -- `now`
/// itself is passed through unchanged to `rank_with_lexical` and keeps
/// driving recency/strength, so this anchors ONLY the date-range parse.
/// `None` (every caller but `mach kb eval`'s `as_of`) keeps today's date
/// exactly as before this parameter existed.
pub fn search_hybrid(
    conn: &Connection,
    query_text: &str,
    query_embedding: &[f32],
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
    date_anchor: Option<&str>,
) -> Result<Vec<RankedHit>, KbError> {
    let lexical = lexical_scores(conn, query_text, LEXICAL_CANDIDATES).unwrap_or_default();
    let today = date_anchor.unwrap_or(now);
    let range = query_date_range(query_text, &today[..10.min(today.len())]);
    rank_with_lexical(
        conn,
        query_embedding,
        &lexical,
        range.as_ref(),
        limit,
        reviewed_only,
        include_superseded,
        min_score,
        now,
    )
}

#[allow(clippy::too_many_arguments)]
fn rank_with_lexical(
    conn: &Connection,
    query_embedding: &[f32],
    lexical: &HashMap<i64, f32>,
    range: Option<&DateRange>,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<RankedHit>, KbError> {
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    // Pass 1: per-candidate channel scores, no fusion yet -- RRF needs each
    // candidate's RANK within each channel, which is only knowable once
    // every candidate has been scored.
    struct Cand {
        memory: Memory,
        sim: f32,
        lex: f32,
        temporal: f32,
        recency: f32,
        strength: f32,
        superseded: bool,
    }
    let mut cands: Vec<Cand> = candidates(conn, !reviewed_only, include_superseded)?
        .into_iter()
        .filter_map(|m| {
            let lex = lexical.get(&m.id).copied().unwrap_or(0.0);
            let sim = match &m.embedding {
                Some(e) if !e.is_empty() => normalize_sim(cosine(query_embedding, e)),
                _ if lex > 0.0 => 0.0,
                _ => return None,
            };
            // Temporal channel: 1.0 when the query named a date range and
            // this memory's own occurrence range overlaps it. Only ever a
            // bonus -- a memory with no occurrence date is not penalised,
            // since "undated" is the normal state of most facts.
            let temporal = match (range, m.occurred_from.as_deref(), m.occurred_to.as_deref()) {
                (Some(r), Some(from), Some(to)) if r.overlaps(from, to) => 1.0,
                _ => 0.0,
            };
            let recency = compute_recency(&m, now_secs);
            let strength = compute_strength(&m, now_secs);
            let superseded = m.is_superseded();
            Some(Cand { memory: m, sim, lex, temporal, recency, strength, superseded })
        })
        .collect();

    // Pass 2: fuse the channels into one relevance term.
    let relevance: HashMap<i64, f32> = match fusion_mode() {
        Fusion::Max => cands
            .iter()
            .map(|c| {
                // Topical channels compete on max; the temporal channel
                // MULTIPLIES the winner instead of joining the contest.
                //
                // It used to be a third max() term at weight 0.75, and that
                // is wrong in a way the harness caught: a date is a
                // constraint, not a relevance signal. Asking about
                // "yesterday" overlaps 27 rows here, every one of them
                // scoring exactly 0.75, so the topical ordering inside the
                // day was erased and the tie broke on recency. The question
                // "what did I decide about umoja admin access yesterday"
                // returned four arbitrary rows from yesterday.
                //
                // As a multiplier the date lifts dated rows above undated
                // ones while preserving how well each one answers the rest
                // of the query.
                let base = c.sim.max(LEXICAL_WEIGHT * c.lex);
                // The floor keeps a date-only question answerable. "What
                // happened on 2026-08-31" carries no topical signal at all,
                // so the multiplicative term has nothing to multiply; the
                // floor lets occurrence alone clear the injection
                // threshold. Where topical signal DOES exist it exceeds the
                // floor and the multiplier orders the results.
                (c.memory.id, (base * (1.0 + TEMPORAL_BONUS * c.temporal)).max(TEMPORAL_FLOOR * c.temporal))
            })
            .collect(),
        Fusion::Rrf => {
            let mut rrf: HashMap<i64, f32> = cands.iter().map(|c| (c.memory.id, 0.0)).collect();
            // One ranked list per channel; a candidate scoring 0 on a
            // channel is simply absent from that list and contributes
            // nothing, which is RRF's own robustness-to-missing-items rule.
            for key in [0u8, 1u8, 2u8] {
                let mut ranked: Vec<(i64, f32)> = cands
                    .iter()
                    .map(|c| {
                        (c.memory.id, match key {
                            0 => c.sim,
                            1 => c.lex,
                            _ => c.temporal,
                        })
                    })
                    .filter(|(_, v)| *v > 0.0)
                    .collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                for (rank, (id, _)) in ranked.iter().enumerate() {
                    *rrf.entry(*id).or_insert(0.0) += rrf_contrib(rank);
                }
            }
            // Normalized against the best fused candidate so the result
            // stays on the same 0-1 scale the blend and the score floor
            // expect. RRF is rank-based, so its absolute magnitude is
            // meaningless on its own.
            let best = rrf.values().copied().fold(0.0f32, f32::max);
            if best > 0.0 {
                rrf.into_iter().map(|(id, v)| (id, v / best)).collect()
            } else {
                rrf
            }
        }
    };

    let (w_sim, w_rec, w_str) = ranking_weights();
    let mut scored: Vec<RankedHit> = cands
        .drain(..)
        .map(|c| {
            let rel = relevance.get(&c.memory.id).copied().unwrap_or(0.0);
            let mut score = w_sim * rel + w_rec * c.recency + w_str * c.strength;
            if c.superseded {
                score *= 0.1;
            }
            if !c.memory.reviewed {
                score *= UNREVIEWED_SEARCH_PENALTY;
            }
            RankedHit {
                memory: c.memory,
                score,
                sim: c.sim,
                recency: c.recency,
                strength: c.strength,
                superseded: c.superseded,
                lexical: c.lex,
            }
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    if min_score > 0.0 {
        scored.retain(|h| h.score >= min_score);
    }
    Ok(scored)
}

/// Words too common to carry lexical signal; dropped from FTS queries.
const FTS_STOPWORDS: &[&str] = &[
    "the", "and", "for", "that", "this", "with", "what", "does", "did", "do", "is", "are", "was", "were", "be",
    "been", "have", "has", "had", "you", "your", "our", "we", "they", "them", "their", "his", "her", "its", "it",
    "me", "my", "in", "on", "at", "to", "of", "a", "an", "or", "not", "no", "so", "if", "as", "by", "from", "about",
    "how", "why", "when", "where", "which", "who", "whom", "can", "could", "would", "should", "will", "just",
    "into", "than", "then", "there", "here", "also", "any", "all", "some", "more", "most", "want", "wants", "know",
    "like", "one", "two", "use", "used", "using", "get", "got", "make", "made", "please", "tell", "remember",
    // Quantifiers and placeholders. Contentless, but common enough in the
    // bank that one of them alone produced a 0.5 coverage score: "how many
    // siblings do I have" matched an unrelated memory purely on "many".
    "many", "much", "few", "several", "other", "another", "thing", "things", "something", "anything",
    "everything", "really", "actually", "very", "still", "even", "only", "same", "such", "each", "every",
];

/// Minimum IDF-weighted coverage (see `lexical_scores`) for the lexical
/// channel to count at all. Below this a "lexical match" is one or two
/// shared common words, which is not evidence: "what is the staging
/// postgres password" matched a deploy memory on "staging" alone, which is
/// not enough of the query's IDF mass to trust. 0.5 is also the coverage
/// real paraphrase answers reach (measured 0.5 to 1.0 on the harness).
pub const LEXICAL_MIN_COVERAGE: f32 = 0.5;

/// Turns free text into a safe FTS5 MATCH expression: lowercase alphanumeric
/// tokens (unicode61's own idea of a word), stopwords and 1-2 letter words
/// dropped (digits of any length kept -- "Q4", "43005"), deduped, capped at
/// 12, each double-quoted so no FTS operator syntax (`NEAR`, `*`, `:`, a
/// stray quote) can leak through, joined with OR. `None` when nothing
/// survives, in which case the lexical channel is skipped entirely.
pub fn fts_query(text: &str) -> Option<String> {
    let terms = fts_terms(text);
    if terms.is_empty() {
        return None;
    }
    Some(or_match_expr(&terms))
}

/// One safe FTS5 MATCH expression matching any of `terms`.
fn or_match_expr(terms: &[String]) -> String {
    terms.iter().map(|t| format!("\"{}\"", t)).collect::<Vec<_>>().join(" OR ")
}

/// The content tokens of `text`, deduped and capped -- the shared basis for
/// the MATCH expression AND for the coverage denominator, so "how many
/// terms did this memory match" is always measured against exactly the
/// terms that were searched for.
pub fn fts_terms(text: &str) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let tok = raw.to_lowercase();
        let has_digit = tok.chars().any(|c| c.is_ascii_digit());
        if !has_digit && tok.chars().count() < 3 {
            continue;
        }
        if FTS_STOPWORDS.contains(&tok.as_str()) {
            continue;
        }
        if seen.contains(&tok) {
            continue;
        }
        seen.push(tok);
        if seen.len() >= 12 {
            break;
        }
    }
    seen
}

/// `memory id -> lexical score` for the query's terms, from the FTS5 index.
/// The score is IDF-WEIGHTED COVERAGE: `sum(idf(t) for matched t) /
/// sum(idf(t) for all query terms)`, computed from a per-term MATCH and
/// capped at `limit` rows by coverage.
///
/// `idf(t) = ln((N + 1) / (df(t) + 1)) + 1`, where `df(t)` is the number of
/// `memories_fts` rows the term matches (the per-term MATCH loop already
/// counts these) and `N` is the total row count of `memories_fts`. This is
/// the standard smoothed IDF (as in scikit-learn's TF-IDF): `df(t) = N`
/// (every row matches) floors `idf(t)` at 1 rather than letting it hit 0,
/// so a ubiquitous term still counts for something; `df(t) = 0` (an unmatched
/// term) caps it at `ln(N + 1) + 1`, the maximum weight a never-seen term
/// can claim. A term absent from the corpus (`df(t) = 0`) still counts in
/// the DENOMINATOR (`sum(idf(t) for all query terms)`), deliberately: no row
/// can ever match it, so it permanently caps every candidate's achievable
/// coverage below 1.0 for that query. Dropping it instead would let a typo
/// or an out-of-vocabulary word be silently ignored -- "mach qwxzyv" would
/// then score every plain "mach" row a perfect 1.0, reintroducing exactly
/// the common-term-carries-a-weak-memory problem this task removes. The
/// embedding channel, not the lexical one, is what should catch a paraphrase
/// or misspelling; `search_hybrid` takes `max()` of the two for this reason.
///
/// Three earlier designs failed here, all worth naming:
///
/// 1. BM25 normalized against the best hit in the result set made the top
///    lexical hit exactly 1.0 however weak it was -- "what is the staging
///    postgres password" scored a memory that merely contains "staging" as
///    a PERFECT lexical match, and a perfect lexical match outranks
///    everything.
/// 2. Coverage computed only over the top-N BM25 rows dropped real answers:
///    BM25 on an OR query rewards rows matching RARE terms, so a memory
///    matching three common query terms ("rust", "agent", "vendor") could
///    sit outside the window and score 0. Coverage is now built from the
///    per-term matches directly, so the candidate set is exactly "every row
///    matching any query term" and the window only bounds the OUTPUT.
/// 3. Unweighted coverage (`matched_terms / total_terms`) let a match on
///    COMMON words carry a weak memory past the injection threshold: a
///    project name, "rule", or "convention" shared with the query counted
///    exactly as much as a rare, decisive term. 37% of logged injections
///    had sub-floor similarity, riding in on this. Weighting each matched
///    term by its rarity (IDF) fixes that: a memory has to match something
///    distinctive, not just something frequent, to clear the floor.
///
/// A fourth failure survived IDF weighting itself, on SHORT queries: with
/// only 2-3 terms surviving stopwording, one moderately common term plus
/// one incidental term can still add up to over half the query's total IDF
/// mass, because there just isn't a third or fourth rare term around to
/// dilute it. "why did I build the meeting recorder" (harness ms4) left
/// exactly 3 terms -- `build` (df 190/1607 rows, ~12% of the corpus, not a
/// stopword: it's a normal word in a software-engineering-heavy bank),
/// `meeting` (df 32), `recorder` (df 53). A memory that matched only
/// `build` plus `recorder` -- via an unrelated FastAPI/Starlette version
/// note whose text happened to contain "builds" and a stemmed "recording"
/// -- cleared 0.5 coverage (0.61) and outranked the real answer, which
/// matched `build` + `meeting` (0.65). Dropping `build` from
/// `FTS_STOPWORDS` was tried and reverted: it just let a different pair of
/// generic terms fill the same slot (see the ms4 section of the harness
/// report), because the defect isn't about any one token, it's structural.
/// The fix is a PARTIAL-match guard: when a candidate matches some but not
/// all of the query's terms, at least one matched term must have a corpus
/// df below the MEDIAN df of the query's own terms (computed once, over
/// every query term including unmatched and out-of-vocabulary ones, same
/// `dfs` the per-term MATCH loop already collected) -- otherwise the match
/// is entirely built from terms at or above the query's own "common" line
/// and is dropped regardless of coverage. `build` (df 190) sits above the
/// 3-term median (53, `recorder`'s own df) in the ms4 query, so build+
/// recorder no longer clears the floor, while build+meeting still does
/// (`meeting`'s df 32 is below the median). A FULL match (every query term
/// present) always passes: with nothing left unmatched there's no
/// "incidental" term to blame, whatever the terms' individual df -- this
/// is what keeps a query whose every term happens to have the same df (a
/// tiny corpus, or a query where all terms are equally rare) from being
/// wrongly zeroed out, since the median would otherwise equal every
/// matched term's own df. A single-term query has no median to compare
/// against (nothing to be "below") and is exempt entirely, unchanged from
/// before this guard existed.
///
/// Empty when the query has no usable terms or nothing matches. Errors
/// (index missing, syntax) propagate; `search_hybrid` treats them as "no
/// lexical channel at all".
pub fn lexical_scores(conn: &Connection, query_text: &str, limit: usize) -> Result<HashMap<i64, f32>, KbError> {
    let terms = fts_terms(query_text);
    if terms.is_empty() {
        return Ok(HashMap::new());
    }
    let total_docs: i64 = conn.query_row("SELECT count(*) FROM memories_fts", [], |r| r.get(0))?;
    // One MATCH per term. No stemmer of our own: FTS5's porter tokenizer
    // decides what "contains" means for both sides, so "reported" in the
    // query finds "reporting" in the text. `matched[id]` collects which
    // term INDICES hit that row, so a row's coverage sums the IDF of only
    // the terms it actually matched. `dfs` parallels `term_weights` (same
    // index) and feeds the dominant-term guard below.
    let mut matched: HashMap<i64, HashSet<usize>> = HashMap::new();
    let mut term_weights: Vec<f32> = Vec::with_capacity(terms.len());
    let mut dfs: Vec<i64> = Vec::with_capacity(terms.len());
    for (i, term) in terms.iter().enumerate() {
        let mut stmt = conn.prepare("SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1")?;
        let hits = stmt.query_map(params![format!("\"{}\"", term)], |r| r.get::<_, i64>(0))?;
        let mut df: i64 = 0;
        for h in hits {
            matched.entry(h?).or_default().insert(i);
            df += 1;
        }
        term_weights.push(term_idf(total_docs, df));
        dfs.push(df);
    }
    let total_idf: f32 = term_weights.iter().sum();
    // Dominant-term guard (median df of the query's OWN terms; `None` when
    // the query has under 2 terms, since a median needs something to
    // compare against). See the doc comment above for the ms4 case this
    // exists for.
    let median_df: Option<f32> = if dfs.len() >= 2 {
        let mut sorted = dfs.clone();
        sorted.sort_unstable();
        let mid = sorted.len() / 2;
        Some(if sorted.len() % 2 == 0 { (sorted[mid - 1] + sorted[mid]) as f32 / 2.0 } else { sorted[mid] as f32 })
    } else {
        None
    };
    let mut scored: Vec<(i64, f32)> = matched
        .into_iter()
        .map(|(id, term_idxs)| {
            let cov = if total_idf > 0.0 {
                term_idxs.iter().map(|&i| term_weights[i]).sum::<f32>() / total_idf
            } else {
                0.0
            };
            (id, cov.clamp(0.0, 1.0), term_idxs)
        })
        .filter(|(_, cov, term_idxs)| {
            if *cov < LEXICAL_MIN_COVERAGE {
                return false;
            }
            // A PARTIAL match (some query terms unmatched) needs at least
            // one matched term rarer than the query's own median -- see the
            // doc comment. A full match (every query term present) always
            // passes: there's no "incidental" term left to be riding on a
            // common one, whatever the terms' individual df.
            let is_partial = term_idxs.len() < terms.len();
            match (is_partial, median_df) {
                (true, Some(med)) => term_idxs.iter().any(|&i| (dfs[i] as f32) < med),
                _ => true,
            }
        })
        .map(|(id, cov, _)| (id, cov))
        .collect();
    // Highest coverage first, id as a stable tiebreak, then bound the output.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(b.0.cmp(&a.0)));
    scored.truncate(limit);
    Ok(scored.into_iter().collect())
}

/// `idf(t) = ln((N + 1) / (df(t) + 1)) + 1` — the smoothed IDF `lexical_scores`
/// documents in full (as in scikit-learn's TF-IDF): floored at 1 for a term
/// every row matches (`df = N`), capped at `ln(N + 1) + 1` for a term no row
/// matches (`df = 0`). Factored out so `lexical_scores` (query/corpus
/// ranking) and `winner_term_coverage` (single-pair coverage, used by
/// `supersession_guard`) share one formula rather than two copies drifting
/// apart.
fn term_idf(total_docs: i64, df: i64) -> f32 {
    (((total_docs + 1) as f32) / ((df + 1) as f32)).ln() + 1.0
}

/// IDF-weighted coverage of `loser_content`'s terms (`fts_terms` — the same
/// salience-capped, stopword-filtered token list `lexical_scores` extracts
/// from a query) by one specific row, `winner_id` — "does the winner's own
/// content actually carry what the loser said," which is what
/// `supersession_guard`'s coverage check (Dedupe only) turns into a block.
///
/// Same per-term FTS5 MATCH + `term_idf` machinery as `lexical_scores`,
/// applied to a single candidate instead of ranking a whole result set: for
/// each of the loser's terms, the per-term MATCH's hit set either contains
/// `winner_id` (covered, weighted by that term's corpus-wide rarity) or it
/// doesn't (uncovered) — `sum(idf(t) for covered t) / sum(idf(t) for all
/// loser terms)`. A term absent from the corpus entirely still counts in the
/// denominator (same reasoning as `lexical_scores`'s own doc comment): it
/// can never be "covered" by anything, so it permanently caps achievable
/// coverage below 1.0 rather than being silently dropped.
///
/// No loser terms at all (nothing >= 3 alnum chars, or all stopwords) ->
/// nothing checkable -> full coverage (`1.0`): an unfingerprintable loser is
/// not itself evidence that the winner drops something.
pub fn winner_term_coverage(conn: &Connection, loser_content: &str, winner_id: i64) -> Result<f32, KbError> {
    let terms = fts_terms(loser_content);
    if terms.is_empty() {
        return Ok(1.0);
    }
    let total_docs: i64 = conn.query_row("SELECT count(*) FROM memories_fts", [], |r| r.get(0))?;
    let mut covered_idf = 0.0f32;
    let mut total_idf = 0.0f32;
    for term in &terms {
        let mut stmt = conn.prepare("SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?1")?;
        let hits = stmt.query_map(params![format!("\"{}\"", term)], |r| r.get::<_, i64>(0))?;
        let mut df: i64 = 0;
        let mut winner_hit = false;
        for h in hits {
            let id = h?;
            df += 1;
            if id == winner_id {
                winner_hit = true;
            }
        }
        let w = term_idf(total_docs, df);
        total_idf += w;
        if winner_hit {
            covered_idf += w;
        }
    }
    Ok(if total_idf > 0.0 { (covered_idf / total_idf).clamp(0.0, 1.0) } else { 1.0 })
}

/// Minimum IDF-weighted term coverage (`winner_term_coverage`) a dedupe
/// winner must reach over its loser's terms for `supersession_guard` to let
/// the merge through. Calibrated against all 162 pairs in the
/// `supersession_audit` label set (`mach kb audit-supersessions`'s verdicts,
/// computed via this exact function against the pre-repair corpus): 0.23
/// blocks 33/117 LOSSY pairs while falsely blocking only 3/45 OK pairs
/// (10% of 45 floors to 4 allowed) — the most LOSSY pairs any single
/// threshold can catch within that budget; the safe window is any value in
/// (0.227, 0.235], and 0.25 already overshoots the budget at 5/45 OK
/// falsely blocked. Measured, not assumed: the date guard alone blocks
/// 20/117 LOSSY (2/45 OK), this coverage guard alone blocks 33/117 LOSSY
/// (3/45 OK), and combined (either fires) 47/117 LOSSY are blocked at
/// 4/45 OK falsely blocked — still under the 10% budget, but leaving
/// 70/117 (60%) of historically-LOSSY tombstones caught by neither guard
/// and dependent entirely on the judges. Re-run `mach kb
/// audit-supersessions` after the guard has been live for a while and
/// recalibrate against what it actually lets through, rather than
/// assuming this split holds indefinitely. See `task-4-report.md` for
/// the full confusion table and the two neighbouring values tried.
pub const DEDUPE_MIN_COVERAGE: f32 = 0.23;

/// Which automatic pass is asking `supersession_guard` — determines which
/// checks run (see that function's own doc comment): every kind gets the
/// date guard, only `Dedupe` also gets the coverage guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    /// `run_dedupe_pass`'s `Keep` branch (`mach kb reflect`'s nightly dedupe
    /// pass).
    Dedupe,
    /// `apply_contradiction_verdict`'s `Conflict`/`ConflictRetro` (`mach kb
    /// reflect`'s contradiction patrol, and the strength-review sampler's
    /// own routing through the same function).
    Contradiction,
    /// `apply_verdict`'s `Supersede` (`mach kb add`'s save-time classifier).
    AddSupersede,
}

/// Deterministic pre-check every automatic supersession path runs BEFORE
/// tombstoning `loser` in favor of `winner` — `run_dedupe_pass`'s `Keep`
/// branch, `apply_contradiction_verdict`'s `Conflict`/`ConflictRetro`, and
/// `apply_verdict`'s `Supersede`. An LLM judge's verdict alone tombstoned
/// 117 of 162 audited pairs lossily (`mach kb audit-supersessions`); this
/// runs first and blocks the shapes that audit found the judge gets wrong
/// in a mechanically checkable way, so a bad verdict degrades to "keep both
/// rows" (the caller's existing pinned-row fallback) rather than destroying
/// information. `None` means proceed exactly as before this existed —
/// `is_pinned`, then `supersede`/`merge_and_supersede`.
///
/// - **Date guard** (every `kind`): a `loser` scoped to a specific date is a
///   historical snapshot, not a stale claim to correct (see `restore`'s own
///   doc comment for the same reasoning applied after the fact) — blocks
///   with `"dated fact"` when `loser.occurred_from` is set and that date
///   string does not appear verbatim in `winner`'s content (`occurred_from`
///   can come from a relative phrase with no literal ISO date anywhere in
///   the loser's own text, so this checks the resolved value against the
///   winner directly), or when `loser`'s content names an ISO date
///   (`iso_dates_in`) that does not also appear, verbatim, in `winner`'s
///   content. A date the winner *does* restate is not a dropped fact, so
///   that case is not blocked.
/// - **Coverage guard** (`GuardKind::Dedupe` only): blocks with `"winner
///   does not carry loser's content"` when `winner_term_coverage` — the
///   loser's terms, IDF-weighted, found in the specific winner row — falls
///   below `DEDUPE_MIN_COVERAGE`. Not run for `Contradiction`/`AddSupersede`:
///   a contradiction's winner and loser are, by construction, two
///   DIFFERENT claims about the same thing (that's what put them in the
///   contradiction band rather than the dedupe band), so low lexical
///   overlap is the expected, healthy case there, not evidence of loss —
///   and the add-time classifier already requires the judge to call
///   SUPERSEDE rather than UPDATE, a stronger signal than a dedupe KEEP. A
///   `winner_term_coverage` query failure fails safe as zero coverage
///   (blocked), consistent with "when unsure, keep both rows".
/// - **Index-owned guard** (every `kind`): blocks with `"index-owned"` when
///   EITHER `loser.source` or `winner.source` starts with
///   `INDEX_OWNED_SOURCE_PREFIX` (a winning index row would otherwise
///   tombstone a user memory into text the indexer later replaces)
///   (`"code-index:"`) -- a code-index summary memory is regenerated and
///   superseded ONLY by the indexer itself (`upsert_index_memory`), never by
///   dedupe, contradiction, or the add-time classifier. Checked first,
///   before the date/coverage guards even run, since no amount of date or
///   term-coverage agreement makes it correct for a different pass to
///   tombstone one of these rows.
pub fn supersession_guard(conn: &Connection, loser: &Memory, winner: &Memory, kind: GuardKind) -> Option<&'static str> {
    // Both sides: an index-owned row must neither be tombstoned by a
    // non-index pass (loser) nor tombstone a user memory by winning one
    // (winner) -- the index's own text is regenerated and replaced
    // wholesale, so a user fact merged "into" it would vanish with the
    // next regeneration.
    if is_index_owned(loser) || is_index_owned(winner) {
        return Some("index-owned");
    }
    if let Some(from) = &loser.occurred_from {
        // `occurred_from` can be set from a relative phrase ("last
        // Tuesday") with no literal ISO date anywhere in the text, so this
        // checks the resolved date against the winner's content directly
        // rather than relying on `iso_dates_in` to find it in the loser
        // first.
        if !winner.content.contains(from.as_str()) {
            return Some("dated fact");
        }
    }
    if iso_dates_in(&loser.content).iter().any(|d| !winner.content.contains(d.as_str())) {
        return Some("dated fact");
    }
    if kind == GuardKind::Dedupe {
        let coverage = winner_term_coverage(conn, &loser.content, winner.id).unwrap_or(0.0);
        if coverage < DEDUPE_MIN_COVERAGE {
            return Some("winner does not carry loser's content");
        }
    }
    None
}

/// Fallback search when embedding the query failed (e.g. ollama is down):
/// a plain case-insensitive substring match over content, newest first.
/// Every non-superseded, reviewed hit gets score 1.0 (not a meaningful
/// ranking); a superseded hit included via `include_superseded` gets 0.1,
/// and an unreviewed hit gets `× UNREVIEWED_SEARCH_PENALTY` (the two
/// discounts stack), mirroring `search_ranked`'s treatment. `reviewed_only`
/// has the same organic-default meaning as `search_ranked`'s: `false`
/// (default) includes unreviewed rows, `true` excludes them.
pub fn search_substring(
    conn: &Connection,
    query: &str,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
) -> Result<Vec<(Memory, f32)>, KbError> {
    let needle = query.to_lowercase();
    let mut hits: Vec<(Memory, f32)> = candidates(conn, !reviewed_only, include_superseded)?
        .into_iter()
        .filter(|m| m.content.to_lowercase().contains(&needle))
        .map(|m| {
            let mut score = if m.is_superseded() { 0.1 } else { 1.0 };
            if !m.reviewed {
                score *= UNREVIEWED_SEARCH_PENALTY;
            }
            (m, score)
        })
        .collect();
    hits.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.0.id.cmp(&a.0.id))
    });
    hits.truncate(limit);
    Ok(hits)
}

/// k-NN top-`limit` over ACTIVE memories (reviewed, not tombstoned) — the
/// save-time supersession check in `mach kb add` runs this before deciding
/// whether a new fact is close enough to an existing one to ask the
/// classifier about it.
pub fn top_similar(conn: &Connection, query_embedding: &[f32], limit: usize) -> Result<Vec<(Memory, f32)>, KbError> {
    // Index-owned rows are never save-time classifier candidates: a NOOP
    // ("duplicate of #N") against one would drop the user's fact in favour
    // of text the indexer replaces on its next regeneration.
    let mut scored: Vec<(Memory, f32)> = candidates(conn, false, false)?
        .into_iter()
        .filter(|m| !is_index_owned(m))
        .filter_map(|m| {
            let score = match &m.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ => return None,
            };
            Some((m, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// What `apply_verdict` actually did to the store, for the CLI to report.
pub enum AddOutcome {
    Added { id: i64 },
    /// `Verdict::Update`: the new fact refines an existing one about the
    /// same thing. Both rows stay active — `old_id` is never touched.
    AddedRefining { new_id: i64, old_id: i64 },
    AddedAndTombstoned { new_id: i64, old_id: i64, verb: &'static str },
    Skipped { reason: String },
}

/// Applies a classifier verdict (or the default `Add` when classification
/// was skipped) to the store.
///
/// `UPDATE` and `SUPERSEDE` diverge here: `UPDATE <id>` means the new fact
/// *refines* memory `<id>` about the same thing, not that `<id>` is now
/// false, so it inserts the new row and leaves the old one exactly as it
/// was — no tombstone, pinned or not. Only `SUPERSEDE <id>` retires the old
/// row, and it still inserts the new fact first and tombstones second:
/// inserting unconditionally means a stale or invalid id in the verdict
/// never costs the new fact, worst case it's just a plain add.
#[allow(clippy::too_many_arguments)]
pub fn apply_verdict(
    conn: &Connection,
    verdict: Verdict,
    content: &str,
    source: Option<&str>,
    project: Option<&str>,
    reviewed: bool,
    embedding: &[f32],
    importance: i64,
    now: &str,
    best_match_id: Option<i64>,
) -> Result<AddOutcome, KbError> {
    match verdict {
        Verdict::Noop => Ok(AddOutcome::Skipped {
            reason: match best_match_id {
                Some(id) => format!("classifier verdict NOOP — duplicate of existing memory #{}", id),
                None => "classifier verdict NOOP — duplicate of an existing memory".to_string(),
            },
        }),
        Verdict::Add => {
            let id = insert(conn, content, source, project, reviewed, Some(embedding), importance)?;
            Ok(AddOutcome::Added { id })
        }
        Verdict::Update(old_id) => {
            let new_id = insert(conn, content, source, project, reviewed, Some(embedding), importance)?;
            // Keep both, as history: UPDATE never tombstones. If the id the
            // classifier gave doesn't actually exist (hallucinated, or the
            // row was deleted between the k-NN check and the verdict) there
            // is nothing to "refine", so this degrades to a plain Add —
            // same fallback shape as Supersede's stale-id case below.
            if get(conn, old_id)?.is_some() {
                Ok(AddOutcome::AddedRefining { new_id, old_id })
            } else {
                Ok(AddOutcome::Added { id: new_id })
            }
        }
        Verdict::Supersede(old_id) => {
            let new_id = insert(conn, content, source, project, reviewed, Some(embedding), importance)?;
            // A pinned row (`mach kb restore` set that) is never touched
            // by automatic supersession, so `is_pinned` short-circuits
            // `supersede` entirely and this degrades to a plain Add —
            // same fallback as when the referenced id is already
            // gone/tombstoned below. `supersession_guard` runs right after
            // (`GuardKind::AddSupersede`): only the date guard applies here
            // (a dated loser the winner doesn't restate) — the coverage
            // guard is Dedupe-only, since a classifier SUPERSEDE verdict is
            // already a stronger signal than a dedupe pass's KEEP (see
            // `supersession_guard`'s own doc comment). A block degrades the
            // same way as the pin check above. Logged to stderr since this
            // path has no per-run "seen" table to record a block in the way
            // the reflect passes do.
            let tombstoned = if is_pinned(conn, old_id)? {
                false
            } else {
                match (get(conn, old_id)?, get(conn, new_id)?) {
                    (Some(loser), Some(winner)) => match supersession_guard(conn, &loser, &winner, GuardKind::AddSupersede) {
                        Some(reason) => {
                            eprintln!("mach kb: supersession blocked ({}) #{} -> #{}", reason, old_id, new_id);
                            false
                        }
                        None => supersede(conn, old_id, new_id, now)?,
                    },
                    // old_id already gone -- let supersede's own no-op
                    // contract decide (mirrors the stale-id case above).
                    _ => supersede(conn, old_id, new_id, now)?,
                }
            };
            if tombstoned {
                Ok(AddOutcome::AddedAndTombstoned { new_id, old_id, verb: "superseded" })
            } else {
                Ok(AddOutcome::Added { id: new_id })
            }
        }
    }
}

// --- reflection subsystem: insights + reflect_state ---

fn row_to_insight(row: &rusqlite::Row) -> rusqlite::Result<Insight> {
    let blob: Option<Vec<u8>> = row.get("embedding")?;
    let source_ids_json: String = row.get("source_ids")?;
    let source_ids: Vec<String> = serde_json::from_str(&source_ids_json).unwrap_or_default();
    Ok(Insight {
        id: row.get("id")?,
        text: row.get("text")?,
        created_at: row.get("created_at")?,
        confidence: row.get("confidence")?,
        source_ids,
        embedding: blob.map(|b| decode_embedding(&b)),
        invalidated_at: row.get("invalidated_at")?,
        flagged_at: row.get("flagged_at")?,
        last_verified_at: row.get("last_verified_at")?,
        level: row.get("level")?,
        revised_at: row.get("revised_at")?,
        prev_text: row.get("prev_text")?,
    })
}

/// Inserts a new level-1 insight (current time as `created_at`) and returns
/// its id.
pub fn insert_insight(
    conn: &Connection,
    text: &str,
    confidence: f64,
    source_ids: &[String],
    embedding: Option<&[f32]>,
) -> Result<i64, KbError> {
    insert_insight_leveled(conn, text, confidence, source_ids, embedding, 1)
}

/// Inserts a level-2 theme — same shape as a plain insight, just tagged
/// `level = 2`. Citation validity (>= 2 level-1 insights, never another
/// theme) is enforced by the caller (`cli::run_meta_pass`, via
/// `reflect::parse_theme`'s `known_insight_ids` check) before this is ever
/// called — this function itself does not re-validate `source_ids`.
pub fn insert_theme(
    conn: &Connection,
    text: &str,
    confidence: f64,
    source_ids: &[String],
    embedding: Option<&[f32]>,
) -> Result<i64, KbError> {
    insert_insight_leveled(conn, text, confidence, source_ids, embedding, 2)
}

fn insert_insight_leveled(
    conn: &Connection,
    text: &str,
    confidence: f64,
    source_ids: &[String],
    embedding: Option<&[f32]>,
    level: i64,
) -> Result<i64, KbError> {
    let created_at = now_rfc3339();
    let source_ids_json =
        serde_json::to_string(source_ids).map_err(|e| KbError::Other(format!("encoding source_ids: {}", e)))?;
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO insights (text, created_at, confidence, source_ids, embedding, level)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![text, created_at, confidence, source_ids_json, blob, level],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Every ACTIVE insight/theme whose `source_ids` cites `id` (a memory id for
/// level-1 insights, an insight id for level-2 themes -- the caller knows
/// which it asked about). `source_ids` is stored as a JSON array of quoted
/// strings, so the quoted form is matched to avoid `"12"` hitting `"112"`.
/// Read-only provenance walk for `mach kb why`.
pub fn insights_citing(conn: &Connection, id: i64) -> Result<Vec<Insight>, KbError> {
    let needle = format!("%\"{}\"%", id);
    let mut stmt = conn.prepare(
        "SELECT * FROM insights WHERE invalidated_at IS NULL AND source_ids LIKE ?1 ORDER BY level ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![needle], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Memories this row replaced: every memory whose `superseded_by` points at
/// `id` (the classifier's UPDATE/SUPERSEDE tombstones). Newest first.
pub fn predecessors_of(conn: &Connection, id: i64) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM memories WHERE superseded_by = ?1 ORDER BY id DESC")?;
    let rows = stmt.query_map(params![id], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Active graph edges whose evidence is memory `id`.
pub fn relations_evidenced_by(conn: &Connection, id: i64) -> Result<Vec<Relation>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT id, src, predicate, dst, evidence_memory_id, confidence, created_at, valid_from, invalidated_at, superseded_by
         FROM relations WHERE evidence_memory_id = ?1 AND invalidated_at IS NULL ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![id], |r| {
        Ok(Relation {
            id: r.get(0)?,
            src: r.get(1)?,
            predicate: r.get(2)?,
            dst: r.get(3)?,
            evidence_memory_id: r.get(4)?,
            confidence: r.get(5)?,
            created_at: r.get(6)?,
            valid_from: r.get(7)?,
            invalidated_at: r.get(8)?,
            superseded_by: r.get(9)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// One session-ingest event near a timestamp: `(session_id, at, kind)` with
/// `kind` "ingested" (a completed `ingest-sessions` pass, from
/// `ingested_sessions`) or "checkpoint" (a mid-session `--partial` digest,
/// from `session_progress`). A `session-digest` memory records no session
/// id of its own, so `mach kb why` reconstructs the likely origin by
/// proximity: whatever digest landed within `window_secs` of the memory's
/// `created_at`. Heuristic, and labelled as such by the caller.
pub fn sessions_near(conn: &Connection, at: &str, window_secs: i64) -> Result<Vec<(String, String, &'static str)>, KbError> {
    let Some(t0) = parse_rfc3339(at) else {
        return Ok(Vec::new());
    };
    let mut out: Vec<(String, String, &'static str)> = Vec::new();
    for (sql, kind) in [
        ("SELECT session_id, ingested_at FROM ingested_sessions", "ingested"),
        ("SELECT session_id, updated_at FROM session_progress", "checkpoint"),
    ] {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (sid, ts) = r?;
            if let Some(t) = parse_rfc3339(&ts) {
                if (t - t0).abs() <= window_secs {
                    out.push((sid, ts, kind));
                }
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

pub fn get_insight(conn: &Connection, id: i64) -> Result<Option<Insight>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM insights WHERE id = ?1")?;
    Ok(stmt.query_row(params![id], row_to_insight).optional()?)
}

/// Active (non-invalidated) insights, newest first, optionally filtered to
/// only those currently flagged by re-verification — `mach kb insights
/// [--flagged]`.
pub fn list_insights(conn: &Connection, flagged_only: bool) -> Result<Vec<Insight>, KbError> {
    let sql = if flagged_only {
        "SELECT * FROM insights WHERE invalidated_at IS NULL AND flagged_at IS NOT NULL ORDER BY id DESC"
    } else {
        "SELECT * FROM insights WHERE invalidated_at IS NULL ORDER BY id DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// All non-invalidated insights, unordered filter aside — the pool
/// `search_insights_ranked`/`top_similar_insights` rank over.
fn active_insights(conn: &Connection) -> Result<Vec<Insight>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM insights WHERE invalidated_at IS NULL")?;
    let rows = stmt.query_map([], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Hard delete — `mach kb insight-forget <id>`. Returns whether a row
/// existed.
pub fn delete_insight(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let n = conn.execute("DELETE FROM insights WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// How much an insight's confidence moves per verification outcome.
/// Reinforcement (in `reinforce_insight`) raises it; a contradiction found
/// by re-verification lowers it by twice as much, following Hindsight's
/// opinion-update rule (`+a` reinforce, `-2a` contradict): evidence AGAINST
/// a belief is stronger information than one more instance of evidence for
/// it, because a belief only survives by not being contradicted.
///
/// Before this, confidence was monotonically increasing -- reinforcement
/// raised it, capped at 0.9, and a contradiction only set `flagged_at`. A
/// belief could therefore be flagged as doubted while still displaying the
/// highest confidence in the bank, which is exactly backwards.
pub const INSIGHT_CONFIDENCE_STEP: f64 = 0.1;
pub const INSIGHT_CONFIDENCE_FLOOR: f64 = 0.05;

/// Marks an insight flagged by re-verification (evidence no longer
/// supports it) AND lowers its confidence by `2 * INSIGHT_CONFIDENCE_STEP`.
/// The flag is never cleared automatically — `mach kb insight-forget` is
/// the only way a flagged insight goes away — but the confidence keeps
/// moving, so a belief contradicted repeatedly decays toward the floor
/// instead of sitting at a fixed "doubted but confident".
///
/// Also bumps `last_verified_at = now`, same as `weaken_insight`/
/// `mark_insight_verified` — a flag IS the outcome of a verification check
/// that just happened, so it counts the same way. Before this, a flagged
/// row kept whatever `last_verified_at` (often `NULL`, never-verified) it
/// had before the flag, which meant `insights_due_for_verification`'s
/// `ORDER BY last_verified_at ASC` (NULLs first) kept the same flagged rows
/// at the head of the queue run after run, crowding out every other
/// insight actually due for a first or repeat check.
pub fn flag_insight(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE insights SET flagged_at = ?1,
             confidence = MAX(?2, confidence - ?3),
             last_verified_at = ?1
         WHERE id = ?4",
        params![now, INSIGHT_CONFIDENCE_FLOOR, 2.0 * INSIGHT_CONFIDENCE_STEP, id],
    )?;
    Ok(n > 0)
}

/// Lowers an insight's confidence by one step without flagging it: the
/// "weaken" verdict between "still holds" and "contradicted", used when
/// evidence has thinned rather than turned.
pub fn weaken_insight(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE insights SET confidence = MAX(?1, confidence - ?2), last_verified_at = ?3 WHERE id = ?4",
        params![INSIGHT_CONFIDENCE_FLOOR, INSIGHT_CONFIDENCE_STEP, now, id],
    )?;
    Ok(n > 0)
}

/// Bumps `last_verified_at` after a re-verification pass finds an insight's
/// evidence still holds.
pub fn mark_insight_verified(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE insights SET last_verified_at = ?1 WHERE id = ?2", params![now, id])?;
    Ok(n > 0)
}

/// Rewrites an insight's text in place after re-verification's revise/
/// drop/keep judge (`reflect::build_revise_prompt`/`parse_revise`) finds
/// the current evidence supports a CORRECTED belief rather than the
/// original wording or nothing at all — `cli::run_insight_stage`'s `REVISE`
/// branch, the one path that mutates `Insight::text` after creation.
///
/// Unlike `mark_insight_verified`, a `REVISE` is not a free pass: the
/// ORIGINAL wording didn't hold, so `confidence` is lowered by one
/// `INSIGHT_CONFIDENCE_STEP` (floored at `INSIGHT_CONFIDENCE_FLOOR`) —
/// the same step `weaken_insight` uses, less severe than `flag_insight`'s
/// two-step drop since a REVISE means the belief itself survives,
/// corrected, rather than being wholly rejected.
///
/// `cited_ids` — the memory ids `reflect::parse_revise` validated against
/// the evidence the judge was shown — are appended to `source_ids`
/// (deduped against what's already there, same convention as
/// `reinforce_insight`): the corrected wording is now also backed by
/// whatever new evidence the judge cited for it.
///
/// Stashes the old text in `prev_text` (one hop back — a second revision
/// overwrites `prev_text` with the wording it just replaced, not a full
/// history), re-embeds if the caller has a fresh embedding (`None` when
/// the embedder itself failed, in which case the OLD embedding is kept
/// rather than wiped to null — a stale-but-present vector still ranks
/// better than none; `cli::run_insight_stage`'s own call site instead
/// skips calling this function at all on an embed failure, so `None` here
/// is for other/future callers), stamps `revised_at`, bumps
/// `last_verified_at` (a revision IS a verification outcome that just
/// happened), and clears any stale `flagged_at` (a `REVISE` verdict means
/// the belief, corrected, is supported again).
///
/// Also clears any `insight_dedupe_seen` rows citing `id` — the dedupe
/// pass judged the OLD wording against its neighbors; a rewritten belief
/// deserves a fresh look rather than staying silently excluded from
/// future candidate pairs on the strength of a verdict about text that no
/// longer exists.
///
/// Returns `false` if `id` doesn't exist — never panics on a stale or
/// hallucinated target.
pub fn revise_insight(
    conn: &Connection,
    id: i64,
    new_text: &str,
    new_embedding: Option<&[f32]>,
    cited_ids: &[i64],
    now: &str,
) -> Result<bool, KbError> {
    let Some(prev) = get_insight(conn, id)? else {
        return Ok(false);
    };
    let mut ids = prev.source_ids.clone();
    for cid in cited_ids {
        let s = cid.to_string();
        if !ids.contains(&s) {
            ids.push(s);
        }
    }
    let source_ids_json =
        serde_json::to_string(&ids).map_err(|e| KbError::Other(format!("encoding source_ids: {}", e)))?;
    let confidence = (prev.confidence - INSIGHT_CONFIDENCE_STEP).max(INSIGHT_CONFIDENCE_FLOOR);
    let n = match new_embedding {
        Some(emb) => {
            let blob = encode_embedding(emb);
            conn.execute(
                "UPDATE insights SET text = ?1, embedding = ?2, prev_text = ?3, revised_at = ?4,
                     last_verified_at = ?4, flagged_at = NULL, source_ids = ?5, confidence = ?6
                 WHERE id = ?7",
                params![new_text, blob, prev.text, now, source_ids_json, confidence, id],
            )?
        }
        None => conn.execute(
            "UPDATE insights SET text = ?1, prev_text = ?2, revised_at = ?3,
                 last_verified_at = ?3, flagged_at = NULL, source_ids = ?4, confidence = ?5
             WHERE id = ?6",
            params![new_text, prev.text, now, source_ids_json, confidence, id],
        )?,
    };
    if n > 0 {
        conn.execute("DELETE FROM insight_dedupe_seen WHERE id_a = ?1 OR id_b = ?1", params![id])?;
    }
    Ok(n > 0)
}

/// Up to `limit` active insights due for re-verification, oldest
/// `last_verified_at` first — SQLite sorts NULL first in `ASC` order, so
/// never-verified insights are naturally prioritized ahead of merely-stale
/// ones without a separate `CASE` clause.
pub fn insights_due_for_verification(conn: &Connection, limit: usize) -> Result<Vec<Insight>, KbError> {
    let sql = format!(
        "SELECT * FROM insights WHERE invalidated_at IS NULL
         ORDER BY last_verified_at ASC, id ASC LIMIT {}",
        limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// One ranked insight search hit, paralleling `RankedHit` — no `strength`
/// component: insights aren't reinforced by `--touch` (their currency comes
/// from re-verification instead), so that term of the blend is fixed at 0.
pub struct InsightHit {
    pub insight: Insight,
    pub score: f32,
    pub sim: f32,
    pub recency: f32,
}

/// Stand-in "stability" for insight recency decay. Insights have no
/// `access_count`/`stability` growth path (excluded from reinforcement by
/// design), so there's nothing to derive a per-row value from; this fixed
/// constant plays the same role `Memory::effective_stability` plays for
/// memories.
const INSIGHT_RECENCY_STABILITY_DAYS: f64 = 60.0;

fn compute_insight_recency(insight: &Insight, now: i64) -> f32 {
    let last = insight.last_verified_at.as_deref().unwrap_or(&insight.created_at);
    let last_secs = parse_rfc3339(last).unwrap_or(now);
    let days = days_between(now, last_secs);
    (-days / INSIGHT_RECENCY_STABILITY_DAYS).exp() as f32
}

/// Ranked top-N search over active insights, for blending into `mach kb
/// search`. Uses `normalize_sim` and the SAME `ranking_weights` the memory
/// blend uses, with the strength term fixed at 0 (insights are never
/// engagement-reinforced).
///
/// Sharing the scale is not cosmetic. When memories moved to normalized
/// similarity and insights kept raw cosine, every insight outscored every
/// memory -- a paraphrase query returned six derived beliefs and not one
/// fact, and measured multi-session recall fell from 83% to 50%. Two
/// blends that get sorted into one list must be on one scale.
/// Minimum NORMALIZED similarity for an insight to be injected at all.
/// Insights are deliberately general ("user prefers direct technical
/// communication"), which makes them weakly similar to almost any
/// question -- once they shared the memory scale they started answering
/// questions the bank has nothing to say about, and measured abstention
/// fell from 80% to 40%. A summary has to be MORE clearly on-topic than a
/// fact to be worth injecting, not less. Overridable for sweeps via
/// `MACH_KB_INSIGHT_MIN_SIM`.
pub const DEFAULT_INSIGHT_MIN_SIM: f32 = 0.30;

pub fn insight_min_sim() -> f32 {
    static V: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        env::var("MACH_KB_INSIGHT_MIN_SIM").ok().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_INSIGHT_MIN_SIM)
    })
}

pub fn search_insights_ranked(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
    now: &str,
) -> Result<Vec<InsightHit>, KbError> {
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    let mut scored: Vec<InsightHit> = active_insights(conn)?
        .into_iter()
        .filter_map(|insight| {
            let sim = match &insight.embedding {
                Some(e) if !e.is_empty() => normalize_sim(cosine(query_embedding, e)),
                _ => return None,
            };
            if sim < insight_min_sim() {
                return None;
            }
            let recency = compute_insight_recency(&insight, now_secs);
            let (w_sim, w_rec, _) = ranking_weights();
            let score = w_sim * sim + w_rec * recency;
            Some(InsightHit { insight, score, sim, recency })
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// Plain cosine top-N over active insights (no recency blend) — used by
/// reflection's stage 2 to show the model existing insights it might be
/// duplicating, not for ranked recall.
pub fn top_similar_insights(conn: &Connection, query_embedding: &[f32], limit: usize) -> Result<Vec<(Insight, f32)>, KbError> {
    let mut scored: Vec<(Insight, f32)> = active_insights(conn)?
        .into_iter()
        .filter_map(|insight| {
            let score = match &insight.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ => return None,
            };
            Some((insight, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// k-NN top-`limit` over the ACTIVE memory store (reviewed and unreviewed
/// alike, just not tombstoned) — unlike `top_similar` (reviewed-only, used
/// by `mach kb add`'s save-time dedup check), reflection deliberately
/// bridges into not-yet-reviewed digest candidates too.
pub fn top_similar_active(conn: &Connection, query_embedding: &[f32], limit: usize) -> Result<Vec<(Memory, f32)>, KbError> {
    let mut scored: Vec<(Memory, f32)> = candidates(conn, true, false)?
        .into_iter()
        .filter_map(|m| {
            let score = match &m.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ => return None,
            };
            Some((m, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// Active memories with `id > last_id`, ascending — `mach kb reflect`'s
/// input-selection step (both reviewed and unreviewed candidates count).
/// Dormant rows are excluded, same as everywhere else in a reflection
/// working set.
pub fn memories_since(conn: &Connection, last_id: i64) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM memories WHERE id > ?1 AND invalidated_at IS NULL AND dormant_at IS NULL ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![last_id], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Count of active, non-dormant memories the reflect insight stage has not
/// yet examined (`reflected_at IS NULL`) — the reflect summary's own
/// `backlog=` figure, and the cheap DB-only pre-check `cmd_reflect` uses to
/// decide whether the insight stage has anything to do at all, without
/// materializing any rows. Replaces the old watermark-based `has_new` check
/// (`memories_since(last_id).is_empty()`), which stopped meaning "is there
/// backlog" the moment the watermark itself stopped moving — see
/// `SCHEMA_VERSION`'s v29->v30 migration for why.
pub fn unreflected_active_count(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE reflected_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}

/// The `limit` most recently created unreflected active memories, newest
/// id first. Half of `cli::reflect_working_set`'s oldest/newest split — see
/// its own doc comment for the full rationale.
pub fn unreflected_active_newest(conn: &Connection, limit: usize) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM memories WHERE reflected_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL \
         ORDER BY id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// The `limit` oldest unreflected active memories, oldest id first,
/// skipping any id already in `exclude` — the other half of
/// `cli::reflect_working_set`'s split. `exclude` is normally the newest
/// slice already chosen, so the two halves never overlap even when the
/// whole backlog is smaller than the newest slice's own cap (every id
/// would otherwise be picked by both queries independently). Scans oldest-
/// first and stops as soon as `limit` rows are collected, so this stays
/// cheap even against a large backlog as long as `exclude` doesn't force it
/// deep into the table (it normally holds the highest ids, which this scan
/// only reaches last).
pub fn unreflected_active_oldest(
    conn: &Connection,
    limit: usize,
    exclude: &HashSet<i64>,
) -> Result<Vec<Memory>, KbError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT * FROM memories WHERE reflected_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL \
         ORDER BY id ASC",
    )?;
    let mut rows = stmt.query(params![])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let m = row_to_memory(row)?;
        if exclude.contains(&m.id) {
            continue;
        }
        out.push(m);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Marks every id in `ids` reflected as of `now` — the insight stage's own
/// per-memory progress marker (`memories.reflected_at`, schema v30),
/// replacing the old all-or-nothing `reflect_state.last_memory_id`
/// watermark. Set only when the insight stage's own working set finished
/// with no LLM failure in that stage (see `reflect::should_mark_reflected`
/// and `cli::run_insight_stage`) — a failure in any other pass never
/// reaches this call. Overwriting an already-reflected row (a race with a
/// concurrent run, or a row reflected twice across two backfills) is
/// harmless: it just means "examined again," never destructive.
pub fn mark_memories_reflected(conn: &Connection, ids: &[i64], now: &str) -> Result<usize, KbError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    let mut updated = 0usize;
    for id in ids {
        updated += tx.execute("UPDATE memories SET reflected_at = ?1 WHERE id = ?2", params![now, id])?;
    }
    tx.commit()?;
    Ok(updated)
}

/// Ids "new since the last reflect run" for the nightly dedupe and
/// contradiction passes, now derived from `memories.reflected_at` instead
/// of the retired `reflect_state` watermark: unioned as (a) still-
/// unreflected backlog (`reflected_at IS NULL`, exactly the insight
/// stage's own definition of new) and (b) rows reflected at or after
/// `previous_run_start` — normally this very run's own working set, marked
/// moments before dedupe/contradiction run, plus anything the immediately
/// preceding run reflected. Without (b), a memory the insight stage just
/// finished examining would have to wait a full extra `mach kb reflect`
/// invocation before dedupe/contradiction ever got a look at it, since (a)
/// alone stops covering it the instant it's marked reflected.
/// `previous_run_start` is `reflect_state.last_run_at` as read at the start
/// of the current run, before this run overwrites it — `None` on a
/// database that has never completed a run (every active row then counts
/// as new via (a) alone, since nothing has been reflected yet).
pub fn ids_new_since_reflect(conn: &Connection, previous_run_start: Option<&str>) -> Result<HashSet<i64>, KbError> {
    let mut out = HashSet::new();
    let mut stmt = match previous_run_start {
        Some(_) => conn.prepare(
            "SELECT id FROM memories WHERE invalidated_at IS NULL AND dormant_at IS NULL \
             AND (reflected_at IS NULL OR reflected_at >= ?1)",
        )?,
        None => conn.prepare(
            "SELECT id FROM memories WHERE invalidated_at IS NULL AND dormant_at IS NULL AND reflected_at IS NULL",
        )?,
    };
    let ids: Vec<i64> = match previous_run_start {
        Some(cutoff) => stmt.query_map(params![cutoff], |r| r.get(0))?.collect::<Result<_, _>>()?,
        None => stmt.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?,
    };
    out.extend(ids);
    Ok(out)
}

pub fn get_reflect_state(conn: &Connection) -> Result<ReflectState, KbError> {
    let row = conn
        .query_row(
            "SELECT last_run_at, last_memory_id, last_completed_at FROM reflect_state WHERE id = 1",
            [],
            |r| Ok(ReflectState { last_run_at: r.get(0)?, last_memory_id: r.get(1)?, last_completed_at: r.get(2)? }),
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

/// Upserts the `reflect_state` singleton row (id=1 is created by the v1->v2
/// migration, but `ON CONFLICT` makes this correct even against a database
/// that somehow never got that row).
pub fn update_reflect_state(conn: &Connection, last_run_at: &str, last_memory_id: Option<i64>) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO reflect_state (id, last_run_at, last_memory_id) VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET last_run_at = excluded.last_run_at, last_memory_id = excluded.last_memory_id",
        params![last_run_at, last_memory_id],
    )?;
    Ok(())
}

/// Records that `mach kb reflect` completed a run at `now` — set
/// unconditionally by every invocation that reaches its own end (including
/// a cheap "nothing new" early exit), but deliberately NOT by the
/// offline-defer early exit (see `ReflectState::last_completed_at`'s own
/// doc comment for why the two watermarks track different things). Only
/// ever touches this one column, via `ON CONFLICT` against the singleton
/// row the v1->v2 migration seeds — `update_reflect_state`'s own
/// `last_run_at`/`last_memory_id` are left exactly as they are.
pub fn mark_reflect_completed(conn: &Connection, now: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO reflect_state (id, last_completed_at) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET last_completed_at = excluded.last_completed_at",
        params![now],
    )?;
    Ok(())
}

// --- improve_state: `mach kb improve` watermarks ---

pub fn get_improve_state(conn: &Connection) -> Result<ImproveState, KbError> {
    let row = conn
        .query_row(
            "SELECT last_run_at, last_memory_id, last_relation_id, last_completed_at FROM improve_state WHERE id = 1",
            [],
            |r| {
                Ok(ImproveState {
                    last_run_at: r.get(0)?,
                    last_memory_id: r.get(1)?,
                    last_relation_id: r.get(2)?,
                    last_completed_at: r.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row.unwrap_or_default())
}

/// Advances the improve watermark trio together (see `ImproveState`).
pub fn update_improve_state(
    conn: &Connection,
    last_run_at: &str,
    last_memory_id: Option<i64>,
    last_relation_id: Option<i64>,
) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO improve_state (id, last_run_at, last_memory_id, last_relation_id) VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET last_run_at = excluded.last_run_at,
             last_memory_id = excluded.last_memory_id, last_relation_id = excluded.last_relation_id",
        params![last_run_at, last_memory_id, last_relation_id],
    )?;
    Ok(())
}

/// Records that `mach kb improve` reached its own end at `now` -- set on a
/// below-threshold exit too, never on the offline-defer exit.
pub fn mark_improve_completed(conn: &Connection, now: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO improve_state (id, last_completed_at) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET last_completed_at = excluded.last_completed_at",
        params![now],
    )?;
    Ok(())
}

/// Active relations with `id > last_id`, oldest first -- the improve pass's
/// affective-signal input, filtered by predicate on the caller's side.
pub fn relations_since(conn: &Connection, last_id: i64) -> Result<Vec<Relation>, KbError> {
    let mut stmt =
        conn.prepare("SELECT * FROM relations WHERE id > ?1 AND invalidated_at IS NULL ORDER BY id ASC")?;
    let rows = stmt.query_map(params![last_id], row_to_relation)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Highest memory id present (0 on an empty table) -- what an improve run
/// advances `last_memory_id` to, so a memory filtered out of the bundle is
/// still counted as seen.
pub fn latest_memory_id(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM memories", [], |r| r.get(0))?)
}

/// Highest relation id present (0 on an empty table); see `latest_memory_id`.
pub fn latest_relation_id(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM relations", [], |r| r.get(0))?)
}

/// Active memories whose `source` starts with `prefix` OR whose `project`
/// equals `project`, oldest first -- how the improve pass reads back its own
/// prior outcomes (`source = "improve <ts> ..."`, `project = "claude-config"`)
/// regardless of the watermark, so it always sees its full history.
pub fn memories_by_source_prefix_or_project(
    conn: &Connection,
    prefix: &str,
    project: &str,
) -> Result<Vec<Memory>, KbError> {
    let like = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
    let mut stmt = conn.prepare(
        "SELECT * FROM memories
         WHERE invalidated_at IS NULL AND dormant_at IS NULL
           AND (source LIKE ?1 ESCAPE '\\' OR project = ?2)
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![like, project], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// --- meta-reflection: level-2 themes ---

/// Count of active (non-invalidated) insights at exactly `level` — the
/// meta-reflection trigger's evidence-count input
/// (`reflect::should_run_meta_pass`).
pub fn count_active_insights_level(conn: &Connection, level: i64) -> Result<i64, KbError> {
    conn.query_row(
        "SELECT COUNT(*) FROM insights WHERE level = ?1 AND invalidated_at IS NULL",
        params![level],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// The newest active level-2 theme (highest id), if any — `None` means no
/// theme has ever been derived yet.
pub fn newest_active_theme(conn: &Connection) -> Result<Option<Insight>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM insights WHERE level = 2 AND invalidated_at IS NULL ORDER BY id DESC LIMIT 1",
    )?;
    Ok(stmt.query_row([], row_to_insight).optional()?)
}

/// Count of active level-1 insights created after `after_id`. Insights and
/// themes share one `id` sequence (same table), so "how many level-1
/// insights have accumulated since the newest theme" is simply "how many
/// have `id` greater than the theme's `id`."
pub fn count_active_level1_created_after(conn: &Connection, after_id: i64) -> Result<i64, KbError> {
    conn.query_row(
        "SELECT COUNT(*) FROM insights WHERE level = 1 AND invalidated_at IS NULL AND id > ?1",
        params![after_id],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// Active insights at exactly `level`, unordered — the meta-reflection
/// clustering pool (level 1) and `mach kb tree`'s theme/insight listings
/// (levels 2 and 1 respectively) both read from here.
pub fn active_insights_by_level(conn: &Connection, level: i64) -> Result<Vec<Insight>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM insights WHERE level = ?1 AND invalidated_at IS NULL")?;
    let rows = stmt.query_map(params![level], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Every level-1 insight id already cited (as an `i<id>` source) by some
/// active level-2 theme — the meta-reflection pass's "unthemed" filter, so
/// a theme's own member insights are never pulled into a later pass's
/// clustering (a level-1 insight belongs to at most one theme).
pub fn themed_insight_ids(conn: &Connection) -> Result<std::collections::HashSet<i64>, KbError> {
    let mut stmt = conn.prepare("SELECT source_ids FROM insights WHERE level = 2 AND invalidated_at IS NULL")?;
    let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?.collect::<Result<_, _>>()?;
    let mut set = std::collections::HashSet::new();
    for json in rows {
        let ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
        for s in ids {
            if let Some(rest) = s.strip_prefix('i').or_else(|| s.strip_prefix('I')) {
                if let Ok(id) = rest.parse::<i64>() {
                    set.insert(id);
                }
            }
        }
    }
    Ok(set)
}

/// Kind of a `mental_model` row — mirrors the literal words `mach kb model`
/// prints (`theme` / `belief`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Theme,
    Belief,
}

/// One row of the mental-model view (`mach kb model`): a theme or a
/// level-1 insight, stripped down to what's worth spending tokens on for
/// always-on context injection — no source ids, no memory leaves. `nested`
/// marks an insight printed under the theme that folded it in.
#[derive(Debug, Clone)]
pub struct ModelRow {
    pub kind: ModelKind,
    pub confidence: f64,
    pub text: String,
    pub doubted: bool,
    pub nested: bool,
    /// Created or revised within the last 7 days (`last_verified_at`
    /// deliberately excluded — see `model_is_recent`). Drives
    /// `cli::render_model_text`'s budget reservation for recent learning;
    /// not part of `--json` output (see `cli::to_model_row_json`).
    pub recent: bool,
}

/// Ranks rows the way `mental_model` wants them read: non-doubted before
/// doubted, then by descending `rank` (either raw confidence, for a
/// theme's nested beliefs, or the recency-boosted `model_score`, for the
/// merged top-level list — see call sites), then `id` as a stable
/// tiebreaker. Applied independently within each group it's called on —
/// never globally across a theme's children and another theme's children.
fn sort_model_rank<T>(items: &mut [T], flagged: impl Fn(&T) -> bool, rank: impl Fn(&T) -> f64, id: impl Fn(&T) -> i64) {
    items.sort_by(|a, b| {
        flagged(a)
            .cmp(&flagged(b))
            .then_with(|| rank(b).partial_cmp(&rank(a)).unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| id(a).cmp(&id(b)))
    });
}

/// Recency-boosted rank score for `mental_model`'s merged top-level list
/// (themes and unthemed beliefs, ranked together as of this task):
/// `confidence * (1 + 0.5 * recency)`, `recency = exp(-days_since(latest) /
/// 14)`, `latest` the most recent of `created_at` and `revised_at` —
/// deliberately NOT `last_verified_at`: a routine re-verification that
/// just confirmed an old belief still holds (`mark_insight_verified`)
/// bumps `last_verified_at` alone, and that isn't "recent learning" the
/// way authoring or a `REVISE` rewrite is; counting it would let a
/// verification-only pass keep an old row artificially boosted forever
/// just by being checked on schedule. A row touched (created or revised)
/// in the last couple weeks outranks an equally-confident but stale one;
/// recency decays to ~0 past a month or so, so an old row's score settles
/// back to its raw confidence. This is what keeps old high-confidence
/// themes from permanently burying new learning — see
/// `cli::render_model_text` for the budget-side half of that fix.
fn model_score(insight: &Insight, now: i64) -> f64 {
    let mut latest = parse_rfc3339(&insight.created_at).unwrap_or(now);
    if let Some(secs) = insight.revised_at.as_deref().and_then(parse_rfc3339) {
        latest = latest.max(secs);
    }
    let recency = (-days_between(now, latest) / 14.0).exp();
    insight.confidence * (1.0 + 0.5 * recency)
}

/// Whether `insight` was created or revised within the last 7 days.
/// `last_verified_at` is deliberately excluded — routine re-verification
/// touching an old row shouldn't count as "recent" the way authoring or a
/// rewrite does. Feeds `ModelRow::recent`, which
/// `cli::render_model_text` uses to reserve budget for recent learning.
fn model_is_recent(insight: &Insight, now: i64) -> bool {
    let created = parse_rfc3339(&insight.created_at).map(|c| days_between(now, c) <= 7.0).unwrap_or(false);
    let revised = insight
        .revised_at
        .as_deref()
        .and_then(parse_rfc3339)
        .map(|r| days_between(now, r) <= 7.0)
        .unwrap_or(false);
    created || revised
}

/// Builds the mental model over all active insights and themes: themes and
/// unthemed level-1 insights are ranked together in one top-level list by
/// `(doubted, -model_score, id)` — see `model_score` — so a recently
/// touched high-confidence belief can outrank an old theme instead of
/// every theme sorting ahead of every belief by construction. Each theme
/// is still immediately followed by its own level-1 insights (nested,
/// ranked among themselves by raw confidence via `sort_model_rank` — that
/// placement rule is unchanged from before this task). Same shape `mach kb
/// tree` renders, minus the memory citations — this view exists to be
/// cheap enough to inject on every session start, not to support
/// investigation.
pub fn mental_model(conn: &Connection) -> Result<Vec<ModelRow>, KbError> {
    let now = now_secs() as i64;
    let themes = active_insights_by_level(conn, 2)?;
    let level1 = active_insights_by_level(conn, 1)?;
    let themed = themed_insight_ids(conn)?;

    enum TopCandidate<'a> {
        Theme(&'a Insight, Vec<&'a Insight>),
        Unthemed(&'a Insight),
    }

    let mut top: Vec<TopCandidate> = Vec::new();
    for theme in &themes {
        let mut nested: Vec<&Insight> = Vec::new();
        for sid in &theme.source_ids {
            if let Some(rest) = sid.strip_prefix('i').or_else(|| sid.strip_prefix('I')) {
                if let Ok(iid) = rest.parse::<i64>() {
                    if let Some(ins) = level1.iter().find(|i| i.id == iid) {
                        nested.push(ins);
                    }
                }
            }
        }
        sort_model_rank(&mut nested, |i| i.is_flagged(), |i| i.confidence, |i| i.id);
        top.push(TopCandidate::Theme(theme, nested));
    }
    for ins in level1.iter().filter(|i| !themed.contains(&i.id)) {
        top.push(TopCandidate::Unthemed(ins));
    }

    sort_model_rank(
        &mut top,
        |c| match c {
            TopCandidate::Theme(t, _) => t.is_flagged(),
            TopCandidate::Unthemed(i) => i.is_flagged(),
        },
        |c| match c {
            TopCandidate::Theme(t, _) => model_score(t, now),
            TopCandidate::Unthemed(i) => model_score(i, now),
        },
        |c| match c {
            TopCandidate::Theme(t, _) => t.id,
            TopCandidate::Unthemed(i) => i.id,
        },
    );

    let mut rows = Vec::new();
    for c in top {
        match c {
            TopCandidate::Theme(theme, nested) => {
                rows.push(ModelRow {
                    kind: ModelKind::Theme,
                    confidence: theme.confidence,
                    text: theme.text.clone(),
                    doubted: theme.is_flagged(),
                    nested: false,
                    recent: model_is_recent(theme, now),
                });
                for ins in nested {
                    rows.push(ModelRow {
                        kind: ModelKind::Belief,
                        confidence: ins.confidence,
                        text: ins.text.clone(),
                        doubted: ins.is_flagged(),
                        nested: true,
                        recent: model_is_recent(ins, now),
                    });
                }
            }
            TopCandidate::Unthemed(ins) => {
                rows.push(ModelRow {
                    kind: ModelKind::Belief,
                    confidence: ins.confidence,
                    text: ins.text.clone(),
                    doubted: ins.is_flagged(),
                    nested: false,
                    recent: model_is_recent(ins, now),
                });
            }
        }
    }

    Ok(rows)
}

/// Recurrence reinforcement: appends `new_memory_ids` to an insight's
/// `source_ids` (deduped against what's already there), bumps `confidence`
/// by `0.05` per new id (capped at `0.9`, the same ceiling fresh insights
/// are capped at — see `reflect::compute_confidence`), and sets
/// `last_verified_at = now`. Used when stage 2's evidence re-confirms an
/// existing insight (`Stage2Result::Reinforce`) rather than yielding a new
/// one. Returns `false` if `id` doesn't exist — never panics on a stale or
/// hallucinated citation.
pub fn reinforce_insight(conn: &Connection, id: i64, new_memory_ids: &[i64], now: &str) -> Result<bool, KbError> {
    let insight = match get_insight(conn, id)? {
        Some(i) => i,
        None => return Ok(false),
    };
    let mut ids = insight.source_ids;
    let mut new_count: usize = 0;
    for mid in new_memory_ids {
        let s = mid.to_string();
        if !ids.contains(&s) {
            ids.push(s);
            new_count += 1;
        }
    }
    let confidence = (insight.confidence + 0.05 * new_count as f64).min(0.9);
    let source_ids_json =
        serde_json::to_string(&ids).map_err(|e| KbError::Other(format!("encoding source_ids: {}", e)))?;
    let n = conn.execute(
        "UPDATE insights SET source_ids = ?1, confidence = ?2, last_verified_at = ?3 WHERE id = ?4",
        params![source_ids_json, confidence, now, id],
    )?;
    Ok(n > 0)
}

// --- nightly dedupe pass ---

/// Normalizes a candidate pair into `(min, max)` order — `dedupe_seen`'s
/// storage and lookup key, so `(a, b)` and `(b, a)` are always the same row.
fn normalize_pair(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Records that a dedupe candidate pair was judged genuinely `DISTINCT` —
/// never asked about again on a future reflect run. Deliberately NOT called
/// for a malformed judge reply or a failed `claude` call: those get a fresh
/// chance next run instead of being silenced forever (see
/// `reflect::DedupeVerdict::Malformed`). A pair that instead resolves to a
/// tombstone never needs this either — the loser drops out of the active
/// pool that candidate generation scans, so it can't resurface as a pair.
pub fn mark_dedupe_seen(conn: &Connection, id_a: i64, id_b: i64) -> Result<(), KbError> {
    let (a, b) = normalize_pair(id_a, id_b);
    conn.execute("INSERT OR IGNORE INTO dedupe_seen (id_a, id_b) VALUES (?1, ?2)", params![a, b])?;
    Ok(())
}

/// Appends one `judge_log` row. `outcome` is the wrapped LLM call's result:
/// `Ok(reply)` fills `reply`, `Err(error)` fills `error`. Callers treat a
/// failure here as non-fatal — see `reflect::LoggedLlm`.
pub fn log_judge_call(
    conn: &Connection,
    pass: &str,
    model: &str,
    prompt: &str,
    outcome: Result<&str, &str>,
    latency_ms: u64,
    now: &str,
) -> Result<(), KbError> {
    let (reply, error) = match outcome {
        Ok(r) => (Some(r), None),
        Err(e) => (None, Some(e)),
    };
    conn.execute(
        "INSERT INTO judge_log (created_at, pass, model, prompt, reply, error, latency_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![now, pass, model, prompt, reply, error, latency_ms as i64],
    )?;
    Ok(())
}

/// Retention window for `judge_log` rows (`mach kb reflect`'s raw LLM-call
/// audit trail — see `migrate_v25_to_v26`) — 180 days. Purely diagnostic
/// training data (balanced haiku-verdict labels for evaluating a local
/// decision model down the line), never read by any ranking or
/// supersession logic, so an age-only cutoff is safe. Pruned at the end of
/// `mach kb reflect` by `prune_judge_log`.
pub const JUDGE_LOG_RETENTION_DAYS: i64 = 180;

/// Deletes every `judge_log` row whose `created_at` is older than
/// `older_than` (an RFC3339 timestamp, compared lexicographically like
/// every other stored timestamp here). Returns how many rows were deleted.
/// Called at the end of `mach kb reflect` with a cutoff
/// `JUDGE_LOG_RETENTION_DAYS` before now. Scoped to this one table only —
/// `supersession_audit` is a permanent record and this function (like its
/// `recall_engagement` sibling below) never touches it.
pub fn prune_judge_log(conn: &Connection, older_than: &str) -> Result<usize, KbError> {
    Ok(conn.execute("DELETE FROM judge_log WHERE created_at < ?1", params![older_than])?)
}

/// Persists one session's engagement verdicts: one `recall_engagement` row
/// per id in `shown` (the ids the recall log listed and
/// `ingest::parse_engagement_verdicts` actually returned a verdict for),
/// flagged `engaged` if it also appears in `engaged`. `INSERT OR REPLACE`
/// on the `(session_id, memory_id)` primary key, so re-running ingest for
/// the same session (a retry, or a forced `--session-id` re-run) overwrites
/// the prior verdict rather than duplicating it -- idempotent by
/// construction. This is the durable record `mach kb recall-stats` reads,
/// surviving the 14-day prune of the recall-log JSONL itself.
pub fn record_engagement(conn: &Connection, session_id: &str, shown: &[i64], engaged: &[i64], now: &str) -> Result<(), KbError> {
    let engaged_set: std::collections::HashSet<i64> = engaged.iter().copied().collect();
    for &id in shown {
        conn.execute(
            "INSERT OR REPLACE INTO recall_engagement (session_id, memory_id, engaged, judged_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![session_id, id, engaged_set.contains(&id) as i64, now],
        )?;
    }
    Ok(())
}

/// One session's aggregate from `recall_engagement`: how many injected ids
/// were shown (judged at all) and how many of those were engaged.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallStatsRow {
    pub session_id: String,
    pub shown: i64,
    pub engaged: i64,
}

/// Every session with at least one `recall_engagement` row judged at or
/// after `since` (an RFC3339 timestamp, compared lexicographically like
/// every other stored timestamp), grouped into shown/engaged counts and
/// ordered by session id. Backs `mach kb recall-stats`.
pub fn recall_stats_since(conn: &Connection, since: &str) -> Result<Vec<RecallStatsRow>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT session_id, COUNT(*), SUM(engaged)
         FROM recall_engagement
         WHERE judged_at >= ?1
         GROUP BY session_id
         ORDER BY session_id",
    )?;
    let rows = stmt.query_map(params![since], |r| {
        Ok(RecallStatsRow { session_id: r.get(0)?, shown: r.get(1)?, engaged: r.get::<_, i64>(2)? })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Retention window for `recall_engagement` rows (`mach kb recall-stats`'s
/// durable per-session shown/engaged verdicts — see `migrate_v26_to_v27`)
/// — 365 days, longer than `judge_log`'s because recall precision is
/// tracked over quarters, not weeks. Pruned at the end of `mach kb
/// reflect` by `prune_recall_engagement`.
pub const RECALL_ENGAGEMENT_RETENTION_DAYS: i64 = 365;

/// Deletes every `recall_engagement` row whose `judged_at` is older than
/// `older_than` (an RFC3339 timestamp, compared lexicographically). Returns
/// how many rows were deleted. Called at the end of `mach kb reflect` with
/// a cutoff `RECALL_ENGAGEMENT_RETENTION_DAYS` before now. Scoped to this
/// one table only — `supersession_audit` is a permanent record and this
/// function (like its `judge_log` sibling above) never touches it.
pub fn prune_recall_engagement(conn: &Connection, older_than: &str) -> Result<usize, KbError> {
    Ok(conn.execute("DELETE FROM recall_engagement WHERE judged_at < ?1", params![older_than])?)
}

/// Every pair ever recorded by `mark_dedupe_seen`, as normalized `(min,
/// max)` tuples — loaded once per reflect run so candidate generation
/// (`reflect::dedupe_candidate_pairs`) can exclude them with a plain set
/// lookup instead of a query per pair.
pub fn dedupe_seen_pairs(conn: &Connection) -> Result<std::collections::HashSet<(i64, i64)>, KbError> {
    let mut stmt = conn.prepare("SELECT id_a, id_b FROM dedupe_seen")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = std::collections::HashSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Merges a dedupe loser's earned reinforcement into its winner, then
/// tombstones the loser via the ordinary `supersede` mechanics: `winner`'s
/// `access_count` absorbs `loser`'s (summed, so reinforcement earned by
/// either copy survives), and `stability` becomes the max of the two
/// (`effective_stability`, so a NULL column on either side still compares
/// sanely). Returns `false` — without merging or tombstoning anything — if
/// either id doesn't exist or `loser_id` is already tombstoned, mirroring
/// `supersede`'s own no-op contract.
pub fn merge_and_supersede(conn: &Connection, loser_id: i64, winner_id: i64, now: &str) -> Result<bool, KbError> {
    let loser = match get(conn, loser_id)? {
        Some(m) => m,
        None => return Ok(false),
    };
    if loser.is_superseded() {
        return Ok(false);
    }
    let winner = match get(conn, winner_id)? {
        Some(m) => m,
        None => return Ok(false),
    };
    let merged_access_count = winner.access_count + loser.access_count;
    let merged_stability = winner.effective_stability().max(loser.effective_stability());
    conn.execute(
        "UPDATE memories SET access_count = ?1, stability = ?2 WHERE id = ?3",
        params![merged_access_count, merged_stability, winner_id],
    )?;
    supersede(conn, loser_id, winner_id, now)
}

/// Re-points every insight/theme's raw-memory citation of `old_id` to
/// `new_id` — called right after a dedupe merge tombstones `old_id`, so
/// provenance follows the merge: an insight that cited the loser now cites
/// the winner instead. Operates over every insight row regardless of
/// active/invalidated/flagged status (a provenance fix, not a currency
/// judgment). A citation already naming `new_id` collapses with the
/// re-pointed one rather than appearing twice (dedup preserves the first
/// occurrence's position; an untouched row's relative citation order is
/// otherwise left exactly as it was). Returns the number of insight rows
/// actually updated.
// --- insight dedupe: one belief, one row ---

/// Insight pairs already judged by the dedupe pass. Same "never re-pay for
/// a verdict" rule as `dedupe_seen` for memories; a failed judge call is
/// never recorded, so it is retried.
pub fn mark_insight_dedupe_seen(conn: &Connection, id_a: i64, id_b: i64) -> Result<(), KbError> {
    let (a, b) = if id_a <= id_b { (id_a, id_b) } else { (id_b, id_a) };
    conn.execute("INSERT OR IGNORE INTO insight_dedupe_seen (id_a, id_b) VALUES (?1, ?2)", params![a, b])?;
    Ok(())
}

pub fn insight_dedupe_seen_pairs(conn: &Connection) -> Result<std::collections::HashSet<(i64, i64)>, KbError> {
    let mut stmt = conn.prepare("SELECT id_a, id_b FROM insight_dedupe_seen")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = std::collections::HashSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Merges insight `drop_id` into `keep_id`: the union of their source ids
/// (deduped), the HIGHER of their confidences, and any theme citing the
/// loser repointed to the keeper. The loser is invalidated, never deleted.
///
/// Confidence takes the max rather than an average because both rows
/// describe one belief: the evidence that earned the higher number is still
/// evidence after the merge, and averaging would silently punish a belief
/// for having been written down twice.
pub fn merge_insights(conn: &Connection, keep_id: i64, drop_id: i64, now: &str) -> Result<bool, KbError> {
    let (Some(keep), Some(drop)) = (get_insight(conn, keep_id)?, get_insight(conn, drop_id)?) else {
        return Ok(false);
    };
    let mut ids = keep.source_ids.clone();
    for id in drop.source_ids {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());
    let confidence = keep.confidence.max(drop.confidence);
    conn.execute(
        "UPDATE insights SET source_ids = ?1, confidence = ?2, last_verified_at = ?3 WHERE id = ?4",
        params![json, confidence, now, keep_id],
    )?;
    repoint_insight_citations(conn, drop_id, keep_id, 2)?;
    conn.execute("UPDATE insights SET invalidated_at = ?1 WHERE id = ?2", params![now, drop_id])?;
    Ok(true)
}

/// Active insight pairs of the SAME level whose embeddings sit at or above
/// `min_sim`, excluding pairs already judged. Oldest-first within a pair so
/// the keeper is deterministic. Capped per run.
pub fn insight_dedupe_candidate_pairs(
    conn: &Connection,
    min_sim: f32,
    seen: &std::collections::HashSet<(i64, i64)>,
    cap: usize,
) -> Result<Vec<(i64, i64)>, KbError> {
    let all = active_insights(conn)?;
    let mut out: Vec<(f32, i64, i64)> = Vec::new();
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let (a, b) = (&all[i], &all[j]);
            if a.level != b.level {
                continue; // a theme and the insight under it are not duplicates
            }
            let (Some(ea), Some(eb)) = (a.embedding.as_deref(), b.embedding.as_deref()) else {
                continue;
            };
            if ea.is_empty() || eb.is_empty() {
                continue;
            }
            let sim = cosine(ea, eb);
            if sim < min_sim {
                continue;
            }
            let (lo, hi) = if a.id <= b.id { (a.id, b.id) } else { (b.id, a.id) };
            if seen.contains(&(lo, hi)) {
                continue;
            }
            out.push((sim, lo, hi));
        }
    }
    out.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(cap);
    Ok(out.into_iter().map(|(_, a, b)| (a, b)).collect())
}

/// Rewrites `old_id` to `new_id` inside the `source_ids` of insights AT
/// `cites_level`, deduping the result.
///
/// The level argument is not optional decoration. `source_ids` holds MEMORY
/// ids on a level-1 insight and INSIGHT ids on a level-2 theme, and the
/// column cannot tell you which -- so a scan of every row conflates the two
/// id spaces the moment the numbers collide. That is not hypothetical: when
/// insight #1 absorbed insight #2, this function rewrote the literal "2" in
/// insight #1's own evidence list (memory #2) into "1", silently swapping
/// one fact's evidence for another's. Callers must say which id space they
/// are repointing: 1 when merging memories, 2 when merging insights.
pub fn repoint_insight_citations(
    conn: &Connection,
    old_id: i64,
    new_id: i64,
    cites_level: i64,
) -> Result<usize, KbError> {
    let old_s = old_id.to_string();
    let new_s = new_id.to_string();
    let mut stmt = conn.prepare("SELECT id, source_ids FROM insights WHERE level = ?1")?;
    let rows: Vec<(i64, String)> =
        stmt.query_map(params![cites_level], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut updated = 0usize;
    for (id, json) in rows {
        let ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
        if !ids.iter().any(|s| *s == old_s) {
            continue;
        }
        let mut seen = std::collections::HashSet::new();
        let repointed: Vec<String> = ids
            .into_iter()
            .map(|s| if s == old_s { new_s.clone() } else { s })
            .filter(|s| seen.insert(s.clone()))
            .collect();
        let repointed_json =
            serde_json::to_string(&repointed).map_err(|e| KbError::Other(format!("encoding source_ids: {}", e)))?;
        conn.execute("UPDATE insights SET source_ids = ?1 WHERE id = ?2", params![repointed_json, id])?;
        updated += 1;
    }
    Ok(updated)
}

// --- contradiction patrol ---

/// Records that a contradiction-patrol candidate pair was judged `BOTH_HOLD`
/// — never asked about again on a future reflect run. Deliberately NOT
/// called for an `UNCLEAR` verdict or a failed `claude` call: those get a
/// fresh chance next run instead of being silenced forever (mirrors
/// `mark_dedupe_seen`'s own contract exactly). A pair that instead resolves
/// to `SUPERSEDES` never needs this either — the loser drops out of the
/// active pool that candidate generation scans, so it can't resurface as a
/// pair.
pub fn mark_contradiction_seen(conn: &Connection, id_a: i64, id_b: i64) -> Result<(), KbError> {
    let (a, b) = normalize_pair(id_a, id_b);
    conn.execute("INSERT OR IGNORE INTO contradiction_seen (id_a, id_b) VALUES (?1, ?2)", params![a, b])?;
    Ok(())
}

/// Every pair ever recorded by `mark_contradiction_seen`, as normalized
/// `(min, max)` tuples — loaded once per reflect run so candidate
/// generation (`reflect::contradiction_candidate_pairs`) can exclude them
/// with a plain set lookup, same shape as `dedupe_seen_pairs`. The caller
/// unions this with `dedupe_seen_pairs` before generating candidates: a pair
/// either judge has already ruled on needs no second opinion from the other.
pub fn contradiction_seen_pairs(conn: &Connection) -> Result<std::collections::HashSet<(i64, i64)>, KbError> {
    let mut stmt = conn.prepare("SELECT id_a, id_b FROM contradiction_seen")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = std::collections::HashSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Flags every active, not-already-flagged insight or theme whose
/// `source_ids` cite `memory_id` as a raw memory — used when the
/// contradiction patrol tombstones a memory rather than merely
/// deduplicating it. Deliberately flags rather than re-points (unlike
/// `repoint_insight_citations`, the dedupe pass's own provenance-preserving
/// mechanism for a same-fact merge): an insight built on the now-superseded
/// fact may itself rest on outdated evidence, so it needs a human to look at
/// it, not a silent citation swap to the winner. Returns the number of
/// insight rows actually flagged.
pub fn flag_insights_citing_memory(conn: &Connection, memory_id: i64, now: &str) -> Result<usize, KbError> {
    let mut flagged = 0usize;
    for id in downstream_of_memory(conn, memory_id)? {
        // Already-flagged rows are skipped so a repeat pass does not keep
        // subtracting confidence for the same dead fact.
        let already = get_insight(conn, id)?.map(|i| i.is_flagged()).unwrap_or(true);
        if !already && flag_insight(conn, id, now)? {
            flagged += 1;
        }
    }
    Ok(flagged)
}

/// Every ACTIVE insight and theme that rests on memory `memory_id`, however
/// many derivation steps away: the insights citing it, the themes citing
/// those insights, and so on.
///
/// One level was not enough. A memory feeds an insight, that insight feeds a
/// theme, and the theme is what the session-start mental model actually
/// shows -- so killing a fact used to leave the belief the user reads
/// untouched. Breadth-first with a visited set, so a citation cycle
/// terminates instead of spinning.
pub fn downstream_of_memory(conn: &Connection, memory_id: i64) -> Result<Vec<i64>, KbError> {
    let mut stmt = conn.prepare("SELECT id, source_ids FROM insights WHERE invalidated_at IS NULL")?;
    let rows: Vec<(i64, Vec<String>)> = stmt
        .query_map([], |r| {
            let json: String = r.get(1)?;
            Ok((r.get::<_, i64>(0)?, serde_json::from_str(&json).unwrap_or_default()))
        })?
        .collect::<Result<_, _>>()?;

    let mut out: Vec<i64> = Vec::new();
    let mut visited: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut frontier: Vec<String> = vec![memory_id.to_string()];
    while !frontier.is_empty() {
        let mut next: Vec<String> = Vec::new();
        for (id, ids) in &rows {
            if visited.contains(id) {
                continue;
            }
            if ids.iter().any(|s| frontier.contains(s)) {
                visited.insert(*id);
                out.push(*id);
                next.push(id.to_string()); // this insight's own id may be cited by a theme
            }
        }
        frontier = next;
    }
    out.sort_unstable();
    Ok(out)
}

// --- strength review sampler ---

/// Up to `limit` ACTIVE memories (not tombstoned, not dormant) at
/// `importance >= min_importance`, oldest `last_verified_at` first — SQLite
/// sorts NULL first in `ASC` order, so never-sampled memories are naturally
/// prioritized ahead of merely-stale ones, exactly mirroring
/// `insights_due_for_verification`'s own ordering trick.
pub fn memories_due_for_strength_review(conn: &Connection, limit: usize, min_importance: i64) -> Result<Vec<Memory>, KbError> {
    let sql = format!(
        "SELECT * FROM memories WHERE invalidated_at IS NULL AND dormant_at IS NULL AND importance >= ?1
           AND {}
         ORDER BY last_verified_at ASC, id ASC LIMIT {}",
        not_index_owned_sql("source"),
        limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![min_importance], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Bumps a memory's `last_verified_at` after the strength-review sampler
/// finds its evidence still holds (or merely aging — see `halve_stability`)
/// — the raw-memory counterpart of `mark_insight_verified`.
pub fn mark_memory_verified(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE memories SET last_verified_at = ?1 WHERE id = ?2", params![now, id])?;
    Ok(n > 0)
}

/// Halves a memory's effective `stability` (so it decays and gets recalled
/// less over time) when the strength-review sampler judges it STALE —
/// aging or weakening evidence, but not directly contradicted by anything
/// shown, so it is never tombstoned here; actual supersession stays the
/// contradiction patrol's job. `COALESCE` mirrors `touch`'s own fallback to
/// `importance * 7.0` for a row whose `stability` somehow ended up NULL.
pub fn halve_stability(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET stability = COALESCE(stability, importance * 7.0) / 2.0
         WHERE id = ?1 AND invalidated_at IS NULL",
        params![id],
    )?;
    Ok(n > 0)
}

/// Multiplies a memory's effective `stability` by `factor`, capped at 365
/// days -- the same cap `touch`'s own reinforcement growth uses. Used by
/// `mach kb reflect`'s curation pass for schema-accelerated consolidation
/// (`reflect::CURATION_SCHEMA_STABILITY_MULTIPLIER` — see
/// `cli::apply_schema_fast_path`'s own doc comment): a fact judged coherent
/// with an existing insight/theme consolidates faster than an orphan fact
/// with nothing to attach to. `COALESCE` mirrors `touch`/`halve_stability`'s
/// own fallback to `importance * 7.0` for a row whose `stability` somehow
/// ended up NULL.
pub fn multiply_stability(conn: &Connection, id: i64, factor: f64) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET stability = MIN(COALESCE(stability, importance * 7.0) * ?1, 365.0)
         WHERE id = ?2 AND invalidated_at IS NULL",
        params![factor, id],
    )?;
    Ok(n > 0)
}

// --- export/import support (mach kb export / mach kb import) ---

/// Every memory row, in every state (reviewed or not, tombstoned, dormant),
/// ascending by id — full-fidelity input for `mach kb export`.
pub fn all_memories(conn: &Connection) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM memories ORDER BY id ASC")?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Every insight/theme row, in every state, ascending by id — the insights
/// counterpart of `all_memories` for `mach kb export`.
pub fn all_insights(conn: &Connection) -> Result<Vec<Insight>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM insights ORDER BY id ASC")?;
    let rows = stmt.query_map([], row_to_insight)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Whether the store has no memories and no insights at all — `mach kb
/// import`'s refuse-without-`--merge` guard. The `reflect_state` singleton
/// row (always present after the v1->v2 migration) is deliberately not part
/// of this check: a database with only that watermark row is, for every
/// practical purpose, still an empty one.
pub fn is_db_empty(conn: &Connection) -> Result<bool, KbError> {
    let mem_count: i64 = conn.query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))?;
    if mem_count > 0 {
        return Ok(false);
    }
    let ins_count: i64 = conn.query_row("SELECT COUNT(*) FROM insights", [], |r| r.get(0))?;
    Ok(ins_count == 0)
}

/// Inserts or fully overwrites a memory row at its own explicit id — used
/// only by `mach kb import`, which is reconstructing rows from another
/// machine's export rather than minting new ones (ordinary `insert` always
/// lets SQLite pick the id). An `ON CONFLICT` upsert rather than a
/// look-up-then-branch: the caller (`export::import_from_reader`) has
/// already decided whether overwriting is correct (identical/older rows are
/// filtered out before this is ever called).
pub fn raw_upsert_memory(conn: &Connection, m: &Memory) -> Result<(), KbError> {
    let blob = m.embedding.as_deref().map(encode_embedding);
    conn.execute(
        "INSERT INTO memories
            (id, content, source, project, created_at, reviewed, embedding, importance, stability,
             access_count, first_accessed_at, last_accessed_at, valid_from, invalidated_at,
             superseded_by, dormant_at, last_verified_at, graph_extracted_at, basis,
             occurred_from, occurred_to, pinned_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
         ON CONFLICT(id) DO UPDATE SET
            content = excluded.content, source = excluded.source, project = excluded.project,
            created_at = excluded.created_at, reviewed = excluded.reviewed, embedding = excluded.embedding,
            importance = excluded.importance, stability = excluded.stability,
            access_count = excluded.access_count, first_accessed_at = excluded.first_accessed_at,
            last_accessed_at = excluded.last_accessed_at, valid_from = excluded.valid_from,
            invalidated_at = excluded.invalidated_at, superseded_by = excluded.superseded_by,
            dormant_at = excluded.dormant_at, last_verified_at = excluded.last_verified_at,
            graph_extracted_at = excluded.graph_extracted_at, basis = excluded.basis,
            occurred_from = excluded.occurred_from, occurred_to = excluded.occurred_to,
            pinned_at = COALESCE(excluded.pinned_at, memories.pinned_at)",
        params![
            m.id,
            m.content,
            m.source,
            m.project,
            m.created_at,
            m.reviewed as i64,
            blob,
            m.importance,
            m.stability,
            m.access_count,
            m.first_accessed_at,
            m.last_accessed_at,
            m.valid_from,
            m.invalidated_at,
            m.superseded_by,
            m.dormant_at,
            m.last_verified_at,
            m.graph_extracted_at,
            m.basis,
            m.occurred_from,
            m.occurred_to,
            m.pinned_at,
        ],
    )?;
    Ok(())
}

/// Insights counterpart of `raw_upsert_memory` — same explicit-id
/// upsert-at-the-caller's-discretion contract, for `mach kb import`.
pub fn raw_upsert_insight(conn: &Connection, i: &Insight) -> Result<(), KbError> {
    let blob = i.embedding.as_deref().map(encode_embedding);
    let source_ids_json =
        serde_json::to_string(&i.source_ids).map_err(|e| KbError::Other(format!("encoding source_ids: {}", e)))?;
    conn.execute(
        "INSERT INTO insights (id, text, created_at, confidence, source_ids, embedding, invalidated_at,
             flagged_at, last_verified_at, level, revised_at, prev_text)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
         ON CONFLICT(id) DO UPDATE SET
            text = excluded.text, created_at = excluded.created_at, confidence = excluded.confidence,
            source_ids = excluded.source_ids, embedding = excluded.embedding,
            invalidated_at = excluded.invalidated_at, flagged_at = excluded.flagged_at,
            last_verified_at = excluded.last_verified_at, level = excluded.level,
            revised_at = excluded.revised_at, prev_text = excluded.prev_text",
        params![
            i.id,
            i.text,
            i.created_at,
            i.confidence,
            source_ids_json,
            blob,
            i.invalidated_at,
            i.flagged_at,
            i.last_verified_at,
            i.level,
            i.revised_at,
            i.prev_text,
        ],
    )?;
    Ok(())
}

/// The "logical last-modified" instant for a memory row — the latest of
/// `created_at`, `last_accessed_at`, `invalidated_at`, `dormant_at` and
/// `last_verified_at` — used by `mach kb import --merge`'s last-write-wins
/// comparison. Every timestamp this store writes is a fixed-width RFC3339
/// string, so a plain lexicographic max is exact here, no parsing needed.
pub fn memory_last_modified(m: &Memory) -> &str {
    let mut latest = m.created_at.as_str();
    for candidate in [
        m.last_accessed_at.as_deref(),
        m.invalidated_at.as_deref(),
        m.dormant_at.as_deref(),
        m.last_verified_at.as_deref(),
        m.graph_extracted_at.as_deref(),
    ] {
        if let Some(c) = candidate {
            if c > latest {
                latest = c;
            }
        }
    }
    latest
}

/// Insights counterpart of `memory_last_modified` — latest of `created_at`,
/// `last_verified_at`, `flagged_at`, `invalidated_at`, `revised_at`.
pub fn insight_last_modified(i: &Insight) -> &str {
    let mut latest = i.created_at.as_str();
    for candidate in [
        i.last_verified_at.as_deref(),
        i.flagged_at.as_deref(),
        i.invalidated_at.as_deref(),
        i.revised_at.as_deref(),
    ] {
        if let Some(c) = candidate {
            if c > latest {
                latest = c;
            }
        }
    }
    latest
}

// --- engagement-gated reinforcement: `mach kb ingest-sessions` processed-set ---

/// Whether `session_id` has already been fully judged by `mach kb
/// ingest-sessions` — its engagement verdicts (if any) applied and its fact
/// digest (if any) extracted. A session is only ever marked via
/// `mark_session_ingested`, and only once every `claude` call it needed
/// actually succeeded — see that function's own doc comment for why a
/// degraded run must never land here.
pub fn is_session_ingested(conn: &Connection, session_id: &str) -> Result<bool, KbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ingested_sessions WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Records that `session_id` has been fully processed by `mach kb
/// ingest-sessions` — never called on a run where any needed `claude` call
/// failed (offline, spawn error, timeout), so a degraded pass stays fully
/// retry-able next time rather than being silently marked done. `INSERT OR
/// IGNORE` makes a second call for the same session (a `--session-id`
/// fast-trigger racing the opportunistic sweep, say) a harmless no-op rather
/// than an error.
pub fn mark_session_ingested(conn: &Connection, session_id: &str, now: &str) -> Result<(), KbError> {
    mark_session_ingested_with_usage(conn, session_id, now, None)
}

/// `mark_session_ingested` plus the session's skill-usage JSON (see
/// `ingest::skill_usage_from_transcript`), `None` when the transcript had
/// no `Skill` calls at all. Same `INSERT OR IGNORE` race posture.
pub fn mark_session_ingested_with_usage(
    conn: &Connection,
    session_id: &str,
    now: &str,
    skill_usage: Option<&str>,
) -> Result<(), KbError> {
    conn.execute(
        "INSERT OR IGNORE INTO ingested_sessions (session_id, ingested_at, skill_usage) VALUES (?1, ?2, ?3)",
        params![session_id, now, skill_usage],
    )?;
    Ok(())
}

/// `(ingested_at, skill_usage_json)` for every session ingested at or after
/// `since` that recorded any skill usage, oldest first -- the improve pass
/// sums these per skill.
pub fn skill_usage_since(conn: &Connection, since: &str) -> Result<Vec<(String, String)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT ingested_at, skill_usage FROM ingested_sessions
         WHERE skill_usage IS NOT NULL AND ingested_at >= ?1 ORDER BY ingested_at ASC",
    )?;
    let rows = stmt.query_map(params![since], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}


// --- session_progress: mid-session checkpoint digests ---

/// Raw transcript line count already digested for a session -- advanced by
/// every checkpoint (`mach kb ingest-sessions --partial`) and by the final
/// pass, and kept after the session is marked finished so a session that
/// keeps growing past its mark still gets its tail digested. 0 when nothing
/// has been digested yet.
pub fn session_progress(conn: &Connection, session_id: &str) -> Result<i64, KbError> {
    let v: Option<i64> = conn
        .query_row("SELECT last_line FROM session_progress WHERE session_id = ?1", params![session_id], |r| r.get(0))
        .optional()?;
    Ok(v.unwrap_or(0))
}

pub fn set_session_progress(conn: &Connection, session_id: &str, last_line: i64, now: &str) -> Result<(), KbError> {
    conn.execute(
        "INSERT INTO session_progress (session_id, last_line, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET last_line = excluded.last_line, updated_at = excluded.updated_at",
        params![session_id, last_line, now],
    )?;
    Ok(())
}

// --- memory_assoc: Hebbian co-activation between memories ---
//
// Memories injected into the same session and BOTH engaged with there
// "fired together": each such pair gets an undirected edge whose count
// grows on every co-engagement and whose effective weight decays with the
// same forgetting curve as everything else here. Search spreads activation
// one hop over these edges, so a memory that has repeatedly been useful
// alongside a hit surfaces even when its embedding is not near the query.
// Distinct from `relations` (entity graph extracted by an LLM from fact
// text): this graph is behavioral, learned only from what recall + the
// conversation actually did, never from what a fact says.

/// Days for an association's effective weight to fall to 1/e without
/// re-engagement.
pub const ASSOC_TAU_DAYS: f64 = 60.0;
/// Below this effective weight an edge is pruned by reflect.
pub const ASSOC_PRUNE_FLOOR: f32 = 0.15;

fn assoc_pair(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

/// Effective weight: `sqrt(count) * exp(-days_since_last / tau)` -- grows
/// sublinearly with repetition (a pair engaged 9 times is 3x, not 9x, a pair
/// engaged once), forgets on the ACT-R-shaped curve the rest of the store
/// uses.
pub fn assoc_weight(count: i64, last_at: &str, now: &str) -> f32 {
    let days = age_days(last_at, now).max(0.0);
    ((count.max(0) as f64).sqrt() * (-days / ASSOC_TAU_DAYS).exp()) as f32
}

/// Records one co-engagement across every pair in `ids` (order-free,
/// self-pairs skipped). Called by the ingest engagement pass with the ids
/// judged ENGAGED in one session.
pub fn reinforce_assoc(conn: &Connection, ids: &[i64], now: &str) -> Result<usize, KbError> {
    let mut uniq: Vec<i64> = ids.to_vec();
    uniq.sort_unstable();
    uniq.dedup();
    let mut n = 0;
    for i in 0..uniq.len() {
        for j in (i + 1)..uniq.len() {
            let (a, b) = assoc_pair(uniq[i], uniq[j]);
            conn.execute(
                "INSERT INTO memory_assoc (a, b, count, first_at, last_at) VALUES (?1, ?2, 1, ?3, ?3)
                 ON CONFLICT(a, b) DO UPDATE SET count = count + 1, last_at = excluded.last_at",
                params![a, b, now],
            )?;
            n += 1;
        }
    }
    Ok(n)
}

/// Active associates of `id` with their effective weight, strongest first.
/// Only edges whose other end is still an active memory are returned.
pub fn assoc_neighbors(conn: &Connection, id: i64, now: &str) -> Result<Vec<(i64, f32)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT CASE WHEN a = ?1 THEN b ELSE a END AS other, count, last_at
         FROM memory_assoc ma
         JOIN memories m ON m.id = CASE WHEN ma.a = ?1 THEN ma.b ELSE ma.a END
         WHERE (a = ?1 OR b = ?1) AND m.invalidated_at IS NULL AND m.dormant_at IS NULL",
    )?;
    let rows = stmt.query_map(params![id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)))?;
    let mut out = Vec::new();
    for r in rows {
        let (other, count, last_at) = r?;
        out.push((other, assoc_weight(count, &last_at, now)));
    }
    out.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

/// Deletes edges whose effective weight fell under `ASSOC_PRUNE_FLOOR` or
/// whose either end no longer exists. Returns how many went.
pub fn prune_assoc(conn: &Connection, now: &str) -> Result<usize, KbError> {
    let mut stmt = conn.prepare("SELECT a, b, count, last_at FROM memory_assoc")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
    })?;
    let mut doomed = Vec::new();
    for r in rows {
        let (a, b, count, last_at) = r?;
        let dead_end = get(conn, a)?.is_none() || get(conn, b)?.is_none();
        if dead_end || assoc_weight(count, &last_at, now) < ASSOC_PRUNE_FLOOR {
            doomed.push((a, b));
        }
    }
    for (a, b) in &doomed {
        conn.execute("DELETE FROM memory_assoc WHERE a = ?1 AND b = ?2", params![a, b])?;
    }
    Ok(doomed.len())
}

pub fn count_assoc(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM memory_assoc", [], |r| r.get(0))?)
}

// --- entities/relations: the graph layer ---
//
// Entities are not just people: a project, a technology, a game engine, a
// practice, a concept, an organization -- anything durable and nameable that
// `mach kb reflect`'s graph-extraction pass judged worth naming from a fact
// it already holds. `kind` is free text with a suggested vocabulary but
// never enforced. This augments the memory it came from, never replaces it
// -- every edge carries an `evidence_memory_id` pointing back at the fact it
// was extracted from.

/// One durable, nameable entity.
#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: i64,
    pub name: String,
    pub kind: Option<String>,
    pub embedding: Option<Vec<f32>>,
    pub created_at: String,
}

/// One directed edge between two entities -- "src predicate dst". Same
/// bi-temporal tombstone semantics as `Memory`/`Insight`: an invalidated
/// edge is never deleted, just excluded from active queries, and
/// `superseded_by` points at the edge that replaced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Relation {
    pub id: i64,
    pub src: i64,
    pub predicate: String,
    pub dst: i64,
    pub evidence_memory_id: Option<i64>,
    pub confidence: Option<f64>,
    pub created_at: String,
    pub valid_from: Option<String>,
    pub invalidated_at: Option<String>,
    pub superseded_by: Option<i64>,
}

impl Relation {
    pub fn is_active(&self) -> bool {
        self.invalidated_at.is_none()
    }
}

fn row_to_entity(row: &rusqlite::Row) -> rusqlite::Result<Entity> {
    let blob: Option<Vec<u8>> = row.get("embedding")?;
    Ok(Entity {
        id: row.get("id")?,
        name: row.get("name")?,
        kind: row.get("kind")?,
        embedding: blob.map(|b| decode_embedding(&b)),
        created_at: row.get("created_at")?,
    })
}

fn row_to_relation(row: &rusqlite::Row) -> rusqlite::Result<Relation> {
    Ok(Relation {
        id: row.get("id")?,
        src: row.get("src")?,
        predicate: row.get("predicate")?,
        dst: row.get("dst")?,
        evidence_memory_id: row.get("evidence_memory_id")?,
        confidence: row.get("confidence")?,
        created_at: row.get("created_at")?,
        valid_from: row.get("valid_from")?,
        invalidated_at: row.get("invalidated_at")?,
        superseded_by: row.get("superseded_by")?,
    })
}

/// Exact, case-insensitive name lookup -- the entity-resolution fast path.
/// Relies on `idx_entities_name_nocase`.
// --- memory <-> entity mentions (the substrate for graph recall and entity cards) ---

pub const MENTION_SOURCE_EXTRACTION: &str = "extraction";
pub const MENTION_SOURCE_NAME_SCAN: &str = "name-scan";
pub const MENTION_SOURCE_BACKFILL: &str = "backfill";

/// Case-insensitive word-boundary containment of `name` in `text`. Names
/// shorter than 3 characters and the reserved "user" never match (they
/// would link everything). Shared by the recall connections lookup, the
/// write-time mention scan, and the v16 backfill so all three agree on what
/// "mentions" means.
pub fn name_mentioned(text: &str, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    if name.chars().count() < 3 || name == "user" {
        return false;
    }
    let t = text.to_lowercase();
    let tb = t.as_bytes();
    let mut start = 0usize;
    while let Some(pos) = t[start..].find(&name) {
        let abs = start + pos;
        let end = abs + name.len();
        let left_ok = abs == 0 || !tb[abs - 1].is_ascii_alphanumeric();
        let right_ok = end == t.len() || !tb[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Records that memory `memory_id` mentions entity `entity_id`. Returns
/// `true` when the pair was new. Idempotent (`INSERT OR IGNORE`).
pub fn link_mention(conn: &Connection, memory_id: i64, entity_id: i64, source: &str, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO memory_entities (memory_id, entity_id, source, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![memory_id, entity_id, source, now],
    )?;
    Ok(n > 0)
}

/// Links `memory_id` to every entity whose name `content` mentions.
pub fn link_mentions_by_name_scan(conn: &Connection, memory_id: i64, content: &str, now: &str) -> Result<usize, KbError> {
    let mut n = 0;
    for e in all_entities(conn)? {
        if name_mentioned(content, &e.name) {
            n += link_mention(conn, memory_id, e.id, MENTION_SOURCE_NAME_SCAN, now)? as usize;
        }
    }
    Ok(n)
}

/// The reverse scan for a NEW entity: links every active memory whose text
/// mentions `name`. Called when graph extraction mints an entity, so older
/// memories that named it before it existed as a node are linked too.
pub fn link_entity_mentions_by_name_scan(conn: &Connection, entity_id: i64, name: &str, now: &str) -> Result<usize, KbError> {
    // Index-owned rows never join the mention graph (see
    // `upsert_index_memory`).
    let sql = format!(
        "SELECT id, content FROM memories WHERE invalidated_at IS NULL AND {}",
        not_index_owned_sql("source"),
    );
    let mut stmt = conn.prepare(&sql)?;
    let mems: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut n = 0;
    for (mid, content) in &mems {
        if name_mentioned(content, name) {
            n += link_mention(conn, *mid, entity_id, MENTION_SOURCE_NAME_SCAN, now)? as usize;
        }
    }
    Ok(n)
}

pub fn entities_of_memory(conn: &Connection, memory_id: i64) -> Result<Vec<Entity>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT e.* FROM entities e JOIN memory_entities me ON me.entity_id = e.id WHERE me.memory_id = ?1 ORDER BY e.name",
    )?;
    let rows = stmt.query_map(params![memory_id], row_to_entity)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Active (not superseded, not dormant) memories mentioning `entity_id`,
/// newest first, optionally capped.
pub fn memories_mentioning(conn: &Connection, entity_id: i64, limit: Option<usize>) -> Result<Vec<Memory>, KbError> {
    let sql = format!(
        "SELECT m.* FROM memories m JOIN memory_entities me ON me.memory_id = m.id
         WHERE me.entity_id = ?1 AND m.invalidated_at IS NULL AND m.dormant_at IS NULL
         ORDER BY m.id DESC{}",
        limit.map(|n| format!(" LIMIT {}", n)).unwrap_or_default()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![entity_id], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// How many ACTIVE memories mention `entity_id`.
pub fn mention_degree(conn: &Connection, entity_id: i64) -> Result<i64, KbError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memory_entities me JOIN memories m ON m.id = me.memory_id
         WHERE me.entity_id = ?1 AND m.invalidated_at IS NULL AND m.dormant_at IS NULL",
        params![entity_id],
        |r| r.get(0),
    )?)
}

pub fn count_active_memories(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE invalidated_at IS NULL AND dormant_at IS NULL",
        [],
        |r| r.get(0),
    )?)
}

/// Moves every mention off `old_id` onto `new_id` (entity merge), deduping.
pub fn repoint_entity_mentions(conn: &Connection, old_id: i64, new_id: i64) -> Result<usize, KbError> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO memory_entities (memory_id, entity_id, source, created_at)
         SELECT memory_id, ?1, source, created_at FROM memory_entities WHERE entity_id = ?2",
        params![new_id, old_id],
    )?;
    conn.execute("DELETE FROM memory_entities WHERE entity_id = ?1", params![old_id])?;
    Ok(n)
}

// --- entity cards: a consolidated profile per entity, any kind ---

/// A distilled profile of one entity, rebuilt by `mach kb reflect` from the
/// memories that mention it. The generic form of Honcho's peer card and
/// Hindsight's observation: the same mechanism serves a person ("how Moses
/// argues"), a project ("what Umoja is and where it stands"), a practice
/// ("what audit-before-build means here"), or a tool. Replaced wholesale on
/// rebuild rather than accumulated, so it never drifts out of step with its
/// evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityCard {
    pub entity_id: i64,
    pub text: String,
    /// Memory ids the card was distilled from, newest-first at build time.
    pub source_ids: Vec<String>,
    pub embedding: Option<Vec<f32>>,
    pub created_at: String,
    pub updated_at: String,
    /// Highest mentioning-memory id seen at build time: a newer mention means
    /// the card is behind its evidence and due for a rebuild.
    pub built_watermark: i64,
    /// Active mention count at build time: a DROP (dormancy, supersession)
    /// also makes the card stale even with no new mention.
    pub mention_count: i64,
}

fn row_to_entity_card(row: &rusqlite::Row) -> rusqlite::Result<EntityCard> {
    let ids: String = row.get("source_ids")?;
    let blob: Option<Vec<u8>> = row.get("embedding")?;
    Ok(EntityCard {
        entity_id: row.get("entity_id")?,
        text: row.get("text")?,
        source_ids: serde_json::from_str(&ids).unwrap_or_default(),
        embedding: blob.map(|b| decode_embedding(&b)),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        built_watermark: row.get("built_watermark")?,
        mention_count: row.get("mention_count")?,
    })
}

pub fn get_entity_card(conn: &Connection, entity_id: i64) -> Result<Option<EntityCard>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM entity_cards WHERE entity_id = ?1")?;
    Ok(stmt.query_row(params![entity_id], row_to_entity_card).optional()?)
}

/// Writes (or replaces) an entity's card. `created_at` is preserved across
/// rebuilds so a card's age reflects when it was first formed.
pub fn upsert_entity_card(
    conn: &Connection,
    entity_id: i64,
    text: &str,
    source_ids: &[String],
    embedding: Option<&[f32]>,
    watermark: i64,
    mention_count: i64,
    now: &str,
) -> Result<(), KbError> {
    let ids = serde_json::to_string(source_ids).unwrap_or_else(|_| "[]".to_string());
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO entity_cards (entity_id, text, source_ids, embedding, created_at, updated_at, built_watermark, mention_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)
         ON CONFLICT(entity_id) DO UPDATE SET
            text = excluded.text, source_ids = excluded.source_ids, embedding = excluded.embedding,
            updated_at = excluded.updated_at, built_watermark = excluded.built_watermark,
            mention_count = excluded.mention_count",
        params![entity_id, text, ids, blob, now, watermark, mention_count],
    )?;
    Ok(())
}

pub fn delete_entity_card(conn: &Connection, entity_id: i64) -> Result<bool, KbError> {
    Ok(conn.execute("DELETE FROM entity_cards WHERE entity_id = ?1", params![entity_id])? > 0)
}

pub fn count_mentions(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM memory_entities", [], |r| r.get(0))?)
}

pub fn count_entity_cards(conn: &Connection) -> Result<i64, KbError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM entity_cards", [], |r| r.get(0))?)
}

/// Highest ACTIVE mentioning-memory id for `entity_id` (0 when none) --
/// the card staleness watermark.
pub fn max_mention_memory_id(conn: &Connection, entity_id: i64) -> Result<i64, KbError> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(me.memory_id), 0) FROM memory_entities me JOIN memories m ON m.id = me.memory_id
         WHERE me.entity_id = ?1 AND m.invalidated_at IS NULL AND m.dormant_at IS NULL",
        params![entity_id],
        |r| r.get(0),
    )?)
}

/// An entity needs at least this many ACTIVE mentioning memories before a
/// card is worth building: below it, the memories themselves are a better
/// answer than a summary of them.
pub const CARD_MIN_MENTIONS: i64 = 4;
/// Memories handed to the card prompt, newest first.
pub const CARD_EVIDENCE_LIMIT: usize = 14;
/// Entities re-carded per reflect run, so one run is bounded.
pub const CARD_MAX_PER_RUN: usize = 6;

/// How novel a memory is against what the bank already holds: `1 - max
/// cosine to any other active memory`. A restatement of something already
/// known scores near 0; a genuinely new fact scores high.
///
/// Used to ORDER what reflection looks at first (Honcho's Dreamer
/// prioritizes by surprisal for the same reason): passes are capped per
/// run, so the cap decides what gets examined, and "oldest unexamined" is a
/// worse basis for that choice than "least like anything we already know".
/// A fact that restates the bank teaches reflection nothing.
pub fn surprisal(conn: &Connection, m: &Memory) -> Result<f32, KbError> {
    let Some(emb) = m.embedding.as_deref().filter(|e| !e.is_empty()) else {
        return Ok(0.5); // unknown novelty: neither promoted nor buried
    };
    let mut best = 0.0f32;
    for other in candidates(conn, true, false)? {
        if other.id == m.id {
            continue;
        }
        if let Some(o) = other.embedding.as_deref().filter(|e| !e.is_empty()) {
            best = best.max(cosine(emb, o));
        }
    }
    Ok((1.0 - normalize_sim(best)).clamp(0.0, 1.0))
}

/// Reorders `rows` most-surprising-first. Fails soft: any row whose
/// surprisal cannot be computed keeps the neutral 0.5 and stays mid-pack,
/// so this never drops work from a pass, only reorders it.
pub fn order_by_surprisal(conn: &Connection, rows: Vec<Memory>) -> Vec<Memory> {
    let mut scored: Vec<(f32, Memory)> =
        rows.into_iter().map(|m| (surprisal(conn, &m).unwrap_or(0.5), m)).collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.id.cmp(&b.1.id)));
    scored.into_iter().map(|(_, m)| m).collect()
}

/// Entities due for a card: at least `CARD_MIN_MENTIONS` active mentions and
/// either no card yet, or a card built before the newest mention, or a card
/// whose mention count no longer matches (evidence died under it). Ordered
/// by "most evidence the card has not seen yet" first, then by degree, so a
/// busy entity is refreshed before a quiet one. Never returns the reserved
/// "user" entity: everything in the bank is about the user, so its card
/// would be the whole bank -- that job belongs to `mach kb model`.
pub fn entity_card_candidates(conn: &Connection, cap: usize) -> Result<Vec<Entity>, KbError> {
    // The per-entity aggregates are correlated subqueries, so the staleness
    // test lives in an outer WHERE (a bare HAVING with no GROUP BY would
    // collapse everything into one aggregate row).
    let mut stmt = conn.prepare(
        "SELECT * FROM (
            SELECT e.*,
                (SELECT COUNT(*) FROM memory_entities me JOIN memories m ON m.id = me.memory_id
                  WHERE me.entity_id = e.id AND m.invalidated_at IS NULL AND m.dormant_at IS NULL) AS deg,
                (SELECT COALESCE(MAX(me.memory_id), 0) FROM memory_entities me JOIN memories m ON m.id = me.memory_id
                  WHERE me.entity_id = e.id AND m.invalidated_at IS NULL AND m.dormant_at IS NULL) AS hi,
                c.built_watermark AS wm, c.mention_count AS mc
            FROM entities e LEFT JOIN entity_cards c ON c.entity_id = e.id
            WHERE e.name <> 'user' COLLATE NOCASE
         )
         WHERE deg >= ?1 AND (wm IS NULL OR hi > wm OR mc <> deg)
         ORDER BY (hi - COALESCE(wm, 0)) DESC, deg DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![CARD_MIN_MENTIONS, cap as i64], row_to_entity)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// --- spreading activation over the memory graph (Hindsight-style recall hop) ---

/// Steps of breadth-first propagation from the seeds. Two: "a memory that
/// shares an entity with a memory that shares an entity with a hit" is as
/// far as relevance survives the decay below.
pub const SPREAD_STEPS: usize = 2;
/// Activation lost per hop. A perfect step-1 neighbor of a seed scored `s`
/// lands at `s * SPREAD_DECAY * mu` -- always below the seed with the
/// multipliers here, so a hop never outranks the hit that led to it.
pub const SPREAD_DECAY: f32 = 0.6;
/// Link-type multipliers (Hindsight's mu(l)): shared entity is the strongest
/// evidence of relatedness, Hebbian co-engagement next, time proximity the
/// weakest (a busy day links unrelated things).
pub const SPREAD_MU_ENTITY: f32 = 1.2;
pub const SPREAD_MU_ASSOC: f32 = 1.0;
// Deliberately NO temporal edge. Hindsight links memories by when the
// described events OCCURRED, which it extracts per fact; all we have is
// `created_at`, i.e. when the fact was written down. One session digest
// writes up to 25 unrelated facts within the same second, so a
// proximity-in-created_at edge would link everything ingested together at
// full weight -- noise, not relatedness. Time belongs in the query channel
// (filter by a date range the query asks for), not in the graph.
/// An entity mentioned by more than this share of active memories (or more
/// than the absolute cap) is a hub ("umoja", "Claude") and carries no
/// relatedness signal; it is skipped as an edge. Every other shared entity
/// is a full-weight link (as in Hindsight, where entity edges carry w = 1.0
/// and relevance is decided elsewhere): what varies the weight here is the
/// entity's affinity to the query, not its rarity. An earlier IDF-style
/// `1 / ln(1 + degree)` term was removed because it fought the affinity
/// term -- the entity a query is ABOUT is usually one of the more common
/// ones, so the two together cancelled out and no hop cleared the recall
/// score floor.
pub const SPREAD_HUB_SHARE: f64 = 0.15;
pub const SPREAD_HUB_ABS: i64 = 40;
/// The share rule only applies from this degree up. In a small bank almost
/// any entity exceeds a 15% share (2 of 12 memories is 17%) without being a
/// hub in any useful sense; below this, only the absolute cap can disqualify
/// an entity.
pub const SPREAD_HUB_MIN_DEGREE: i64 = 8;
/// Per-node fan-out cap per link type, and the overall cap on returned hops.
pub const SPREAD_FANOUT: usize = 12;
pub const SPREAD_MAX_OUT: usize = 3;
/// At most this many hops may arrive over the SAME link. Every memory
/// sharing one entity with a seed gets identical activation, so without
/// this one well-connected entity fills the whole hop budget with near
/// duplicates; one hop each from three different links says far more per
/// token than three from one.
pub const SPREAD_MAX_PER_EDGE: usize = 1;
/// Query affinity assumed for an entity with no name embedding: neutral,
/// neither favoured nor suppressed.
pub const SPREAD_AFFINITY_DEFAULT: f32 = 0.5;
/// A shared entity is only walked when its own name embedding is at least
/// this fraction as close to the query as the closest shared entity of that
/// node. Two memories often share several entities, and only some are what
/// the query is ABOUT: "deploy zenith to popobawa" shares "orchestrator"
/// (the subject) and "systemd" (incidental) with its neighbours, and
/// without this the incidental one hops just as readily. Relative, not an
/// absolute cosine floor, because a prose query is never as close to a
/// short entity name as another name would be -- the calibration has to
/// come from the candidates themselves. Set to admit any shared entity in
/// the same relevance league as the best one, not only the single best: the
/// entity that finally carries a hop is usually a rare, specific one
/// (rareness is what earns weight in `entity_edge_weight`), and a floor
/// tight enough to keep only the top entity dropped those too, leaving no
/// hops at all above the recall score floor.
pub const SPREAD_AFFINITY_REL_FLOOR: f32 = 0.6;

/// One memory reached by spreading activation.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphHit {
    pub id: i64,
    pub activation: f32,
    /// The seed hit this activation traces back to.
    pub via_seed: i64,
    /// The edge that first reached it: "entity:<name>" or "assoc".
    pub via_edge: String,
}

fn entity_edge_weight(degree: i64, total_active: i64) -> Option<f32> {
    if degree <= 0 {
        return None;
    }
    let share_hub =
        degree >= SPREAD_HUB_MIN_DEGREE && total_active > 0 && degree as f64 > SPREAD_HUB_SHARE * total_active as f64;
    if degree > SPREAD_HUB_ABS || share_hub {
        return None; // hub
    }
    Some(1.0)
}

/// Neighbors of `id` over shared entities: `(other_id, weight, entity_name)`,
/// hubs excluded, query-affinity weighted (`SPREAD_AFFINITY_REL_FLOOR`),
/// best weight per neighbor, capped at `SPREAD_FANOUT`.
fn entity_neighbors(
    conn: &Connection,
    id: i64,
    total_active: i64,
    q_emb: Option<&[f32]>,
) -> Result<Vec<(i64, f32, String)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT e.id, e.name, e.embedding,
                (SELECT COUNT(*) FROM memory_entities x JOIN memories mm ON mm.id = x.memory_id
                  WHERE x.entity_id = e.id AND mm.invalidated_at IS NULL AND mm.dormant_at IS NULL) AS deg
         FROM memory_entities me JOIN entities e ON e.id = me.entity_id WHERE me.memory_id = ?1",
    )?;
    let raw: Vec<(i64, String, Option<Vec<u8>>, i64)> = stmt
        .query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;

    // Query affinity per shared entity, and the node's best, so the floor
    // below is relative to what this node actually offers.
    let mut ents: Vec<(i64, String, i64, f32)> = Vec::new();
    for (eid, name, blob, deg) in raw {
        let affinity = match (q_emb, blob) {
            (Some(q), Some(b)) => {
                let v = decode_embedding(&b);
                if v.is_empty() {
                    SPREAD_AFFINITY_DEFAULT
                } else {
                    cosine(q, &v).clamp(0.0, 1.0)
                }
            }
            _ => SPREAD_AFFINITY_DEFAULT,
        };
        ents.push((eid, name, deg, affinity));
    }
    let best_affinity = ents.iter().map(|(_, _, _, a)| *a).fold(0.0f32, f32::max);

    let mut best: HashMap<i64, (f32, String)> = HashMap::new();
    for (eid, name, deg, affinity) in ents {
        let rel = if best_affinity > 0.0 { affinity / best_affinity } else { 1.0 };
        if rel < SPREAD_AFFINITY_REL_FLOOR {
            continue; // shared, but not what the query is about
        }
        let Some(base) = entity_edge_weight(deg, total_active) else { continue };
        let w = base * rel;
        let mut stmt = conn.prepare(
            "SELECT me.memory_id FROM memory_entities me JOIN memories m ON m.id = me.memory_id
             WHERE me.entity_id = ?1 AND me.memory_id != ?2 AND m.invalidated_at IS NULL AND m.dormant_at IS NULL
             ORDER BY m.id DESC LIMIT ?3",
        )?;
        let others: Vec<i64> =
            stmt.query_map(params![eid, id, SPREAD_FANOUT as i64], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for o in others {
            let e = best.entry(o).or_insert((0.0, String::new()));
            if w > e.0 {
                *e = (w, name.clone());
            }
        }
    }
    let mut out: Vec<(i64, f32, String)> = best.into_iter().map(|(k, (w, n))| (k, w, n)).collect();
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(SPREAD_FANOUT);
    Ok(out)
}

/// Spreading activation from `seeds` (`(memory_id, score)`) over two link
/// types -- shared entity and Hebbian co-engagement -- for `SPREAD_STEPS`
/// steps with `A(j) = max(A(i) * w * SPREAD_DECAY * mu)`.
/// Returns up to `SPREAD_MAX_OUT` memories that are neither seeds nor in
/// `exclude`, best activation first, each tagged with the seed it traces to
/// and the edge that first reached it. `q_emb` (the query embedding, when
/// the caller has one) steers which shared entity is worth walking; without
/// it every shared entity is treated as equally relevant. Pure read.
pub fn spread_activation(
    conn: &Connection,
    seeds: &[(i64, f32)],
    exclude: &std::collections::HashSet<i64>,
    now: &str,
    q_emb: Option<&[f32]>,
) -> Result<Vec<GraphHit>, KbError> {
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let total_active = count_active_memories(conn)?;
    let seed_ids: std::collections::HashSet<i64> = seeds.iter().map(|(id, _)| *id).collect();
    // id -> (activation, via_seed, via_edge)
    let mut act: HashMap<i64, (f32, i64, String)> = HashMap::new();
    let mut frontier: Vec<(i64, f32, i64)> = seeds.iter().map(|(id, s)| (*id, *s, *id)).collect();
    for _ in 0..SPREAD_STEPS {
        let mut next: Vec<(i64, f32, i64)> = Vec::new();
        for (node, a, seed) in &frontier {
            let mut relax = |other: i64, w: f32, mu: f32, edge: String, next: &mut Vec<(i64, f32, i64)>| {
                if seed_ids.contains(&other) || exclude.contains(&other) {
                    return;
                }
                let na = a * w * SPREAD_DECAY * mu;
                if na <= 0.0 {
                    return;
                }
                let better = act.get(&other).map(|(cur, _, _)| na > *cur).unwrap_or(true);
                if better {
                    act.insert(other, (na, *seed, edge));
                    next.push((other, na, *seed));
                }
            };
            for (o, w, name) in entity_neighbors(conn, *node, total_active, q_emb)? {
                relax(o, w, SPREAD_MU_ENTITY, format!("entity:{}", name), &mut next);
            }
            for (o, w) in assoc_neighbors(conn, *node, now)? {
                relax(o, 1.0 - (-w).exp(), SPREAD_MU_ASSOC, "assoc".to_string(), &mut next);
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    let mut ranked: Vec<GraphHit> = act
        .into_iter()
        .map(|(id, (activation, via_seed, via_edge))| GraphHit { id, activation, via_seed, via_edge })
        .collect();
    // Strongest first, then a stable tiebreak: equal-activation hops (the
    // common case -- one entity, many neighbours) resolve to the newest
    // memory rather than to whatever order the map happened to yield.
    ranked.sort_by(|a, b| {
        b.activation.partial_cmp(&a.activation).unwrap_or(std::cmp::Ordering::Equal).then(b.id.cmp(&a.id))
    });
    let mut per_edge: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<GraphHit> = Vec::new();
    for hit in ranked {
        if out.len() >= SPREAD_MAX_OUT {
            break;
        }
        let n = per_edge.entry(hit.via_edge.clone()).or_insert(0);
        if *n >= SPREAD_MAX_PER_EDGE {
            continue;
        }
        *n += 1;
        out.push(hit);
    }
    Ok(out)
}

pub fn find_entity_by_name(conn: &Connection, name: &str) -> Result<Option<Entity>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM entities WHERE name = ?1 COLLATE NOCASE")?;
    Ok(stmt.query_row(params![name], row_to_entity).optional()?)
}

pub fn get_entity(conn: &Connection, id: i64) -> Result<Option<Entity>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM entities WHERE id = ?1")?;
    Ok(stmt.query_row(params![id], row_to_entity).optional()?)
}

/// Every entity, unordered -- the pool `find_entity_by_similarity` scans.
/// Personal-scale (comfortably under a few thousand entities), so a linear
/// scan mirrors `top_similar_active`'s own brute-force cosine approach
/// rather than adding a vector index for this size of corpus.
pub fn all_entities(conn: &Connection) -> Result<Vec<Entity>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM entities")?;
    let rows = stmt.query_map([], row_to_entity)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Best entity match by name-embedding cosine similarity, if any clears
/// `min_sim` -- shared by entity resolution during extraction (its own,
/// higher, reuse threshold) and by search enrichment (its own, lower,
/// threshold for surfacing connections), each supplying its own floor.
pub fn find_entity_by_similarity(
    conn: &Connection,
    query_embedding: &[f32],
    min_sim: f32,
) -> Result<Option<(Entity, f32)>, KbError> {
    let mut best: Option<(Entity, f32)> = None;
    for e in all_entities(conn)? {
        let sim = match &e.embedding {
            Some(v) if !v.is_empty() => cosine(query_embedding, v),
            _ => continue,
        };
        if sim >= min_sim && best.as_ref().map(|(_, b)| sim > *b).unwrap_or(true) {
            best = Some((e, sim));
        }
    }
    Ok(best)
}

/// Inserts a new entity and returns its id. Name collision (case-
/// insensitive, via `idx_entities_name_nocase`) is the caller's job to rule
/// out first (`find_entity_by_name`) -- this always inserts, and a caller
/// that skipped that check would simply surface the unique-index violation
/// as a `KbError::Db`.
pub fn insert_entity(
    conn: &Connection,
    name: &str,
    kind: Option<&str>,
    embedding: Option<&[f32]>,
) -> Result<i64, KbError> {
    let created_at = now_rfc3339();
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO entities (name, kind, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![name, kind, blob, created_at],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Entity counts grouped by `kind` (NULL/blank folded into `"unspecified"`),
/// most populous first -- `mach kb graph --stats`.
pub fn entity_counts_by_kind(conn: &Connection) -> Result<Vec<(String, i64)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(TRIM(kind), ''), 'unspecified') AS k, COUNT(*) AS n
         FROM entities GROUP BY k ORDER BY n DESC, k ASC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn entity_count(conn: &Connection) -> Result<i64, KbError> {
    conn.query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0)).map_err(Into::into)
}

pub fn active_relation_count(conn: &Connection) -> Result<i64, KbError> {
    conn.query_row("SELECT COUNT(*) FROM relations WHERE invalidated_at IS NULL", [], |r| r.get(0)).map_err(Into::into)
}

pub fn invalidated_relation_count(conn: &Connection) -> Result<i64, KbError> {
    conn.query_row("SELECT COUNT(*) FROM relations WHERE invalidated_at IS NOT NULL", [], |r| r.get(0))
        .map_err(Into::into)
}

/// Inserts a new active edge (`src` --`predicate`--> `dst`) and returns its
/// id. Pure insert -- conflict detection/resolution against existing active
/// edges is the caller's own job (`cli::insert_edge_with_conflict_check`),
/// the same separation `supersede` keeps from the memory-level contradiction
/// patrol's verdict logic.
pub fn insert_relation(
    conn: &Connection,
    src: i64,
    predicate: &str,
    dst: i64,
    evidence_memory_id: Option<i64>,
    confidence: Option<f64>,
    now: &str,
) -> Result<i64, KbError> {
    conn.execute(
        "INSERT INTO relations (src, predicate, dst, evidence_memory_id, confidence, created_at, valid_from)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![src, predicate, dst, evidence_memory_id, confidence, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn get_relation(conn: &Connection, id: i64) -> Result<Option<Relation>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM relations WHERE id = ?1")?;
    Ok(stmt.query_row(params![id], row_to_relation).optional()?)
}

/// Tombstones `old_id` in favor of `new_id` -- the edge-graph counterpart of
/// `supersede`. Never deletes; the row stays as an audit trail. Returns
/// `false` (no-op) if `old_id` doesn't exist or is already tombstoned.
pub fn supersede_relation(conn: &Connection, old_id: i64, new_id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE relations SET invalidated_at = ?1, superseded_by = ?2 WHERE id = ?3 AND invalidated_at IS NULL",
        params![now, new_id, old_id],
    )?;
    Ok(n > 0)
}

/// Active edges touching `entity_id` in either direction, newest first --
/// `mach kb entity <name>` and the search-enrichment "connections" field.
pub fn active_relations_for_entity(conn: &Connection, entity_id: i64) -> Result<Vec<Relation>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM relations WHERE (src = ?1 OR dst = ?1) AND invalidated_at IS NULL ORDER BY id DESC",
    )?;
    let rows = stmt.query_map(params![entity_id], row_to_relation)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Count of invalidated (tombstoned) edges touching `entity_id` in either
/// direction -- `mach kb entity <name>`'s "N invalidated" footnote.
pub fn invalidated_relation_count_for_entity(conn: &Connection, entity_id: i64) -> Result<i64, KbError> {
    conn.query_row(
        "SELECT COUNT(*) FROM relations WHERE (src = ?1 OR dst = ?1) AND invalidated_at IS NOT NULL",
        params![entity_id],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

/// ACTIVE edges that plausibly conflict with a candidate `(src, predicate,
/// dst)` edge before it's inserted: same predicate (case-insensitive exact
/// match -- extraction produces short, hyphenated, normalized predicates,
/// making this a reliable enough proxy for "the same kind of relationship"
/// without an extra embedding call per edge) sharing `src` or `dst` with the
/// candidate, excluding the literal same edge. The boss test is exactly
/// this shape: "Moses boss-of user" and "Ivar boss-of user" share
/// `predicate` ("boss-of") and `dst` (user) while `src` differs.
pub fn relations_conflicting_with(conn: &Connection, src: i64, predicate: &str, dst: i64) -> Result<Vec<Relation>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT * FROM relations
         WHERE invalidated_at IS NULL
           AND predicate = ?1 COLLATE NOCASE
           AND (src = ?2 OR dst = ?3)
           AND NOT (src = ?2 AND dst = ?3)
         ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(params![predicate, src, dst], row_to_relation)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

// --- graph extraction pass (mach kb reflect) ---

/// Up to `cap` ACTIVE memories (not invalidated, not dormant) never yet
/// offered to the graph-extraction judge (`graph_extracted_at IS NULL`),
/// oldest first -- same "drains a backlog in stable order across runs"
/// rationale as `curation_candidates`.
pub fn graph_extraction_candidates(conn: &Connection, cap: usize) -> Result<Vec<Memory>, KbError> {
    let sql = format!(
        "SELECT * FROM memories WHERE graph_extracted_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL \
           AND {} \
         ORDER BY id ASC LIMIT {}",
        not_index_owned_sql("source"),
        cap
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Cheap existence check for the early-exit guard at the top of `mach kb
/// reflect`: whether there is at least one row `graph_extraction_candidates`
/// would return, without materializing any of them -- so a database with no
/// due insight re-verification and nothing newly added still runs the
/// graph-extraction pass when it has an unprocessed backlog (e.g. this
/// feature's own one-time backfill).
pub fn has_graph_extraction_candidates(conn: &Connection) -> Result<bool, KbError> {
    let sql = format!(
        "SELECT EXISTS(SELECT 1 FROM memories WHERE graph_extracted_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL \
           AND {})",
        not_index_owned_sql("source"),
    );
    let n: i64 = conn.query_row(&sql, [], |r| r.get(0))?;
    Ok(n != 0)
}

/// Marks a memory as offered to the graph-extraction judge -- call only
/// after a successful LLM call (see `Memory::graph_extracted_at`'s own doc
/// comment for why a failed call must never call this).
pub fn mark_graph_extracted(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE memories SET graph_extracted_at = ?1 WHERE id = ?2", params![now, id])?;
    Ok(n > 0)
}

// --- graph hygiene pass (mach kb reflect, after edge extraction, before
// dormancy): evidence-death propagation + entity merge ---

/// Invalidates a relation on its own, `superseded_by` left NULL -- for a
/// death this graph never lost a competition over: either its own evidence
/// memory died (evidence-death propagation, below) or a one-time audit
/// judged it poisoned/generic (`mach kb graph audit`). Contrast
/// `supersede_relation`, which always names a winner. Never deletes; the
/// row stays as an audit trail like every other tombstone in this store.
/// Returns `false` (no-op) if `id` doesn't exist or is already invalidated.
pub fn invalidate_relation(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE relations SET invalidated_at = ?1 WHERE id = ?2 AND invalidated_at IS NULL",
        params![now, id],
    )?;
    Ok(n > 0)
}

/// ACTIVE edges whose evidence memory has died -- invalidated (superseded,
/// contradicted, deduped away) or hard-deleted (`mach kb forget`) -- the
/// graph hygiene pass's deterministic, no-LLM "evidence-death propagation"
/// candidate pool. Deliberately excludes an edge whose evidence is merely
/// *dormant*: a dormant fact can wake (`mach kb wake`), so its edges must
/// not die with it -- see `relation_evidence_is_dormant` for how that case
/// is instead respected only at query time.
pub fn relations_with_dead_evidence(conn: &Connection) -> Result<Vec<Relation>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT r.* FROM relations r
         LEFT JOIN memories m ON m.id = r.evidence_memory_id
         WHERE r.invalidated_at IS NULL
           AND r.evidence_memory_id IS NOT NULL
           AND (m.id IS NULL OR m.invalidated_at IS NOT NULL)
         ORDER BY r.id ASC",
    )?;
    let rows = stmt.query_map([], row_to_relation)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Whether a relation's own evidence memory is currently dormant -- `true`
/// only when `evidence_memory_id` is `Some` and that memory still exists and
/// is dormant (a memory that's simply gone is `relations_with_dead_evidence`'s
/// concern, not this one; a relation with no evidence at all is never
/// dormant by definition). Called at query time (never inside the reflect
/// pass, which must never tombstone an edge merely because its evidence
/// napped) by every path that surfaces "connections" for recall, so a
/// dormant-but-wakeable fact's edge is excluded from that view without ever
/// being invalidated.
pub fn relation_evidence_is_dormant(conn: &Connection, r: &Relation) -> Result<bool, KbError> {
    match r.evidence_memory_id {
        Some(mid) => Ok(matches!(get(conn, mid)?, Some(m) if m.is_dormant())),
        None => Ok(false),
    }
}

/// Normalizes an entity name for merge-candidate matching: lowercased,
/// stripped of everything but letters/digits -- so "C++", "c++", and "C ++"
/// collapse to the same key, as do "Umoja" and "umoja." — a
/// case/punctuation-insensitive match the embedding-similarity path (below)
/// might otherwise miss on a very short name.
fn normalize_entity_name(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Candidate entity-merge pairs for the graph hygiene pass: active entity
/// pairs `(a, b)` with `a < b` where EITHER their name embeddings clear
/// `min_sim` cosine similarity OR their normalized (case/punctuation-
/// insensitive) names are identical, excluding any pair already in `seen`
/// (`entity_merge_seen_pairs`). Personal-scale linear scan, same rationale
/// as `find_entity_by_similarity`. Deduped and capped at `cap`, ascending by
/// the pair's own `(a, b)` ordering so a backlog drains in a stable order
/// across runs, same as `dedupe_candidate_pairs`.
pub fn entity_merge_candidate_pairs(
    conn: &Connection,
    min_sim: f32,
    seen: &std::collections::HashSet<(i64, i64)>,
    cap: usize,
) -> Result<Vec<(i64, i64)>, KbError> {
    let entities = all_entities(conn)?;
    let mut out: Vec<(i64, i64)> = Vec::new();
    for i in 0..entities.len() {
        for j in (i + 1)..entities.len() {
            let (a, b) = (&entities[i], &entities[j]);
            let (id_a, id_b) = if a.id < b.id { (a.id, b.id) } else { (b.id, a.id) };
            if seen.contains(&(id_a, id_b)) {
                continue;
            }
            let name_match = normalize_entity_name(&a.name) == normalize_entity_name(&b.name);
            let sim_match = match (&a.embedding, &b.embedding) {
                (Some(ea), Some(eb)) if !ea.is_empty() && !eb.is_empty() => cosine(ea, eb) >= min_sim,
                _ => false,
            };
            if name_match || sim_match {
                out.push((id_a, id_b));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out.truncate(cap);
    Ok(out)
}

/// Up to `n` active edges touching `entity_id`, rendered as plain
/// `"src --predicate--> dst"` strings by entity NAME (never an id) -- the
/// entity-merge judge prompt's "2 sample edges" description, id-free per
/// this feature's own house rule.
pub fn sample_relation_descriptions(conn: &Connection, entity_id: i64, n: usize) -> Result<Vec<String>, KbError> {
    let edges = active_relations_for_entity(conn, entity_id)?;
    let mut out = Vec::new();
    for r in edges.iter().take(n) {
        let src = get_entity(conn, r.src)?;
        let dst = get_entity(conn, r.dst)?;
        if let (Some(s), Some(d)) = (src, dst) {
            out.push(format!("{} --{}--> {}", s.name, r.predicate, d.name));
        }
    }
    Ok(out)
}

/// Records that an entity-merge candidate pair was judged genuinely
/// `DIFFERENT` -- never asked about again on a future reflect run. Same
/// "never watermark a failed or malformed judgment" contract as
/// `mark_dedupe_seen`: a `SAME` verdict needs no entry here either, since
/// the merged (newer) entity is deleted outright and can never resurface as
/// a candidate.
pub fn mark_entity_merge_seen(conn: &Connection, id_a: i64, id_b: i64) -> Result<(), KbError> {
    let (a, b) = normalize_pair(id_a, id_b);
    conn.execute("INSERT OR IGNORE INTO entity_merge_seen (id_a, id_b) VALUES (?1, ?2)", params![a, b])?;
    Ok(())
}

/// Every pair ever recorded by `mark_entity_merge_seen`, as normalized
/// `(min, max)` tuples -- loaded once per reflect run, same shape as
/// `dedupe_seen_pairs`.
pub fn entity_merge_seen_pairs(conn: &Connection) -> Result<std::collections::HashSet<(i64, i64)>, KbError> {
    let mut stmt = conn.prepare("SELECT id_a, id_b FROM entity_merge_seen")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let mut out = std::collections::HashSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Repoints every relation's `src`/`dst` reference from `old_id` to
/// `new_id` (both directions, across every relation regardless of active/
/// invalidated status -- a provenance fix, not a currency judgment, mirrors
/// `repoint_insight_citations`' own scope), then deduplicates: when the
/// repoint makes two ACTIVE relations identical (`src`, `predicate`
/// case-insensitive, `dst`), only the oldest (lowest id) survives active --
/// any newer duplicate is invalidated via `invalidate_relation`-shaped
/// mechanics, with `superseded_by` pointing at the survivor (an ordinary
/// supersession outcome, not an evidence-death or audit one). Returns the
/// number of relation rows whose `src`/`dst` was actually repointed.
pub fn repoint_entity_relations(conn: &Connection, old_id: i64, new_id: i64, now: &str) -> Result<usize, KbError> {
    let mut count_stmt = conn.prepare("SELECT COUNT(*) FROM relations WHERE src = ?1 OR dst = ?1")?;
    let touched: i64 = count_stmt.query_row(params![old_id], |r| r.get(0))?;

    conn.execute("UPDATE relations SET src = ?1 WHERE src = ?2", params![new_id, old_id])?;
    conn.execute("UPDATE relations SET dst = ?1 WHERE dst = ?2", params![new_id, old_id])?;
    repoint_entity_mentions(conn, old_id, new_id)?;
    // The survivor inherited evidence its card never saw; drop the card so the
    // next reflect rebuilds it (a card is cheap to regenerate, wrong to keep).
    delete_entity_card(conn, new_id)?;

    let mut stmt = conn.prepare(
        "SELECT id, src, predicate, dst FROM relations
         WHERE invalidated_at IS NULL AND (src = ?1 OR dst = ?1) ORDER BY id ASC",
    )?;
    let rows: Vec<(i64, i64, String, i64)> =
        stmt.query_map(params![new_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
    let mut seen_keys: std::collections::HashMap<(i64, String, i64), i64> = std::collections::HashMap::new();
    for (id, src, predicate, dst) in rows {
        let key = (src, predicate.to_lowercase(), dst);
        match seen_keys.get(&key) {
            Some(&survivor) => {
                conn.execute(
                    "UPDATE relations SET invalidated_at = ?1, superseded_by = ?2 WHERE id = ?3 AND invalidated_at IS NULL",
                    params![now, survivor, id],
                )?;
            }
            None => {
                seen_keys.insert(key, id);
            }
        }
    }
    Ok(touched as usize)
}

/// Hard-deletes an entity row -- used only by the merge pass, and only
/// right after `repoint_entity_relations` has unconditionally moved every
/// relation off it, so by the time this runs the row has zero remaining
/// references. Returns whether a row existed.
pub fn delete_entity(conn: &Connection, id: i64) -> Result<bool, KbError> {
    conn.execute(
        "DELETE FROM relation_evidence WHERE relation_id IN (SELECT id FROM relations WHERE src = ?1 OR dst = ?1)",
        params![id],
    )?;
    conn.execute("DELETE FROM memory_entities WHERE entity_id = ?1", params![id])?;
    conn.execute("DELETE FROM entity_cards WHERE entity_id = ?1", params![id])?;
    let n = conn.execute("DELETE FROM entities WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

/// All ACTIVE relations, any entity, ascending by id -- the pool `mach kb
/// graph audit` re-examines every time it's run (not a drained backlog with
/// its own watermark, unlike `graph_extraction_candidates`: an edge this
/// audit invalidates simply drops out of this pool on the next run, and a
/// clean bill of health costs nothing to re-check).
pub fn active_relations_all(conn: &Connection) -> Result<Vec<Relation>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM relations WHERE invalidated_at IS NULL ORDER BY id ASC")?;
    let rows = stmt.query_map([], row_to_relation)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_conn() -> Connection {
        open_with_path(Path::new(":memory:")).expect("open in-memory store")
    }

    // Deterministic fake embedding, standing in for a real model in tests:
    // hashing-trick bag-of-words. Each lowercased word hashes into one of a
    // handful of buckets, so texts sharing words end up with a higher
    // cosine score than unrelated texts — enough to exercise ranking
    // without any network dependency.
    fn fnv1a(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn fake_embed(text: &str) -> Vec<f32> {
        const DIMS: usize = 64;
        let mut v = vec![0.0f32; DIMS];
        for word in text.to_lowercase().split_whitespace() {
            v[(fnv1a(word) as usize) % DIMS] += 1.0;
        }
        v
    }

    fn insert5(conn: &Connection, content: &str, embedding: Option<&[f32]>) -> i64 {
        insert(conn, content, None, None, true, embedding, 5).unwrap()
    }

    #[test]
    fn encode_decode_roundtrip() {
        let v = vec![1.0f32, -2.5, 0.0, 3.25];
        let blob = encode_embedding(&v);
        assert_eq!(blob.len(), 16);
        assert_eq!(decode_embedding(&blob), v);
    }

    #[test]
    fn decode_malformed_blob_is_empty() {
        assert_eq!(decode_embedding(&[1, 2, 3]), Vec::<f32>::new());
    }

    #[test]
    fn basis_round_trips_and_rejects_unknown_values() {
        let conn = mem_conn();
        let a = insert_with_basis(&conn, "the user said tea", None, None, true, None, 5, Some(BASIS_STATED)).unwrap();
        let b = insert(&conn, "legacy row", None, None, true, None, 5).unwrap();
        assert_eq!(get(&conn, a).unwrap().unwrap().basis.as_deref(), Some("stated"));
        assert_eq!(get(&conn, b).unwrap().unwrap().basis, None);
        set_basis(&conn, b, BASIS_INFERRED).unwrap();
        assert_eq!(get(&conn, b).unwrap().unwrap().basis.as_deref(), Some("inferred"));
        assert!(insert_with_basis(&conn, "x", None, None, true, None, 5, Some("guessed")).is_err());
        assert!(set_basis(&conn, a, "guessed").is_err());
        assert_eq!(get(&conn, a).unwrap().unwrap().basis.as_deref(), Some("stated"), "a rejected write must not touch the row");
    }

    #[test]
    fn entity_cards_round_trip_and_go_stale_on_new_or_dead_evidence() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let mut ids = Vec::new();
        for i in 0..CARD_MIN_MENTIONS {
            ids.push(insert(&conn, &format!("Moses said thing {}", i), None, None, true, None, 5).unwrap());
        }
        // due: enough mentions, no card yet
        let due = entity_card_candidates(&conn, 10).unwrap();
        assert_eq!(due.iter().map(|e| e.id).collect::<Vec<_>>(), vec![moses]);

        let wm = max_mention_memory_id(&conn, moses).unwrap();
        let deg = mention_degree(&conn, moses).unwrap();
        let src: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        upsert_entity_card(&conn, moses, "- pragmatic about hotfixes", &src, None, wm, deg, &now).unwrap();
        let card = get_entity_card(&conn, moses).unwrap().unwrap();
        assert_eq!(card.source_ids.len(), CARD_MIN_MENTIONS as usize);
        assert_eq!(card.built_watermark, wm);
        assert!(entity_card_candidates(&conn, 10).unwrap().is_empty(), "a current card is not due");

        // a new mention makes it stale
        let newer = insert(&conn, "Moses asked about NORAD ids", None, None, true, None, 5).unwrap();
        assert_eq!(entity_card_candidates(&conn, 10).unwrap().len(), 1);
        let wm2 = max_mention_memory_id(&conn, moses).unwrap();
        assert_eq!(wm2, newer);
        upsert_entity_card(&conn, moses, "- v2", &src, None, wm2, mention_degree(&conn, moses).unwrap(), &now).unwrap();
        assert!(entity_card_candidates(&conn, 10).unwrap().is_empty());
        // created_at survives a rebuild, updated_at is rewritten
        let rebuilt = get_entity_card(&conn, moses).unwrap().unwrap();
        assert_eq!(rebuilt.created_at, card.created_at);
        assert_eq!(rebuilt.text, "- v2");

        // evidence dying under the card also makes it stale
        conn.execute("UPDATE memories SET dormant_at = ?1 WHERE id = ?2", params![now, newer]).unwrap();
        assert_eq!(entity_card_candidates(&conn, 10).unwrap().len(), 1, "mention_count no longer matches");
    }

    #[test]
    fn entity_card_candidates_skip_thin_entities_and_the_reserved_user() {
        let conn = mem_conn();
        let thin = insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        insert(&conn, "Ivar reported a drift bug", None, None, true, None, 5).unwrap();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let now = now_rfc3339();
        for i in 0..10 {
            let m = insert(&conn, &format!("a fact {}", i), None, None, true, None, 5).unwrap();
            link_mention(&conn, m, user, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        }
        let due: Vec<i64> = entity_card_candidates(&conn, 10).unwrap().iter().map(|e| e.id).collect();
        assert!(!due.contains(&thin), "one mention is below CARD_MIN_MENTIONS");
        assert!(!due.contains(&user), "the reserved user entity never gets a card");
    }

    #[test]
    fn merging_an_entity_drops_the_survivors_stale_card_and_the_loser_row() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let keep = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let drop_ = insert_entity(&conn, "Dr Moses", Some("person"), None).unwrap();
        let m = insert(&conn, "a fact", None, None, true, None, 5).unwrap();
        link_mention(&conn, m, drop_, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        upsert_entity_card(&conn, keep, "- old card", &[], None, 0, 0, &now).unwrap();
        upsert_entity_card(&conn, drop_, "- doomed", &[], None, 0, 0, &now).unwrap();
        repoint_entity_relations(&conn, drop_, keep, &now).unwrap();
        delete_entity(&conn, drop_).unwrap();
        assert!(get_entity_card(&conn, keep).unwrap().is_none(), "survivor's card is dropped for a rebuild");
        assert!(get_entity_card(&conn, drop_).unwrap().is_none());
        assert_eq!(mention_degree(&conn, keep).unwrap(), 1);
    }

    #[test]
    fn spread_takes_one_hop_per_link_not_a_cluster_from_one_entity() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let e1 = insert_entity(&conn, "orchestrator", Some("component"), None).unwrap();
        let e2 = insert_entity(&conn, "popobawa", Some("infrastructure"), None).unwrap();
        let seed = insert(&conn, "seed", None, None, true, None, 5).unwrap();
        link_mention(&conn, seed, e1, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        link_mention(&conn, seed, e2, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        // five neighbours behind e1, one behind e2
        for i in 0..5 {
            let m = insert(&conn, &format!("orchestrator fact {}", i), None, None, true, None, 5).unwrap();
            link_mention(&conn, m, e1, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        }
        let only = insert(&conn, "a popobawa fact", None, None, true, None, 5).unwrap();
        link_mention(&conn, only, e2, MENTION_SOURCE_EXTRACTION, &now).unwrap();

        let hits =
            spread_activation(&conn, &[(seed, 0.9)], &std::collections::HashSet::new(), &now, None).unwrap();
        assert!(hits.len() <= SPREAD_MAX_OUT);
        assert_eq!(hits.iter().filter(|h| h.via_edge == "entity:orchestrator").count(), SPREAD_MAX_PER_EDGE);
        assert!(hits.iter().any(|h| h.id == only), "the quiet second link still gets its hop: {:?}", hits);
    }

    #[test]
    fn spread_prefers_the_shared_entity_the_query_is_about() {
        let conn = mem_conn();
        let now = now_rfc3339();
        // "orchestrator" sits on the query's axis; "systemd" is orthogonal.
        let subject = insert_entity(&conn, "orchestrator", Some("component"), Some(&unit_vec(4, 0))).unwrap();
        let incidental = insert_entity(&conn, "systemd", Some("technology"), Some(&unit_vec(4, 3))).unwrap();
        let seed = insert(&conn, "seed memory", None, None, true, None, 5).unwrap();
        let on_topic = insert(&conn, "an orchestrator fact", None, None, true, None, 5).unwrap();
        let off_topic = insert(&conn, "a systemd unit fact", None, None, true, None, 5).unwrap();
        for (m, e) in [(seed, subject), (seed, incidental), (on_topic, subject), (off_topic, incidental)] {
            link_mention(&conn, m, e, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        }
        let q = unit_vec(4, 0);
        let empty = std::collections::HashSet::new();

        let steered = spread_activation(&conn, &[(seed, 0.9)], &empty, &now, Some(&q)).unwrap();
        let ids: Vec<i64> = steered.iter().map(|h| h.id).collect();
        assert!(ids.contains(&on_topic), "{:?}", steered);
        assert!(!ids.contains(&off_topic), "an incidental shared entity must not hop: {:?}", steered);

        // With no query embedding both are equally walkable.
        let blind = spread_activation(&conn, &[(seed, 0.9)], &empty, &now, None).unwrap();
        let ids: Vec<i64> = blind.iter().map(|h| h.id).collect();
        assert!(ids.contains(&on_topic) && ids.contains(&off_topic), "{:?}", blind);
    }

    #[test]
    fn entity_edge_weight_only_calls_a_hub_a_hub_above_the_degree_floor() {
        // small bank: 2 of 5 memories is 40% share but only degree 2 -- not a hub
        assert!(entity_edge_weight(2, 5).is_some());
        // big enough degree AND share -> hub
        assert!(entity_edge_weight(SPREAD_HUB_MIN_DEGREE, 10).is_none());
        // big degree, tiny share -> still a real link
        assert!(entity_edge_weight(SPREAD_HUB_MIN_DEGREE, 1000).is_some());
        // above the absolute cap -> hub regardless of share
        assert!(entity_edge_weight(SPREAD_HUB_ABS + 1, 100_000).is_none());
        assert!(entity_edge_weight(0, 10).is_none());
        // not a hub means full weight, whatever the degree
        assert_eq!(entity_edge_weight(2, 1000).unwrap(), 1.0);
        assert_eq!(entity_edge_weight(30, 1000).unwrap(), 1.0);
    }

    #[test]
    fn backfill_mentions_tolerates_a_database_without_the_graph_tables() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (id INTEGER PRIMARY KEY, content TEXT NOT NULL, created_at TEXT NOT NULL);",
        )
        .unwrap();
        assert_eq!(backfill_mentions(&conn, &now_rfc3339()).unwrap(), 0);
    }

    #[test]
    fn name_mentioned_is_word_bounded_case_insensitive_and_skips_short_or_user() {
        assert!(name_mentioned("deployed to popobawa yesterday", "Popobawa"));
        assert!(!name_mentioned("the umojan portal", "umoja"));
        assert!(name_mentioned("the umoja portal", "umoja"));
        assert!(!name_mentioned("the user said", "user"));
        assert!(!name_mentioned("go go go", "go"));
        assert!(name_mentioned("mach kb search", "mach kb"));
    }

    #[test]
    fn insert_links_mentions_by_name_scan_and_new_entity_links_back() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let m1 = insert(&conn, "Moses asked for TLE ownership", None, None, true, None, 5).unwrap();
        let m2 = insert(&conn, "the sim agent runs on popobawa", None, None, true, None, 5).unwrap();
        assert_eq!(entities_of_memory(&conn, m1).unwrap().iter().map(|e| e.id).collect::<Vec<_>>(), vec![moses]);
        assert!(entities_of_memory(&conn, m2).unwrap().is_empty());
        let host = insert_entity(&conn, "popobawa", Some("infrastructure"), None).unwrap();
        assert_eq!(link_entity_mentions_by_name_scan(&conn, host, "popobawa", &now).unwrap(), 1);
        assert_eq!(memories_mentioning(&conn, host, None).unwrap()[0].id, m2);
        assert_eq!(mention_degree(&conn, host).unwrap(), 1);
        // merge: mentions follow the survivor, and the dropped entity's rows are gone
        let dup = insert_entity(&conn, "Dr Moses", Some("person"), None).unwrap();
        link_mention(&conn, m2, dup, MENTION_SOURCE_EXTRACTION, &now).unwrap();
        repoint_entity_relations(&conn, dup, moses, &now).unwrap();
        delete_entity(&conn, dup).unwrap();
        assert_eq!(mention_degree(&conn, moses).unwrap(), 2);
        assert_eq!(conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM memory_entities WHERE entity_id = ?1", params![dup], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn migrate_v15_to_v16_backfills_mentions_from_relations_and_names() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT, graph_extracted_at TEXT, basis TEXT);
             CREATE TABLE entities (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, kind TEXT, embedding BLOB, created_at TEXT NOT NULL);
             CREATE TABLE relations (id INTEGER PRIMARY KEY AUTOINCREMENT, src INTEGER NOT NULL, predicate TEXT NOT NULL, dst INTEGER NOT NULL,
                evidence_memory_id INTEGER, confidence REAL, created_at TEXT NOT NULL, valid_from TEXT, invalidated_at TEXT, superseded_by INTEGER);
             INSERT INTO memories (content, created_at) VALUES ('Ivar approved the redesign', '2026-01-01T00:00:00Z');
             INSERT INTO memories (content, created_at) VALUES ('nothing named here', '2026-01-01T00:00:00Z');
             INSERT INTO entities (name, kind, created_at) VALUES ('Ivar', 'person', '2026-01-01T00:00:00Z');
             INSERT INTO entities (name, kind, created_at) VALUES ('RHI redesign', 'project', '2026-01-01T00:00:00Z');
             INSERT INTO relations (src, predicate, dst, evidence_memory_id, created_at) VALUES (1, 'approved', 2, 1, '2026-01-01T00:00:00Z');
             PRAGMA user_version = 15;",
        )
        .unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let ents = entities_of_memory(&conn, 1).unwrap();
        assert_eq!(ents.len(), 2, "Ivar by name scan AND relation evidence, RHI redesign by relation evidence only");
        assert!(entities_of_memory(&conn, 2).unwrap().is_empty());
        migrate(&conn).unwrap(); // idempotent
        assert_eq!(entities_of_memory(&conn, 1).unwrap().len(), 2);
    }

    #[test]
    fn spread_activation_reaches_a_memory_through_a_shared_entity_but_not_a_hub() {
        let conn = mem_conn();
        let now = now_rfc3339();
        insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let hub = insert_entity(&conn, "umoja", Some("project"), None).unwrap();
        let seed = insert(&conn, "Moses wants TLE ownership in umoja", None, None, true, None, 5).unwrap();
        let via_moses = insert(&conn, "Moses prefers hotfix pragmatism", None, None, true, None, 5).unwrap();
        // 60 memories mention only the hub -> hub degree far above the cap
        let mut hub_only = Vec::new();
        for i in 0..60 {
            hub_only.push(insert(&conn, &format!("umoja note {}", i), None, None, true, None, 5).unwrap());
        }
        assert!(mention_degree(&conn, hub).unwrap() > SPREAD_HUB_ABS);
        let hits = spread_activation(&conn, &[(seed, 0.9)], &std::collections::HashSet::new(), &now, None).unwrap();
        let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
        assert!(ids.contains(&via_moses), "{:?}", hits);
        let m = hits.iter().find(|h| h.id == via_moses).unwrap();
        assert_eq!(m.via_seed, seed);
        assert_eq!(m.via_edge, "entity:Moses");
        assert!(m.activation < 0.9, "a hop never outranks its seed");
        assert!(m.activation > 0.3, "{:?}", m);
        assert!(hits.iter().filter(|h| h.via_edge == "entity:Moses").count() <= SPREAD_MAX_PER_EDGE);
        // nothing arrives through the hub entity, so the 60 hub-only notes stay out entirely
        assert!(hits.iter().all(|h| !hub_only.contains(&h.id)), "{:?}", hits);
        assert!(hits.len() <= SPREAD_MAX_OUT);
    }

    #[test]
    fn why_helpers_walk_citations_predecessors_edges_and_nearby_sessions() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let old = insert(&conn, "Moses is the boss", None, None, true, None, 5).unwrap();
        let new = insert(&conn, "Ivar is the boss", None, None, true, None, 5).unwrap();
        conn.execute("UPDATE memories SET superseded_by = ?1, invalidated_at = ?2 WHERE id = ?3", params![new, now, old]).unwrap();
        let ins = insert_insight(&conn, "the boss changed", 0.8, &[new.to_string(), "999".to_string()], None).unwrap();
        // a memory whose id is a suffix of ours must not be matched (quoted form)
        let _decoy = insert_insight(&conn, "decoy", 0.5, &[format!("1{}", new)], None).unwrap();
        let theme = insert_theme(&conn, "leadership shifts", 0.6, &[ins.to_string(), "998".to_string()], None).unwrap();
        let a = insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let b = insert_entity(&conn, "user", None, None).unwrap();
        insert_relation(&conn, a, "boss-of", b, Some(new), Some(0.9), &now).unwrap();
        conn.execute("INSERT INTO ingested_sessions (session_id, ingested_at) VALUES ('sess-A', ?1)", params![now]).unwrap();
        conn.execute("INSERT INTO session_progress (session_id, last_line, updated_at) VALUES ('sess-B', 10, '2000-01-01T00:00:00Z')", []).unwrap();

        let cites = insights_citing(&conn, new).unwrap();
        assert_eq!(cites.iter().map(|i| i.id).collect::<Vec<_>>(), vec![ins]);
        let cites_theme = insights_citing(&conn, ins).unwrap();
        assert_eq!(cites_theme.iter().map(|i| i.id).collect::<Vec<_>>(), vec![theme]);
        let preds = predecessors_of(&conn, new).unwrap();
        assert_eq!(preds.iter().map(|m| m.id).collect::<Vec<_>>(), vec![old]);
        let edges = relations_evidenced_by(&conn, new).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predicate, "boss-of");
        let near = sessions_near(&conn, &now, 120).unwrap();
        assert_eq!(near.len(), 1, "only the session ingested within the window");
        assert_eq!((near[0].0.as_str(), near[0].2), ("sess-A", "ingested"));
    }

    #[test]
    fn fts_query_sanitizes_and_keeps_exact_tokens() {
        assert_eq!(
            fts_query("What's the NORAD id 43005 for react-app on 10.8.0.63?").as_deref(),
            Some("\"norad\" OR \"43005\" OR \"react\" OR \"app\" OR \"10\" OR \"8\" OR \"0\" OR \"63\"")
        );
        assert_eq!(fts_query("the a to"), None);
        assert_eq!(fts_query("!!! ---"), None);
        // operator syntax cannot leak: everything is a quoted bare token
        let q = fts_query("NEAR(foo bar) \"quoted\" col:val x*").unwrap();
        assert!(!q.contains("NEAR("));
        assert!(!q.contains(':'));
        assert!(!q.contains('*'));
        assert!(q.split(" OR ").all(|t| t.starts_with('"') && t.ends_with('"')));
    }

    #[test]
    fn fts_index_tracks_insert_update_and_delete() {
        let conn = mem_conn();
        let id = insert(&conn, "TigriSat beacon NORAD 43005 at 435 MHz", None, None, true, None, 5).unwrap();
        assert!(lexical_scores(&conn, "43005", 10).unwrap().contains_key(&id));
        conn.execute("UPDATE memories SET content = 'renamed to OBJECT A, catalog 99999' WHERE id = ?1", [id]).unwrap();
        assert!(!lexical_scores(&conn, "43005", 10).unwrap().contains_key(&id));
        assert!(lexical_scores(&conn, "99999", 10).unwrap().contains_key(&id));
        conn.execute("DELETE FROM memories WHERE id = ?1", [id]).unwrap();
        assert!(lexical_scores(&conn, "99999", 10).unwrap().is_empty());
    }

    #[test]
    fn hybrid_search_finds_an_exact_token_the_embedding_misses() {
        let conn = mem_conn();
        let norad = insert(&conn, "TigriSat beacon uses NORAD 43005", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        insert(&conn, "the user prefers dark mode", None, None, true, Some(&unit_vec(4, 1)), 5).unwrap();
        let now = now_rfc3339();
        // A query embedding orthogonal to both rows: cosine alone has nothing.
        let q = unit_vec(4, 2);
        let plain = search_ranked(&conn, &q, 5, false, false, 0.0, &now).unwrap();
        assert!(plain.iter().all(|h| h.sim == 0.0 && h.lexical == 0.0));

        let hybrid = search_hybrid(&conn, "what is 43005", &q, 5, false, false, 0.0, &now, None).unwrap();
        assert_eq!(hybrid[0].memory.id, norad, "the exact catalog number must win on the lexical channel");
        assert!((hybrid[0].lexical - 1.0).abs() < 1e-6);
        assert_eq!(hybrid[0].sim, 0.0, "raw cosine is reported untouched");
        assert!(hybrid[0].score >= 0.7 * LEXICAL_WEIGHT, "a perfect lexical hit clears the hooks' 0.45 floor");
        assert!(hybrid[0].score > hybrid[1].score);
    }

    #[test]
    fn hybrid_search_never_exceeds_a_perfect_cosine_and_never_double_counts() {
        let conn = mem_conn();
        let both = insert(&conn, "catalog 43005", None, None, true, Some(&unit_vec(4, 0)), 5).unwrap();
        let now = now_rfc3339();
        let q = unit_vec(4, 0); // cosine 1.0 AND lexical 1.0
        let hybrid = search_hybrid(&conn, "43005", &q, 5, false, false, 0.0, &now, None).unwrap();
        let plain = search_ranked(&conn, &q, 5, false, false, 0.0, &now).unwrap();
        assert_eq!(hybrid[0].memory.id, both);
        assert!((hybrid[0].score - plain[0].score).abs() < 1e-6, "max(), not sum: same score as cosine alone");
    }

    #[test]
    fn hybrid_search_surfaces_an_embeddingless_row_on_a_lexical_hit_only() {
        let conn = mem_conn();
        let id = insert(&conn, "host popobawa is 10.8.0.63", None, None, true, None, 5).unwrap();
        let now = now_rfc3339();
        let q = unit_vec(4, 0);
        assert!(search_ranked(&conn, &q, 5, false, false, 0.0, &now).unwrap().is_empty());
        let hybrid = search_hybrid(&conn, "popobawa", &q, 5, false, false, 0.0, &now, None).unwrap();
        assert_eq!(hybrid.len(), 1);
        assert_eq!(hybrid[0].memory.id, id);
        // and with no usable tokens at all it is still excluded
        assert!(search_hybrid(&conn, "the a", &q, 5, false, false, 0.0, &now, None).unwrap().is_empty());
    }

    #[test]
    fn migrate_v14_to_v15_rebuilds_the_index_over_pre_existing_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT, graph_extracted_at TEXT, basis TEXT);
             INSERT INTO memories (content, created_at) VALUES ('legacy row mentions 43005', '2026-01-01T00:00:00Z');
             PRAGMA user_version = 14;",
        )
        .unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(lexical_scores(&conn, "43005", 10).unwrap().contains_key(&1), "pre-existing rows get indexed by the rebuild");
        migrate(&conn).unwrap(); // idempotent
    }

    #[test]
    fn add_column_if_missing_is_idempotent_across_a_lost_race() {
        // Simulates the second of two concurrent migrators: the column
        // already landed (the other process won), and this one's ALTER
        // must be a no-op success, not "duplicate column name".
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY);").unwrap();
        add_column_if_missing(&conn, "t", "basis", "TEXT").unwrap();
        add_column_if_missing(&conn, "t", "basis", "TEXT").unwrap();
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(t)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(cols.iter().filter(|c| *c == "basis").count(), 1);
        // a genuinely different failure still surfaces
        assert!(add_column_if_missing(&conn, "no_such_table", "x", "TEXT").is_err());
    }

    #[test]
    fn migrate_v13_to_v14_adds_basis_to_a_pre_existing_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT, graph_extracted_at TEXT);
             INSERT INTO memories (content, created_at) VALUES ('old', '2026-01-01T00:00:00Z');
             PRAGMA user_version = 13;",
        )
        .unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let m = get(&conn, 1).unwrap().unwrap();
        assert_eq!(m.basis, None, "pre-existing rows stay basis-unknown");
        // idempotent
        migrate(&conn).unwrap();
    }

    #[test]
    fn schema_version_constant_matches_what_migrate_actually_reaches() {
        let conn = mem_conn();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "bump SCHEMA_VERSION when adding a migration");
    }

    #[test]
    fn ensure_memory_columns_lets_an_early_migration_read_rows() {
        // A v15-era table lacks basis/occurred_*, and the v16 mention
        // backfill reads rows through `row_to_memory`, which needs them.
        // This is the ordering trap `ensure_memory_columns` exists for.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER);
             INSERT INTO memories (content, created_at) VALUES ('dated 2026-09-07', '2026-01-01T00:00:00Z');
             PRAGMA user_version = 15;",
        )
        .unwrap();
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();
        for (name, _) in MEMORY_COLUMNS {
            assert!(existing_columns(&conn).unwrap().iter().any(|c| c == name), "missing {}", name);
        }
        // and the v19 backfill still ran over it
        assert_eq!(get(&conn, 1).unwrap().unwrap().occurred_from.as_deref(), Some("2026-09-07"));
    }

    #[test]
    fn forgetting_a_cited_memory_repoints_edges_with_other_evidence_and_tombstones_the_rest() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let helios = insert_entity(&conn, "Helios", Some("project"), None).unwrap();
        let umoja = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let doomed = insert(&conn, "the memory being forgotten", None, None, true, None, 5).unwrap();
        let survivor = insert(&conn, "another memory backing the same claim", None, None, true, None, 5).unwrap();

        // One edge has a second piece of evidence; the other has only the
        // memory about to be forgotten.
        let shared = insert_relation(&conn, user, "works-on", helios, Some(doomed), Some(0.9), &now).unwrap();
        add_relation_evidence(&conn, shared, doomed, &now).unwrap();
        add_relation_evidence(&conn, shared, survivor, &now).unwrap();
        let sole = insert_relation(&conn, user, "works-on", umoja, Some(doomed), Some(0.9), &now).unwrap();
        add_relation_evidence(&conn, sole, doomed, &now).unwrap();

        // Before the fix this returned Err("FOREIGN KEY constraint failed"),
        // which made 214 of 738 live memories undeletable.
        assert!(delete(&conn, doomed).unwrap(), "a cited memory must still be deletable");
        assert!(get(&conn, doomed).unwrap().is_none());

        let shared = get_relation(&conn, shared).unwrap().unwrap();
        assert_eq!(
            shared.evidence_memory_id,
            Some(survivor),
            "an edge with other evidence is repointed at a survivor, not tombstoned"
        );
        assert!(shared.is_active(), "and stays active, because the claim still has backing");

        let sole = get_relation(&conn, sole).unwrap().unwrap();
        assert!(
            !sole.is_active(),
            "an edge whose only evidence was forgotten is tombstoned here, which is what \
             the graph hygiene pass would have done to it later anyway"
        );
        assert_eq!(sole.evidence_memory_id, None, "and holds no dangling reference");

        // No evidence link may outlive the memory it names.
        let left: i64 = conn
            .query_row(
                "SELECT count(*) FROM relation_evidence WHERE memory_id = ?1",
                params![doomed],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn one_claim_is_one_row_with_many_evidence_links() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let helios = insert_entity(&conn, "Helios", Some("project"), None).unwrap();
        let other = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let m1 = insert(&conn, "first mention", None, None, true, None, 5).unwrap();
        let m2 = insert(&conn, "second mention", None, None, true, None, 5).unwrap();

        let r1 = insert_relation(&conn, user, "works-on", helios, Some(m1), Some(0.9), &now).unwrap();
        // The same claim again from another memory, and once with different case.
        let r2 = insert_relation(&conn, user, "works-on", helios, Some(m2), Some(0.9), &now).unwrap();
        let r3 = insert_relation(&conn, user, "Works-On", helios, None, Some(0.9), &now).unwrap();
        // A genuinely different claim survives untouched.
        let r4 = insert_relation(&conn, user, "works-on", other, None, Some(0.9), &now).unwrap();

        // v21 style: seed the links the migration would have, then fold.
        add_relation_evidence(&conn, r1, m1, &now).unwrap();
        add_relation_evidence(&conn, r2, m2, &now).unwrap();
        assert_eq!(fold_duplicate_relations(&conn, &now).unwrap(), 2);

        assert!(get_relation(&conn, r1).unwrap().unwrap().is_active(), "the oldest row survives");
        assert!(!get_relation(&conn, r2).unwrap().unwrap().is_active());
        assert_eq!(get_relation(&conn, r2).unwrap().unwrap().superseded_by, Some(r1));
        assert!(!get_relation(&conn, r3).unwrap().unwrap().is_active(), "case does not make a new claim");
        assert!(get_relation(&conn, r4).unwrap().unwrap().is_active());

        // the survivor now carries both memories as evidence
        assert_eq!(relation_evidence(&conn, r1).unwrap(), vec![m1, m2]);
        assert!(relation_evidence(&conn, r2).unwrap().is_empty());

        // and extraction can find the claim to attach to instead of re-inserting
        assert_eq!(active_relation_for_claim(&conn, user, "WORKS-ON", helios).unwrap(), Some(r1));
        assert_eq!(active_relation_for_claim(&conn, user, "works-on", 999).unwrap(), None);
        assert_eq!(fold_duplicate_relations(&conn, &now).unwrap(), 0, "idempotent");
    }

    #[test]
    fn downstream_of_memory_walks_past_the_first_level() {
        let conn = mem_conn();
        let m = insert(&conn, "a fact", None, None, true, None, 5).unwrap();
        let i1 = insert_insight(&conn, "an insight on it", 0.8, &[m.to_string()], None).unwrap();
        let t1 = insert_theme(&conn, "a theme on the insight", 0.7, &[i1.to_string()], None).unwrap();
        let unrelated = insert_insight(&conn, "unrelated", 0.5, &["999".to_string()], None).unwrap();

        let down = downstream_of_memory(&conn, m).unwrap();
        assert!(down.contains(&i1) && down.contains(&t1), "the theme rests on the fact too: {:?}", down);
        assert!(!down.contains(&unrelated));

        // and flagging propagates the whole way, which is what the mental
        // model the user reads is actually built from
        let now = now_rfc3339();
        assert_eq!(flag_insights_citing_memory(&conn, m, &now).unwrap(), 2);
        assert!(get_insight(&conn, t1).unwrap().unwrap().is_flagged());
        // idempotent: a second pass does not re-subtract confidence
        let c = get_insight(&conn, t1).unwrap().unwrap().confidence;
        assert_eq!(flag_insights_citing_memory(&conn, m, &now).unwrap(), 0);
        assert_eq!(get_insight(&conn, t1).unwrap().unwrap().confidence, c);
    }

    #[test]
    fn repointing_never_crosses_the_two_id_spaces() {
        // A level-1 insight's source_ids are MEMORY ids; a theme's are
        // INSIGHT ids. Repointing insight 2 -> 1 must not touch memory "2"
        // inside a level-1 row, which is what corrupted evidence before the
        // level argument existed.
        let conn = mem_conn();
        let l1 = insert_insight(&conn, "cites memories 1 and 2", 0.7, &["1".to_string(), "2".to_string()], None).unwrap();
        let theme = insert_theme(&conn, "cites insight 2", 0.7, &["2".to_string()], None).unwrap();

        assert_eq!(repoint_insight_citations(&conn, 2, 1, 2).unwrap(), 1, "only the theme is rewritten");
        assert_eq!(get_insight(&conn, l1).unwrap().unwrap().source_ids, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(get_insight(&conn, theme).unwrap().unwrap().source_ids, vec!["1".to_string()]);
    }

    #[test]
    fn merge_insights_unions_evidence_keeps_the_higher_confidence_and_repoints_themes() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let keep = insert_insight(&conn, "the user audits before building", 0.6, &["1".to_string(), "2".to_string()], None).unwrap();
        let drop_ = insert_insight(&conn, "the user checks existing state first", 0.9, &["2".to_string(), "3".to_string()], None).unwrap();
        let theme = insert_theme(&conn, "engineering discipline", 0.7, &[drop_.to_string()], None).unwrap();

        assert!(merge_insights(&conn, keep, drop_, &now).unwrap());
        let merged = get_insight(&conn, keep).unwrap().unwrap();
        assert_eq!(merged.source_ids, vec!["1".to_string(), "2".to_string(), "3".to_string()]);
        assert!((merged.confidence - 0.9).abs() < 1e-9, "keeps the higher: {}", merged.confidence);
        assert!(!get_insight(&conn, drop_).unwrap().unwrap().is_active(), "the loser is invalidated, not deleted");
        assert_eq!(get_insight(&conn, theme).unwrap().unwrap().source_ids, vec![keep.to_string()]);
    }

    #[test]
    fn insight_dedupe_candidates_pair_only_same_level_unseen_and_similar() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "one phrasing", 0.7, &["1".to_string()], Some(&[1.0, 0.0])).unwrap();
        let b = insert_insight(&conn, "another phrasing", 0.7, &["2".to_string()], Some(&[0.99, 0.14])).unwrap();
        let far = insert_insight(&conn, "unrelated", 0.7, &["3".to_string()], Some(&[0.0, 1.0])).unwrap();
        let theme = insert_theme(&conn, "a theme", 0.7, &[a.to_string()], Some(&[1.0, 0.0])).unwrap();

        let seen = std::collections::HashSet::new();
        let pairs = insight_dedupe_candidate_pairs(&conn, 0.9, &seen, 10).unwrap();
        assert_eq!(pairs, vec![(a, b)], "same level, similar, oldest first");
        assert!(!pairs.iter().any(|(x, y)| *x == far || *y == far));
        assert!(!pairs.iter().any(|(x, y)| *x == theme || *y == theme), "a theme is never merged with an insight");

        mark_insight_dedupe_seen(&conn, b, a).unwrap();
        let seen = insight_dedupe_seen_pairs(&conn).unwrap();
        assert!(insight_dedupe_candidate_pairs(&conn, 0.9, &seen, 10).unwrap().is_empty(), "a judged pair is never re-judged");
    }

    #[test]
    fn insight_confidence_moves_both_ways() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let id = insert_insight(&conn, "a belief", 0.8, &["1".to_string()], None).unwrap();
        weaken_insight(&conn, id, &now).unwrap();
        let after_weaken = get_insight(&conn, id).unwrap().unwrap();
        assert!((after_weaken.confidence - 0.7).abs() < 1e-9, "one step down: {}", after_weaken.confidence);
        assert!(!after_weaken.is_flagged(), "weakening is not doubting");

        flag_insight(&conn, id, &now).unwrap();
        let after_flag = get_insight(&conn, id).unwrap().unwrap();
        assert!((after_flag.confidence - 0.5).abs() < 1e-9, "two steps down: {}", after_flag.confidence);
        assert!(after_flag.is_flagged());

        // and it floors rather than going negative
        for _ in 0..10 {
            flag_insight(&conn, id, &now).unwrap();
        }
        assert_eq!(get_insight(&conn, id).unwrap().unwrap().confidence, INSIGHT_CONFIDENCE_FLOOR);
    }

    #[test]
    fn flag_insight_bumps_last_verified_at_so_it_leaves_the_head_of_the_verification_queue() {
        let conn = mem_conn();
        let flagged = insert_insight(&conn, "a belief that gets contradicted", 0.8, &["1".to_string()], None).unwrap();
        let never_checked = insert_insight(&conn, "never yet verified", 0.6, &["2".to_string()], None).unwrap();
        let now = "2026-09-23T00:00:00Z";
        flag_insight(&conn, flagged, now).unwrap();

        let after = get_insight(&conn, flagged).unwrap().unwrap();
        assert_eq!(after.last_verified_at.as_deref(), Some(now), "a flag IS a verification outcome");

        // Without the bump, `flagged` (NULL last_verified_at) would still
        // sort ahead of `never_checked` every run -- one contradicted
        // insight would permanently crowd out the rest of the queue.
        let due = insights_due_for_verification(&conn, 10).unwrap();
        let due_ids: Vec<i64> = due.iter().map(|i| i.id).collect();
        assert!(
            due_ids.iter().position(|&id| id == never_checked) < due_ids.iter().position(|&id| id == flagged),
            "never-verified must now sort ahead of the just-flagged row: {:?}",
            due_ids
        );
    }

    #[test]
    fn revise_insight_rewrites_text_stashes_prev_text_and_clears_a_flag() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "the user always ships on Fridays", 0.7, &["1".to_string()], None).unwrap();
        flag_insight(&conn, id, "2026-09-20T00:00:00Z").unwrap();
        let flagged_confidence = get_insight(&conn, id).unwrap().unwrap().confidence;
        assert!(get_insight(&conn, id).unwrap().unwrap().is_flagged());

        let now = "2026-09-23T00:00:00Z";
        let ok =
            revise_insight(&conn, id, "the user ships mid-week, never on Fridays", Some(&[0.5, 0.5]), &[5, 6], now)
                .unwrap();
        assert!(ok);

        let after = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(after.text, "the user ships mid-week, never on Fridays");
        assert_eq!(after.prev_text.as_deref(), Some("the user always ships on Fridays"));
        assert_eq!(after.revised_at.as_deref(), Some(now));
        assert_eq!(after.last_verified_at.as_deref(), Some(now), "a revision is a verification outcome too");
        assert!(!after.is_flagged(), "a REVISE verdict means the corrected belief is supported again");
        assert_eq!(
            after.confidence,
            flagged_confidence - INSIGHT_CONFIDENCE_STEP,
            "a REVISE lowers confidence by one step, same as flag's own step (item 3 of the final fix list)"
        );
        assert_eq!(after.embedding, Some(vec![0.5, 0.5]));
        assert_eq!(after.source_ids, vec!["1".to_string(), "5".to_string(), "6".to_string()], "cited ids are appended");
    }

    #[test]
    fn revise_insight_lowers_confidence_by_one_step_with_a_floor() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "barely-supported belief", 0.1, &["1".to_string()], None).unwrap();
        revise_insight(&conn, id, "corrected wording", None, &[], "2026-09-23T00:00:00Z").unwrap();
        let after = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(
            after.confidence, INSIGHT_CONFIDENCE_FLOOR,
            "0.1 - INSIGHT_CONFIDENCE_STEP would go below the floor, so it must clamp there instead"
        );
    }

    #[test]
    fn revise_insight_appends_cited_ids_deduped_against_existing_source_ids() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "original", 0.6, &["1".to_string(), "5".to_string()], None).unwrap();
        revise_insight(&conn, id, "corrected", None, &[5, 6], "2026-09-23T00:00:00Z").unwrap();
        let after = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(
            after.source_ids,
            vec!["1".to_string(), "5".to_string(), "6".to_string()],
            "id 5 was already cited and must not be duplicated; id 6 is newly appended"
        );
    }

    #[test]
    fn revise_insight_clears_insight_dedupe_seen_rows_for_the_revised_id() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "original", 0.6, &["1".to_string()], None).unwrap();
        let other = insert_insight(&conn, "an unrelated insight", 0.6, &["2".to_string()], None).unwrap();
        let untouched_a = insert_insight(&conn, "untouched a", 0.6, &["3".to_string()], None).unwrap();
        let untouched_b = insert_insight(&conn, "untouched b", 0.6, &["4".to_string()], None).unwrap();
        mark_insight_dedupe_seen(&conn, id, other).unwrap();
        mark_insight_dedupe_seen(&conn, untouched_a, untouched_b).unwrap();

        revise_insight(&conn, id, "corrected", None, &[], "2026-09-23T00:00:00Z").unwrap();

        let seen = insight_dedupe_seen_pairs(&conn).unwrap();
        let (lo, hi) = if id <= other { (id, other) } else { (other, id) };
        assert!(!seen.contains(&(lo, hi)), "the revised insight's dedupe verdict must be cleared, to be re-judged");
        let (ulo, uhi) = if untouched_a <= untouched_b { (untouched_a, untouched_b) } else { (untouched_b, untouched_a) };
        assert!(seen.contains(&(ulo, uhi)), "an unrelated pair's verdict must be left alone");
    }

    #[test]
    fn revise_insight_keeps_the_old_embedding_when_the_caller_has_no_fresh_one() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "original", 0.6, &["1".to_string()], Some(&[1.0, 0.0])).unwrap();

        revise_insight(&conn, id, "corrected", None, &[], "2026-09-23T00:00:00Z").unwrap();

        let after = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(after.text, "corrected");
        assert_eq!(after.embedding, Some(vec![1.0, 0.0]), "no fresh embedding must not wipe the old one");
    }

    #[test]
    fn revise_insight_a_second_time_only_keeps_the_immediately_prior_wording() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "first wording", 0.6, &["1".to_string()], None).unwrap();
        revise_insight(&conn, id, "second wording", None, &[], "2026-09-21T00:00:00Z").unwrap();
        revise_insight(&conn, id, "third wording", None, &[], "2026-09-22T00:00:00Z").unwrap();

        let after = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(after.text, "third wording");
        assert_eq!(after.prev_text.as_deref(), Some("second wording"), "prev_text is one hop back, not the full history");
    }

    #[test]
    fn revise_insight_missing_id_returns_false() {
        let conn = mem_conn();
        assert!(!revise_insight(&conn, 999, "text", None, &[], "2026-09-23T00:00:00Z").unwrap());
    }

    #[test]
    fn surprisal_ranks_a_novel_fact_above_a_restatement() {
        let conn = mem_conn();
        insert5(&conn, "the orchestrator deploys to popobawa", Some(&[1.0, 0.0]));
        let restatement = insert5(&conn, "the orchestrator is deployed to the popobawa host", Some(&[0.999, 0.045]));
        let novel = insert5(&conn, "something entirely unrelated", Some(&[0.0, 1.0]));

        let s_novel = surprisal(&conn, &get(&conn, novel).unwrap().unwrap()).unwrap();
        let s_restate = surprisal(&conn, &get(&conn, restatement).unwrap().unwrap()).unwrap();
        assert!(s_novel > s_restate, "novel {} vs restatement {}", s_novel, s_restate);

        let rows = vec![
            get(&conn, restatement).unwrap().unwrap(),
            get(&conn, novel).unwrap().unwrap(),
        ];
        let ordered = order_by_surprisal(&conn, rows);
        assert_eq!(ordered[0].id, novel, "the surprising row is examined first");

        // no embedding -> neutral, never dropped
        let blind = insert(&conn, "no embedding", None, None, true, None, 5).unwrap();
        let s_blind = surprisal(&conn, &get(&conn, blind).unwrap().unwrap()).unwrap();
        assert_eq!(s_blind, 0.5);
        assert_eq!(order_by_surprisal(&conn, vec![get(&conn, blind).unwrap().unwrap()]).len(), 1);
    }

    #[test]
    fn a_single_common_word_is_not_a_lexical_match() {
        let conn = mem_conn();
        let id = insert(&conn, "deploying react-app to staging needs the built bundle", None, None, true, None, 5).unwrap();
        // One content term of three ("staging") -> under half the query's IDF mass, under the floor.
        assert!(lexical_scores(&conn, "what is the staging postgres password", 200).unwrap().get(&id).is_none());
        // Two of three clears it.
        assert!(lexical_scores(&conn, "how is staging react-app deployed", 200).unwrap().get(&id).is_some());
        // A pure quantifier is not a term at all.
        assert!(fts_terms("how many siblings do I have").iter().all(|t| t != "many"));
    }

    #[test]
    fn insights_and_memories_are_scored_on_the_same_scale() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let q: Vec<f32> = vec![1.0, 0.0];
        // Same embedding, so any score gap is the BLEND's fault, not the
        // content's. Before both used `normalize_sim` and one set of
        // weights, the insight scored roughly triple the memory.
        insert5(&conn, "a memory about the thing", Some(&[0.9, 0.436]));
        insert_insight(&conn, "an insight about the thing", 0.8, &["1".to_string()], Some(&[0.9, 0.436])).unwrap();
        let mem = search_ranked(&conn, &q, 5, false, false, 0.0, &now).unwrap();
        let ins = search_insights_ranked(&conn, &q, 5, &now).unwrap();
        assert!(!mem.is_empty() && !ins.is_empty());
        let gap = (mem[0].score - ins[0].score).abs();
        assert!(gap < 0.12, "memory {} vs insight {} must be comparable", mem[0].score, ins[0].score);
    }

    #[test]
    fn lexical_coverage_finds_a_row_matching_several_common_terms() {
        let conn = mem_conn();
        // The target matches every query term, but two of those terms
        // ("rust", "agent") are common in the bank -- the case that made
        // coverage-over-a-BM25-window return nothing, since BM25 on the OR
        // query buries a row that matches only common terms. Per-term-match
        // candidate discovery (not a BM25 window) still surfaces it; IDF
        // weighting is what then separates it from the decoys below.
        let target = insert(&conn, "the Rust agent wraps vendor modems behind a ModemDriver trait", None, None, true, None, 5).unwrap();
        for i in 0..60 {
            insert(&conn, &format!("another rust agent note number {}", i), None, None, true, None, 5).unwrap();
        }
        let scores = lexical_scores(&conn, "what vendor in the rust agent", 200).unwrap();
        let got = scores.get(&target).copied().unwrap_or(0.0);
        // terms: vendor, rust, agent -> target matches all three, and
        // "vendor" (df=1) carries far more weight than the ubiquitous
        // "rust"/"agent" (df=61, floored idf=1 each).
        assert!(got >= 0.9, "coverage {} should reflect a match on the rare term plus both common ones", got);
        // the decoys match only rust+agent (both common, idf floored at 1)
        // and are discounted below LEXICAL_MIN_COVERAGE entirely.
        let decoy = scores.iter().filter(|(id, _)| **id != target).map(|(_, v)| *v).fold(0.0f32, f32::max);
        assert!(got > decoy, "target {} vs decoy {}", got, decoy);
        assert_eq!(decoy, 0.0, "decoys matching only common terms are dropped by the floor");
    }

    #[test]
    fn lexical_coverage_discounts_common_terms() {
        let conn = mem_conn();
        // "mach" is common (present in every row below); "zeppelin" is rare
        // (present in exactly one). A row matching only the common term
        // must not ride "mach" past the floor, but the row that also has
        // the rare term should clear it easily.
        let mach_only = insert(&conn, "mach is a knowledge bank", None, None, true, None, 5).unwrap();
        for i in 0..18 {
            insert(&conn, &format!("mach note number {}", i), None, None, true, None, 5).unwrap();
        }
        let zeppelin = insert(&conn, "mach zeppelin project notes", None, None, true, None, 5).unwrap();
        let scores = lexical_scores(&conn, "mach zeppelin", 200).unwrap();
        let z = scores.get(&zeppelin).copied().unwrap_or(0.0);
        assert!(z > 0.5, "zeppelin row coverage {} should clear the floor on its rare term", z);
        let m = scores.get(&mach_only).copied().unwrap_or(0.0);
        assert!(m < 0.5, "mach-only row coverage {} should be discounted below the floor", m);
    }

    #[test]
    fn lexical_coverage_counts_terms_absent_from_the_corpus() {
        let conn = mem_conn();
        // "qwxzyv" appears in no row and never will. If an absent term were
        // dropped from the denominator instead of counted against it, every
        // "mach" row would score a perfect 1.0 on "mach qwxzyv" -- exactly
        // the common-term-rides-along problem this task removes. Kept in
        // the denominator, it permanently caps achievable coverage below
        // the floor for this query, so nothing is returned.
        let mut mach_ids = Vec::new();
        for i in 0..20 {
            mach_ids.push(insert(&conn, &format!("mach note number {}", i), None, None, true, None, 5).unwrap());
        }
        let scores = lexical_scores(&conn, "mach qwxzyv", 200).unwrap();
        for id in &mach_ids {
            assert!(scores.get(id).is_none(), "row {} should not clear the floor on a term absent from the corpus", id);
        }
    }

    #[test]
    fn dominant_term_guard_drops_a_partial_match_built_only_from_terms_at_or_above_the_median() {
        let conn = mem_conn();
        // Same shape as harness question ms4 ("why did I build the meeting
        // recorder"): "build" is common (df 13), "meeting" is rare (df 2),
        // "recorder" sits exactly at the query's own median df (4). A row
        // matching only build+recorder is built entirely from terms at or
        // above that median and must not clear the floor, even though its
        // IDF-weighted coverage (0.56) would have cleared it before this
        // guard existed. A row matching build+meeting still clears it,
        // because meeting is below the median.
        for i in 0..11 {
            insert(&conn, &format!("build note {}", i), None, None, true, None, 5).unwrap();
        }
        for i in 0..3 {
            insert(&conn, &format!("recorder note {}", i), None, None, true, None, 5).unwrap();
        }
        insert(&conn, "a meeting happened", None, None, true, None, 5).unwrap();
        let build_recorder = insert(&conn, "we build a recorder", None, None, true, None, 5).unwrap();
        let build_meeting = insert(&conn, "we build for the meeting", None, None, true, None, 5).unwrap();

        let scores = lexical_scores(&conn, "why did I build the meeting recorder", 200).unwrap();
        assert!(
            scores.get(&build_recorder).is_none(),
            "build+recorder is entirely at/above the query's median df ({:?}), must not clear the floor",
            scores.get(&build_recorder)
        );
        assert!(
            scores.get(&build_meeting).is_some(),
            "build+meeting includes meeting, which is below the median, so it still clears the floor"
        );
    }

    #[test]
    fn dominant_term_guard_exempts_a_full_match_even_when_every_term_ties_at_the_same_df() {
        let conn = mem_conn();
        // A single-row corpus: every query term that matches at all matches
        // in exactly that one row, so every matched df ties and the guard's
        // "median" degenerates to that same value. A FULL match (every
        // query term present) must still pass -- there's no unmatched
        // "incidental" term left to blame, so the guard is skipped
        // entirely regardless of how the matched terms' df compares.
        let id = insert(&conn, "the widget calibration runbook", None, None, true, None, 5).unwrap();
        let scores = lexical_scores(&conn, "widget calibration runbook", 200).unwrap();
        assert_eq!(
            scores.get(&id).copied(),
            Some(1.0),
            "a full match on a one-row corpus must still score 1.0, not be zeroed out by the guard"
        );
    }

    #[test]
    fn iso_dates_in_finds_real_dates_and_rejects_lookalikes() {
        assert_eq!(iso_dates_in("shipped 2026-08-31 and again 2026-09-07"), vec!["2026-08-31", "2026-09-07"]);
        assert_eq!(iso_dates_in("sorted"), Vec::<String>::new());
        assert!(iso_dates_in("version 1.2.3 at 10.8.0.63, NORAD 43005").is_empty());
        assert!(iso_dates_in("2026-13-01").is_empty(), "month 13");
        assert!(iso_dates_in("2026-00-10").is_empty(), "month 0");
        assert!(iso_dates_in("12026-01-01").is_empty(), "a longer number is not a date");
        // deduped and ordered
        assert_eq!(iso_dates_in("2026-09-08 then 2026-09-07 then 2026-09-08"), vec!["2026-09-07", "2026-09-08"]);
    }

    #[test]
    fn iso_dates_in_rejects_a_date_shaped_span_embedded_in_an_identifier() {
        assert!(iso_dates_in("TICKET-2026-01-15-hotfix").is_empty(), "preceded by '-' and followed by '-'");
        assert!(
            iso_dates_in("https://x/reports/2026-01-15/summary").is_empty(),
            "preceded by '/' and followed by '/'"
        );
        assert!(iso_dates_in("req-2026-01-15-abcxyz").is_empty(), "preceded by '-' and followed by '-'");
    }

    #[test]
    fn iso_dates_in_accepts_real_boundaries_and_iso_timestamps() {
        assert_eq!(iso_dates_in("shipped 2026-08-31."), vec!["2026-08-31"], "trailing punctuation is a real boundary");
        assert_eq!(iso_dates_in("(as of 2026-09-09)"), vec!["2026-09-09"], "parens are a real boundary on both sides");
        assert_eq!(
            iso_dates_in("2026-01-15T10:00:00Z"),
            vec!["2026-01-15"],
            "a 'T' + digit right after the date is an ISO timestamp, not an identifier suffix"
        );
    }

    #[test]
    fn query_date_range_reads_explicit_and_relative_time() {
        let today = "2026-09-08";
        assert_eq!(
            query_date_range("what shipped on 2026-08-31", today),
            Some(DateRange { from: "2026-08-31".into(), to: "2026-08-31".into() })
        );
        assert_eq!(
            query_date_range("between 2026-09-01 and 2026-09-05", today),
            Some(DateRange { from: "2026-09-01".into(), to: "2026-09-05".into() })
        );
        assert_eq!(query_date_range("what did I do today", today).unwrap().from, "2026-09-08");
        assert_eq!(query_date_range("what did I do yesterday", today).unwrap().from, "2026-09-07");
        let wk = query_date_range("what changed last week", today).unwrap();
        assert_eq!((wk.from.as_str(), wk.to.as_str()), ("2026-09-01", "2026-09-08"));
        let jun = query_date_range("what did we decide in June", today).unwrap();
        assert_eq!((jun.from.as_str(), jun.to.as_str()), ("2026-06-01", "2026-06-30"));
        let feb = query_date_range("february 2024 decisions", today).unwrap();
        assert_eq!(feb.to, "2024-02-29", "leap year");
        // no time reference at all -> the channel sits out
        assert_eq!(query_date_range("what host does zenith deploy to", today), None);
    }

    #[test]
    fn date_range_overlap_is_inclusive_at_both_ends() {
        let r = DateRange { from: "2026-09-05".into(), to: "2026-09-07".into() };
        assert!(r.overlaps("2026-09-07", "2026-09-09"), "touching at the end");
        assert!(r.overlaps("2026-09-01", "2026-09-05"), "touching at the start");
        assert!(r.overlaps("2026-09-06", "2026-09-06"), "inside");
        assert!(r.overlaps("2026-01-01", "2027-01-01"), "spanning");
        assert!(!r.overlaps("2026-09-08", "2026-09-09"));
        assert!(!r.overlaps("2026-09-01", "2026-09-04"));
    }

    #[test]
    fn a_dated_query_surfaces_the_memory_that_happened_then() {
        let conn = mem_conn();
        let now = now_rfc3339();
        // Both rows are equally unrelated to the query text; only the date
        // separates them, and only one carries an occurrence.
        let dated = insert(&conn, "satellite identity switched to a uuid", None, None, true, Some(&[0.0, 1.0]), 5).unwrap();
        set_occurrence(&conn, dated, "2026-08-31", "2026-08-31").unwrap();
        let undated = insert(&conn, "some other note entirely", None, None, true, Some(&[0.0, 1.0]), 5).unwrap();

        let q: Vec<f32> = vec![1.0, 0.0]; // orthogonal: no semantic signal at all
        let hits = search_hybrid(&conn, "what shipped on 2026-08-31", &q, 5, false, false, 0.0, &now, None).unwrap();
        assert_eq!(hits[0].memory.id, dated, "the dated match must outrank the undated row");
        // 0.45 is the real threshold -- `socket::DEFAULT_MIN_SCORE`, and
        // `mach kb eval`'s default, both mirroring kb-recall's. The old
        // assertion here was 0.6, a number that came from the previous
        // TEMPORAL_WEIGHT of 0.75 rather than from anything the system
        // actually enforces, so it broke when the weight became a floor at
        // 0.5 even though a 0.525 row is still injected.
        assert!(hits[0].score > 0.45, "a temporal match alone clears the injection floor: {}", hits[0].score);
        let undated_hit = hits.iter().find(|h| h.memory.id == undated).unwrap();
        assert!(hits[0].score > undated_hit.score);

        // The same query without a date gives the temporal channel nothing.
        let plain = search_hybrid(&conn, "what shipped", &q, 5, false, false, 0.0, &now, None).unwrap();
        assert!(plain.iter().all(|h| h.score < 0.45), "no date in the query, no temporal lift");
    }

    #[test]
    fn date_anchor_overrides_only_the_relative_date_parse_not_recency() {
        // `mach kb eval`'s `as_of`: the query's "yesterday" must resolve
        // against the anchor, not against `now`, while recency (which is
        // about how long ago the memory itself was touched, not what day
        // the query means) keeps using the real `now` untouched.
        let conn = mem_conn();
        let now = now_rfc3339(); // the real "today" this test runs on
        let anchor = "2026-09-09"; // a fixed, unrelated day: as_of on rel1..rel5

        let old = insert(&conn, "umoja admin access decision", None, None, true, Some(&[0.0, 1.0]), 5).unwrap();
        set_occurrence(&conn, old, "2026-09-08", "2026-09-08").unwrap(); // "yesterday" relative to the anchor
        let undated = insert(&conn, "some unrelated other note", None, None, true, Some(&[0.0, 1.0]), 5).unwrap();

        let q: Vec<f32> = vec![1.0, 0.0]; // orthogonal: no semantic signal

        // Without an anchor, "yesterday" resolves against the real today,
        // which this test's fixture predates by design -- no overlap, no
        // temporal lift for `old`.
        let unanchored = search_hybrid(&conn, "what did I decide yesterday", &q, 5, false, false, 0.0, &now, None).unwrap();
        let old_unanchored = unanchored.iter().find(|h| h.memory.id == old).unwrap();
        assert!(old_unanchored.score < 0.45, "2026-09-08 is not 'yesterday' relative to the real today");

        // With the anchor, "yesterday" resolves to 2026-09-08 and `old`
        // gets the temporal lift.
        let anchored =
            search_hybrid(&conn, "what did I decide yesterday", &q, 5, false, false, 0.0, &now, Some(anchor)).unwrap();
        let old_anchored = anchored.iter().find(|h| h.memory.id == old).unwrap();
        assert!(old_anchored.score > 0.45, "anchored to 2026-09-09, 'yesterday' is 2026-09-08 and must overlap `old`");

        // Recency is computed from `now`, never from the anchor: an
        // undated row (temporal channel always 0, anchored or not) must
        // score identically either way, and both its `recency` fields must
        // match exactly -- the anchor changed the date RANGE, not the
        // clock recency is measured against.
        let undated_unanchored = unanchored.iter().find(|h| h.memory.id == undated).unwrap();
        let undated_anchored = anchored.iter().find(|h| h.memory.id == undated).unwrap();
        assert_eq!(undated_unanchored.recency, undated_anchored.recency, "recency must not move with the date anchor");
        assert_eq!(undated_unanchored.score, undated_anchored.score, "an undated row's score is untouched by the anchor");
    }

    #[test]
    fn occurrence_is_taken_from_the_text_on_insert_and_validated_on_set() {
        let conn = mem_conn();
        let id = insert(&conn, "On 2026-09-07 Ivan tested the recorder", None, None, true, None, 5).unwrap();
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.occurred_from.as_deref(), Some("2026-09-07"));
        assert_eq!(m.occurred_to.as_deref(), Some("2026-09-07"));

        let undated = insert(&conn, "no date in this one", None, None, true, None, 5).unwrap();
        assert_eq!(get(&conn, undated).unwrap().unwrap().occurred_from, None, "undated stays NULL, never created_at");

        // reversed input is normalized, garbage is rejected
        set_occurrence(&conn, undated, "2026-09-09", "2026-09-01").unwrap();
        let m = get(&conn, undated).unwrap().unwrap();
        assert_eq!((m.occurred_from.as_deref(), m.occurred_to.as_deref()), (Some("2026-09-01"), Some("2026-09-09")));
        assert!(set_occurrence(&conn, undated, "yesterday", "today").is_err());
        assert!(set_occurrence(&conn, undated, "2026-13-01", "2026-13-02").is_err());
    }

    #[test]
    fn migrate_v18_to_v19_backfills_occurrence_from_the_text() {
        let conn = mem_conn();
        // Simulate pre-v19 rows by clearing what insert() derived.
        let dated = insert(&conn, "As of 2026-09-07, the bank held 150 memories", None, None, true, None, 5).unwrap();
        let undated = insert(&conn, "nothing dated here", None, None, true, None, 5).unwrap();
        conn.execute("UPDATE memories SET occurred_from = NULL, occurred_to = NULL", []).unwrap();
        let n = backfill_occurrence_from_text(&conn).unwrap();
        assert_eq!(n, 1);
        assert_eq!(get(&conn, dated).unwrap().unwrap().occurred_from.as_deref(), Some("2026-09-07"));
        assert_eq!(get(&conn, undated).unwrap().unwrap().occurred_from, None);
        assert_eq!(backfill_occurrence_from_text(&conn).unwrap(), 0, "idempotent");
    }

    #[test]
    fn rrf_contrib_falls_off_with_rank_and_rewards_agreement() {
        assert!(rrf_contrib(0) > rrf_contrib(1));
        assert!(rrf_contrib(1) > rrf_contrib(9));
        // Two channels agreeing at rank 2 beat one channel at rank 0 --
        // the whole point of fusing by rank rather than by score.
        assert!(rrf_contrib(2) * 2.0 > rrf_contrib(0));
    }

    #[test]
    fn normalize_sim_gives_cosine_a_usable_zero() {
        assert_eq!(normalize_sim(SIM_NOISE_FLOOR), 0.0);
        assert_eq!(normalize_sim(0.0), 0.0, "below the noise floor is still nothing");
        assert_eq!(normalize_sim(1.0), 1.0);
        // an unanswerable query's best cosine (measured 0.38-0.55) lands low
        assert!(normalize_sim(0.55) < 0.26);
        // an answerable one's (measured 0.67-0.80) lands high
        assert!(normalize_sim(0.67) > 0.44);
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0f32, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!(cosine(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn cosine_mismatched_len_is_zero() {
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0);
    }

    #[test]
    fn parse_rfc3339_roundtrips_through_now_rfc3339() {
        let s = now_rfc3339_from_secs(1_757_000_000);
        let secs = parse_rfc3339(&s).expect("parses");
        assert_eq!(secs, 1_757_000_000);
    }

    #[test]
    fn parse_rfc3339_rejects_garbage() {
        assert_eq!(parse_rfc3339("not a timestamp"), None);
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let conn = mem_conn();
        let emb = fake_embed("hello");
        let id = insert(&conn, "hello", Some("test"), Some("proj"), true, Some(&emb), 7).unwrap();
        let m = get(&conn, id).unwrap().expect("row exists");
        assert_eq!(m.content, "hello");
        assert_eq!(m.source.as_deref(), Some("test"));
        assert_eq!(m.project.as_deref(), Some("proj"));
        assert!(m.reviewed);
        assert_eq!(m.embedding.unwrap(), emb);
        assert!(!m.created_at.is_empty());
        assert_eq!(m.importance, 7);
        assert_eq!(m.stability, Some(49.0));
        assert_eq!(m.valid_from, m.created_at);
        assert_eq!(m.access_count, 0);
        assert!(m.invalidated_at.is_none());
        assert!(m.superseded_by.is_none());
    }

    #[test]
    fn distinct_projects_dedupes_orders_and_skips_empty_and_null() {
        let conn = mem_conn();
        insert(&conn, "a", None, Some("helios-rendering"), true, None, 5).unwrap();
        insert(&conn, "b", None, Some("helios-rendering"), true, None, 5).unwrap();
        insert(&conn, "c", None, Some("dotfiles"), true, None, 5).unwrap();
        insert(&conn, "d", None, None, true, None, 5).unwrap();
        insert(&conn, "e", None, Some("  "), true, None, 5).unwrap();
        insert(&conn, "f", None, Some(""), true, None, 5).unwrap();
        let projects = distinct_projects(&conn).unwrap();
        assert_eq!(projects, vec!["dotfiles".to_string(), "helios-rendering".to_string()]);
    }

    #[test]
    fn list_orders_newest_first_and_respects_limit() {
        let conn = mem_conn();
        for c in ["one", "two", "three"] {
            insert5(&conn, c, None);
        }
        let all = list(&conn, None, false).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].content, "three");
        assert_eq!(all[2].content, "one");

        let capped = list(&conn, Some(2), false).unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].content, "three");
    }

    #[test]
    fn list_hides_superseded_by_default_and_shows_only_them_with_flag() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old fact", None);
        let new_id = insert5(&conn, "new fact", None);
        let now = now_rfc3339();
        assert!(supersede(&conn, old_id, new_id, &now).unwrap());

        let active = list(&conn, None, false).unwrap();
        assert!(active.iter().all(|m| m.id != old_id));
        assert!(active.iter().any(|m| m.id == new_id));

        let superseded = list(&conn, None, true).unwrap();
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].id, old_id);
        assert_eq!(superseded[0].superseded_by, Some(new_id));
    }

    #[test]
    fn unreviewed_only_returns_reviewed_zero_rows() {
        let conn = mem_conn();
        insert5(&conn, "kept", None);
        let candidate_id = insert(&conn, "candidate", None, None, false, None, 5).unwrap();
        let pending = unreviewed(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, candidate_id);
        assert_eq!(pending[0].content, "candidate");
    }

    #[test]
    fn set_reviewed_flips_flag_and_removes_from_unreviewed() {
        let conn = mem_conn();
        let id = insert(&conn, "candidate", None, None, false, None, 5).unwrap();
        assert!(set_reviewed(&conn, id, true).unwrap());
        assert!(unreviewed(&conn).unwrap().is_empty());
        assert!(get(&conn, id).unwrap().unwrap().reviewed);
    }

    #[test]
    fn set_importance_updates_the_row_and_is_a_noop_on_a_missing_one() {
        let conn = mem_conn();
        let id = insert5(&conn, "will be demoted", None);
        assert!(set_importance(&conn, id, 2).unwrap());
        assert_eq!(get(&conn, id).unwrap().unwrap().importance, 2);
        assert!(!set_importance(&conn, 999, 2).unwrap());
    }

    #[test]
    fn age_days_computes_whole_fractional_days_and_floors_at_zero_on_clock_skew() {
        let now = now_rfc3339();
        assert_eq!(age_days(&now, &now), 0.0);
        let five_days_ago = now_rfc3339_from_secs(now_secs() - 5 * 86400);
        assert!((age_days(&five_days_ago, &now) - 5.0).abs() < 0.01);
        let future = now_rfc3339_from_secs(now_secs() + 86400);
        assert_eq!(age_days(&future, &now), 0.0, "created_at after now must floor at 0, not go negative");
    }

    #[test]
    fn curation_candidates_only_active_unreviewed_rows_past_the_settling_period() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let old_ts = now_rfc3339_from_secs(now_secs() - 10 * 86400);

        let old_unreviewed = insert(&conn, "old unreviewed", None, None, false, None, 5).unwrap();
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![old_ts, old_unreviewed]).unwrap();

        let young_unreviewed = insert(&conn, "young unreviewed", None, None, false, None, 5).unwrap();

        let old_reviewed = insert(&conn, "old reviewed", None, None, true, None, 5).unwrap();
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![old_ts, old_reviewed]).unwrap();

        let old_dormant = insert(&conn, "old dormant unreviewed", None, None, false, None, 5).unwrap();
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![old_ts, old_dormant]).unwrap();
        set_dormant(&conn, old_dormant, &now).unwrap();

        let candidates = curation_candidates(&conn, &now, 3.0, 12).unwrap();
        let ids: std::collections::HashSet<i64> = candidates.iter().map(|m| m.id).collect();
        assert!(ids.contains(&old_unreviewed));
        assert!(!ids.contains(&young_unreviewed), "must respect the settling period");
        assert!(!ids.contains(&old_reviewed), "reviewed rows are never curation candidates");
        assert!(!ids.contains(&old_dormant), "dormant rows are excluded");
    }

    #[test]
    fn curation_candidates_oldest_first_and_capped() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let mut ids = Vec::new();
        for i in 0..5u64 {
            let content = format!("candidate {}", i);
            let id = insert(&conn, &content, None, None, false, None, 5).unwrap();
            let ts = now_rfc3339_from_secs(now_secs() - (10 + i) * 86400);
            conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![ts, id]).unwrap();
            ids.push(id);
        }
        // ids[4] was backdated 14 days, ids[0] only 10 -- oldest-first means
        // ids[4] then ids[3].
        let capped = curation_candidates(&conn, &now, 3.0, 2).unwrap();
        assert_eq!(capped.len(), 2, "cap must be respected");
        assert_eq!(capped[0].id, ids[4], "oldest first");
        assert_eq!(capped[1].id, ids[3]);
    }

    #[test]
    fn update_content_with_new_embedding() {
        let conn = mem_conn();
        let id = insert(&conn, "old", None, None, true, Some(&fake_embed("old")), 5).unwrap();
        let new_emb = fake_embed("new");
        assert!(update_content(&conn, id, "new", Some(&new_emb)).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.content, "new");
        assert_eq!(m.embedding.unwrap(), new_emb);
    }

    #[test]
    fn update_content_without_embedding_keeps_old_vector() {
        let conn = mem_conn();
        let old_emb = fake_embed("old");
        let id = insert(&conn, "old", None, None, true, Some(&old_emb), 5).unwrap();
        assert!(update_content(&conn, id, "edited", None).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.content, "edited");
        assert_eq!(m.embedding.unwrap(), old_emb);
    }

    #[test]
    fn delete_removes_row_and_reports_existence() {
        let conn = mem_conn();
        let id = insert5(&conn, "x", None);
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
        assert!(!delete(&conn, id).unwrap(), "second delete of same id reports false");
    }

    #[test]
    fn supersede_tombstones_old_row_and_is_idempotent_false_on_repeat() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old", None);
        let new_id = insert5(&conn, "new", None);
        let now = now_rfc3339();
        assert!(supersede(&conn, old_id, new_id, &now).unwrap());
        let old = get(&conn, old_id).unwrap().unwrap();
        assert!(old.invalidated_at.is_some());
        assert_eq!(old.superseded_by, Some(new_id));
        // already tombstoned — a second call is a no-op, not a re-stamp
        assert!(!supersede(&conn, old_id, new_id, &now).unwrap());
    }

    #[test]
    fn restore_undoes_a_supersession_and_is_a_no_op_on_a_live_row() {
        let conn = mem_conn();
        let dated_2026_09_07 = insert5(&conn, "As of 2026-09-07 the bank held 150 memories", None);
        let dated_2026_09_09 = insert5(&conn, "As of 2026-09-09 the bank holds 739 memories", None);
        let now = now_rfc3339();
        // the contradiction judge read two dated snapshots as a stale claim
        // and its correction, so the older reading was tombstoned
        assert!(supersede(&conn, dated_2026_09_07, dated_2026_09_09, &now).unwrap());

        assert!(restore(&conn, dated_2026_09_07, &now).unwrap());
        let back = get(&conn, dated_2026_09_07).unwrap().unwrap();
        assert!(
            back.invalidated_at.is_none() && back.superseded_by.is_none(),
            "a restored row must be fully active again, not merely un-dated: a lingering \
             superseded_by would still point at a survivor that never replaced it"
        );
        // nothing to undo on a live row
        assert!(!restore(&conn, dated_2026_09_07, &now_rfc3339()).unwrap());
    }

    #[test]
    fn restore_pins_the_row() {
        // The bug this closes: restore cleared a tombstone but recorded
        // nothing, so the very next reflect pass re-judged the same pair
        // and tombstoned it again. Restore must pin the row so every
        // automatic tombstone path skips it from here on.
        let conn = mem_conn();
        let old_id = insert5(&conn, "old fact", None);
        let new_id = insert5(&conn, "new fact", None);
        let now = now_rfc3339();
        assert!(supersede(&conn, old_id, new_id, &now).unwrap());
        assert!(!is_pinned(&conn, old_id).unwrap(), "not pinned before restore");

        assert!(restore(&conn, old_id, &now).unwrap());
        assert!(is_pinned(&conn, old_id).unwrap(), "restore must pin the row");

        // a no-op restore (already active) must not pin anything
        let never_tombstoned = insert5(&conn, "always active", None);
        assert!(!restore(&conn, never_tombstoned, &now).unwrap());
        assert!(!is_pinned(&conn, never_tombstoned).unwrap());
    }

    // --- supersession audit ---

    #[test]
    fn supersession_candidates_joins_old_to_direct_successor_oldest_first() {
        let conn = mem_conn();
        let old_a = insert(&conn, "old A", None, None, true, None, 5).unwrap();
        let ts_a = now_rfc3339_from_secs(now_secs() - 20 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![ts_a, old_a]).unwrap();
        let old_b = insert(&conn, "old B", None, None, true, None, 5).unwrap();
        let ts_b = now_rfc3339_from_secs(now_secs() - 10 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![ts_b, old_b]).unwrap();
        let new_a = insert(&conn, "new A", None, None, true, None, 5).unwrap();
        let new_b = insert(&conn, "new B", None, None, true, None, 5).unwrap();
        let live = insert(&conn, "never tombstoned", None, None, true, None, 5).unwrap();
        let now = now_rfc3339();
        assert!(supersede(&conn, old_b, new_b, &now).unwrap());
        assert!(supersede(&conn, old_a, new_a, &now).unwrap());
        let _ = live;

        let candidates = supersession_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 2, "only the two tombstoned rows are candidates");
        assert_eq!(candidates[0].old_id, old_a, "the older tombstone comes first");
        assert_eq!(candidates[0].new_content, "new A");
        assert_eq!(candidates[1].old_id, old_b);
        assert_eq!(candidates[1].new_content, "new B");
    }

    #[test]
    fn supersession_candidates_audits_each_hop_of_a_chain_independently() {
        let conn = mem_conn();
        let a = insert(&conn, "A", None, None, true, None, 5).unwrap();
        let b = insert(&conn, "B", None, None, true, None, 5).unwrap();
        let c = insert(&conn, "C", None, None, true, None, 5).unwrap();
        let now = now_rfc3339();
        assert!(supersede(&conn, a, b, &now).unwrap());
        assert!(supersede(&conn, b, c, &now).unwrap());

        let candidates = supersession_candidates(&conn).unwrap();
        assert_eq!(candidates.len(), 2, "A->B and B->C are each their own hop");
        let by_old: HashMap<i64, i64> = candidates.iter().map(|c| (c.old_id, c.new_id)).collect();
        assert_eq!(by_old.get(&a), Some(&b));
        assert_eq!(by_old.get(&b), Some(&c));
    }

    #[test]
    fn record_supersession_audit_round_trips_and_reaudit_overwrites() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old fact", None);
        let new_id = insert5(&conn, "new fact", None);
        let now = now_rfc3339();
        assert!(get_supersession_audit(&conn, old_id).unwrap().is_none());

        record_supersession_audit(&conn, old_id, new_id, "OK", "no lost facts", &now).unwrap();
        let row = get_supersession_audit(&conn, old_id).unwrap().unwrap();
        assert_eq!(row.new_id, new_id);
        assert_eq!(row.verdict, "OK");
        assert_eq!(row.reason, "no lost facts");
        assert_eq!(supersession_audited_pairs(&conn).unwrap().get(&old_id), Some(&new_id));

        // --reaudit overwrites the same primary key rather than erroring
        let later = now_rfc3339();
        record_supersession_audit(&conn, old_id, new_id, "LOSSY", "dated fact dropped", &later).unwrap();
        let row = get_supersession_audit(&conn, old_id).unwrap().unwrap();
        assert_eq!(row.verdict, "LOSSY");
        assert_eq!(row.reason, "dated fact dropped");
    }

    #[test]
    fn supersession_audit_lossy_rows_excludes_ok_verdicts_oldest_first() {
        let conn = mem_conn();
        let (old_a, new_a) = (insert5(&conn, "old A", None), insert5(&conn, "new A", None));
        let (old_b, new_b) = (insert5(&conn, "old B", None), insert5(&conn, "new B", None));
        let (old_ok, new_ok) = (insert5(&conn, "old OK", None), insert5(&conn, "new OK", None));
        record_supersession_audit(&conn, old_b, new_b, "LOSSY", "reason b", "2026-09-10T00:00:00Z").unwrap();
        record_supersession_audit(&conn, old_a, new_a, "LOSSY", "reason a", "2026-09-01T00:00:00Z").unwrap();
        record_supersession_audit(&conn, old_ok, new_ok, "OK", "fine", "2026-09-05T00:00:00Z").unwrap();

        let rows = supersession_audit_lossy_rows(&conn).unwrap();
        assert_eq!(rows.len(), 2, "the OK-verdict row must not appear");
        assert_eq!(rows[0].old_id, old_a, "oldest audited_at first");
        assert_eq!(rows[1].old_id, old_b);
    }

    #[test]
    fn unpin_clears_pinned_at() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old fact", None);
        let new_id = insert5(&conn, "new fact", None);
        let now = now_rfc3339();
        supersede(&conn, old_id, new_id, &now).unwrap();
        restore(&conn, old_id, &now).unwrap();
        assert!(is_pinned(&conn, old_id).unwrap());

        assert!(unpin(&conn, old_id).unwrap());
        assert!(!is_pinned(&conn, old_id).unwrap());
        // second unpin is a no-op
        assert!(!unpin(&conn, old_id).unwrap());
    }

    #[test]
    fn is_pinned_false_for_unknown_id() {
        let conn = mem_conn();
        assert!(!is_pinned(&conn, 999_999).unwrap());
    }

    #[test]
    fn search_substring_fallback_matches_case_insensitively() {
        let conn = mem_conn();
        insert5(&conn, "The Boss wants retention metrics", None);
        insert5(&conn, "completely different topic", None);

        let results = search_substring(&conn, "boss", 10, false, false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.content, "The Boss wants retention metrics");

        let none = search_substring(&conn, "nonexistent phrase", 10, false, false).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn search_substring_excludes_superseded_unless_included() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "shared needle old", None);
        let new_id = insert5(&conn, "shared needle new", None);
        supersede(&conn, old_id, new_id, &now_rfc3339()).unwrap();

        let default_hits = search_substring(&conn, "needle", 10, false, false).unwrap();
        assert_eq!(default_hits.len(), 1);
        assert_eq!(default_hits[0].0.id, new_id);

        let all_hits = search_substring(&conn, "needle", 10, false, true).unwrap();
        assert_eq!(all_hits.len(), 2);
        let old_hit = all_hits.iter().find(|(m, _)| m.id == old_id).unwrap();
        assert_eq!(old_hit.1, 0.1);
    }

    // --- ranking blend ---

    #[test]
    fn ranking_favors_high_similarity_when_all_else_equal() {
        let conn = mem_conn();
        // Explicit vectors rather than `fake_embed`: scores now run through
        // `normalize_sim`, which floors everything under SIM_NOISE_FLOOR at
        // 0, and fake_embed's whole cosine range sits below that floor -- so
        // both rows would tie at 0 and the test would prove nothing.
        insert5(&conn, "the boss wants Q4 retention metrics", Some(&[1.0, 0.05]));
        insert5(&conn, "unrelated quantum chess trivia", Some(&[0.6, 0.8]));
        insert(&conn, "no embedding stored for this one", None, None, true, None, 5).unwrap();

        let now = now_rfc3339();
        let query: Vec<f32> = vec![1.0, 0.0];
        let results = search_ranked(&conn, &query, 5, false, false, 0.0, &now).unwrap();
        assert!(results[0].sim > results.last().unwrap().sim, "the near-parallel row is more similar");

        // the no-embedding row must never surface from a cosine search
        assert!(results.iter().all(|h| h.memory.content != "no embedding stored for this one"));
        assert!(!results.is_empty());
        assert_eq!(results[0].memory.content, "the boss wants Q4 retention metrics");
        assert!(results[0].score > results.last().unwrap().score);
    }

    #[test]
    fn ranking_includes_unreviewed_by_default_with_penalty_but_reviewed_only_excludes_them() {
        let conn = mem_conn();
        let emb = fake_embed("shared topic");
        insert5(&conn, "reviewed memory shared topic", Some(&emb));
        insert(&conn, "unreviewed memory shared topic", None, None, false, Some(&emb), 5).unwrap();

        let now = now_rfc3339();
        let default_results = search_ranked(&conn, &emb, 10, false, false, 0.0, &now).unwrap();
        assert_eq!(default_results.len(), 2, "default search must be organic — unreviewed rows surface too");
        assert_eq!(default_results[0].memory.content, "reviewed memory shared topic", "same sim/recency/strength, so the unpenalized reviewed row wins");
        assert_eq!(default_results[1].memory.content, "unreviewed memory shared topic");
        let expected_penalized = default_results[0].score * UNREVIEWED_SEARCH_PENALTY;
        assert!(
            (default_results[1].score - expected_penalized).abs() < 1e-4,
            "unreviewed score {} must equal reviewed score {} x {}",
            default_results[1].score,
            default_results[0].score,
            UNREVIEWED_SEARCH_PENALTY
        );

        let reviewed_only_results = search_ranked(&conn, &emb, 10, true, false, 0.0, &now).unwrap();
        assert_eq!(reviewed_only_results.len(), 1, "--reviewed-only must still exclude unreviewed rows");
        assert_eq!(reviewed_only_results[0].memory.content, "reviewed memory shared topic");
    }

    #[test]
    fn ranking_respects_limit() {
        let conn = mem_conn();
        for i in 0..5 {
            let text = format!("memory number {}", i);
            insert5(&conn, &text, Some(&fake_embed(&text)));
        }
        let now = now_rfc3339();
        let results = search_ranked(&conn, &fake_embed("memory number 2"), 2, false, false, 0.0, &now).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn ranking_additive_blend_lets_high_sim_old_beat_low_sim_fresh() {
        // The property this blend exists for: an old, never-touched memory
        // with a strong topical match must still outrank a brand new,
        // barely-related one — decay (recency) never buries relevance
        // (sim) outright, it only nudges the ranking.
        let conn = mem_conn();
        let topic = "quarterly retention plan for enterprise customers";
        let old_id = insert5(&conn, topic, Some(&fake_embed(topic)));
        // stability stays at the default (5*7=35 days); push last_accessed
        // (== created_at, never touched) back 400 days so recency decays
        // hard, while similarity stays high (same text).
        let old_now = now_secs() as i64 - 400 * 86400;
        let old_ts = now_rfc3339_from_secs(old_now as u64);
        conn.execute(
            "UPDATE memories SET created_at = ?1, valid_from = ?1 WHERE id = ?2",
            params![old_ts, old_id],
        )
        .unwrap();

        let unrelated = "a completely unrelated grocery list for the weekend";
        insert5(&conn, unrelated, Some(&fake_embed(unrelated)));

        let now = now_rfc3339();
        let query = fake_embed("quarterly retention plan enterprise");
        let results = search_ranked(&conn, &query, 5, false, false, 0.0, &now).unwrap();
        assert_eq!(results[0].memory.id, old_id, "high-sim old memory must still win");
    }

    // --- reinforcement ---

    #[test]
    fn touch_resets_recency_and_updates_access_bookkeeping() {
        let conn = mem_conn();
        let id = insert5(&conn, "reinforced fact", None);
        // importance 5 -> initial stability 35.0
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(35.0));

        // Backdate creation well past the current 35.0-day stability so
        // this touch is a fully spaced repetition (elapsed >= stability),
        // isolating the access-bookkeeping assertions from the interval
        // math covered by the dedicated spacing tests below.
        let created = now_rfc3339_from_secs(now_secs() - 40 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![created, id]).unwrap();

        let now = now_rfc3339();
        touch(&conn, &[id], &now).unwrap();
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.access_count, 1);
        assert_eq!(m.last_accessed_at.as_deref(), Some(now.as_str()));
        assert_eq!(m.first_accessed_at.as_deref(), Some(now.as_str()));
        assert!((m.stability.unwrap() - 35.0 * 1.3).abs() < 1e-6);
    }

    #[test]
    fn touch_same_minute_repeat_barely_grows_stability() {
        // Two touches moments apart (elapsed_days ~= 0) must not deliver
        // the flat 30% gain — that's the spacing effect this task adds.
        let conn = mem_conn();
        let id = insert5(&conn, "reinforced fact", None);
        let now = now_rfc3339();
        touch(&conn, &[id], &now).unwrap();
        let after_first = get(&conn, id).unwrap().unwrap().stability.unwrap();

        touch(&conn, &[id], &now).unwrap();
        let after_second = get(&conn, id).unwrap().unwrap().stability.unwrap();

        let growth = (after_second - after_first) / after_first;
        assert!(growth < 0.01, "same-instant re-touch grew stability by {}%, expected < 1%", growth * 100.0);
    }

    #[test]
    fn touch_after_full_interval_grows_by_flat_1_3x() {
        // A gap at least as long as the current stability earns the full
        // ×1.3 gain, same as the old flat rule (spacing effect saturates).
        let conn = mem_conn();
        let id = insert5(&conn, "spaced fact", None); // stability 35.0
        let created = now_rfc3339_from_secs(now_secs() - 100 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![created, id]).unwrap();

        let now = now_rfc3339();
        touch(&conn, &[id], &now).unwrap();
        let after_first = get(&conn, id).unwrap().unwrap().stability.unwrap();
        assert!((after_first - 35.0 * 1.3).abs() < 1e-6);

        // second touch after another gap >= the new stability (45.5 days)
        let later_secs = parse_rfc3339(&now).unwrap() + (after_first.ceil() as i64 + 1) * 86400;
        let later = now_rfc3339_from_secs(later_secs as u64);
        touch(&conn, &[id], &later).unwrap();
        let after_second = get(&conn, id).unwrap().unwrap().stability.unwrap();
        assert!((after_second - after_first * 1.3).abs() < 1e-6);
    }

    #[test]
    fn touch_stability_cap_holds_at_365() {
        let conn = mem_conn();
        let id = insert5(&conn, "very reinforced fact", None);
        let mut t = parse_rfc3339(&now_rfc3339()).unwrap();
        // Each touch is spaced by more than the row's current stability so
        // every touch earns the full 1.3x gain; a handful of those clears
        // the 365-day cap.
        for _ in 0..40 {
            let stability = get(&conn, id).unwrap().unwrap().stability.unwrap_or(35.0);
            t += ((stability.ceil() as i64) + 1) * 86400;
            let ts = now_rfc3339_from_secs(t as u64);
            touch(&conn, &[id], &ts).unwrap();
        }
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(365.0));
    }

    #[test]
    fn touch_first_touch_of_never_accessed_row_uses_created_at() {
        // A row that has never been touched has no last_accessed_at, so
        // the elapsed interval for its first touch must be measured from
        // created_at, not from "now" (which would always read as 0 days).
        let conn = mem_conn();
        let id = insert5(&conn, "never touched fact", None); // stability 35.0
        let created = now_rfc3339_from_secs(now_secs() - 50 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![created, id]).unwrap();
        assert!(get(&conn, id).unwrap().unwrap().last_accessed_at.is_none());

        let now = now_rfc3339();
        touch(&conn, &[id], &now).unwrap();
        let m = get(&conn, id).unwrap().unwrap();
        assert!((m.stability.unwrap() - 35.0 * 1.3).abs() < 1e-6);
    }

    #[test]
    fn touch_reinforcement_saturates_like_a_log_not_a_line() {
        // ACT-R's log form must self-limit: repeated touches keep raising
        // strength, but by ever-smaller amounts — the "rich get richer"
        // fix the linear count would fail to provide.
        let conn = mem_conn();
        let id = insert5(&conn, "heavily reinforced fact", None);
        let now = now_rfc3339();

        let strength_after = |n: i64, conn: &Connection| -> f32 {
            // touching n times, then reading strength as of `now` (touch
            // itself doesn't change access_count/first_accessed_at beyond
            // what search reads, so recompute via a 1-row ranked search).
            let m = get(conn, id).unwrap().unwrap();
            let _ = n;
            compute_strength(&m, parse_rfc3339(&now).unwrap())
        };

        touch(&conn, &vec![id; 5], &now).unwrap();
        let s5 = strength_after(5, &conn);
        touch(&conn, &vec![id; 5], &now).unwrap(); // total 10
        let s10 = strength_after(10, &conn);
        touch(&conn, &vec![id; 40], &now).unwrap(); // total 50
        let s50 = strength_after(50, &conn);
        touch(&conn, &vec![id; 50], &now).unwrap(); // total 100
        let s100 = strength_after(100, &conn);

        assert!(s100 - s50 < s10 - s5, "gain from 50->100 must be smaller than 5->10 (log growth)");
    }

    // --- migration ---

    #[test]
    fn migration_v0_backfills_new_columns_and_keeps_rows() {
        // Simulate a pre-migration (v0) database by building the old
        // 7-column schema by hand and inserting a row the way the old
        // `insert` used to, bypassing `open_with_path`/`init_schema` so the
        // new columns are genuinely absent going into `migrate`.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY,
                content TEXT NOT NULL,
                source TEXT,
                project TEXT,
                created_at TEXT NOT NULL,
                reviewed INTEGER NOT NULL DEFAULT 1,
                embedding BLOB
            );
            INSERT INTO memories (content, source, project, created_at, reviewed, embedding)
            VALUES ('legacy memory', 'test', 'proj', '2026-01-01T00:00:00Z', 1, NULL);",
        )
        .unwrap();

        assert_eq!(existing_columns(&conn).unwrap().len(), 7);
        // init_schema (CREATE TABLE IF NOT EXISTS) always runs before
        // migrate in the real open_with_path flow — it's what creates the
        // insights/reflect_state tables the v1->v2 step below now expects
        // to already exist. It's a no-op on the memories table itself
        // (already present with the old 7 columns; IF NOT EXISTS never
        // alters an existing table), so the migration below still has real
        // ALTER work to do.
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        // From v0, migrate() runs every step in one call: v0->v1 (the
        // memories column backfill this test is about), v1->v2 (reflection
        // tables), v2->v3 (the insights `level` column), v3->v4 (the
        // memories `dormant_at` column), v4->v5 (the `dedupe_seen` table),
        // v5->v6 (`last_verified_at` + `contradiction_seen`), v6->v7 (the
        // AUTOINCREMENT rebuild), v7->v8 (`ingested_sessions`), ... v10->v11
        // (`improve_state` + `ingested_sessions.skill_usage`), landing at the
        // current version.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1);
        let m = &rows[0];
        assert_eq!(m.content, "legacy memory");
        assert_eq!(m.source.as_deref(), Some("test"));
        assert_eq!(m.importance, 5);
        assert_eq!(m.stability, Some(35.0));
        assert_eq!(m.valid_from, "2026-01-01T00:00:00Z");
        assert_eq!(m.access_count, 0);
        assert!(m.invalidated_at.is_none());
        assert!(m.dormant_at.is_none());

        // idempotent: running it again (as every `open` does) is a no-op
        migrate(&conn).unwrap();
        let rows_again = list(&conn, None, false).unwrap();
        assert_eq!(rows_again.len(), 1);
    }

    // (fresh-database version-landing is covered by
    // fresh_database_lands_at_user_version_4 in the reflection tests below)

    // --- supersession apply_verdict ---

    #[test]
    fn apply_verdict_add_inserts_plainly() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let outcome = apply_verdict(&conn, Verdict::Add, "a new fact", None, None, true, &fake_embed("a new fact"), 5, &now, None).unwrap();
        match outcome {
            AddOutcome::Added { id } => assert!(get(&conn, id).unwrap().is_some()),
            _ => panic!("expected Added"),
        }
    }

    #[test]
    fn apply_verdict_update_keeps_old_row_active() {
        // UPDATE means "refines" — keep both, as history — not "replaces".
        // A colour decision tombstoned by a later, unrelated font decision
        // is exactly the data loss this must never do again.
        let conn = mem_conn();
        let old_id = insert5(&conn, "old version of the fact", None);
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Update(old_id), "new version of the fact", None, None, true,
            &fake_embed("new version of the fact"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::AddedRefining { new_id, old_id: o } => {
                assert_eq!(o, old_id);
                assert!(get(&conn, new_id).unwrap().is_some());
                let old = get(&conn, old_id).unwrap().unwrap();
                assert_eq!(old.superseded_by, None, "UPDATE must not tombstone the old row");
                assert!(old.invalidated_at.is_none(), "UPDATE must not tombstone the old row");
            }
            _ => panic!("expected AddedRefining"),
        }
    }

    #[test]
    fn apply_verdict_supersede_inserts_new_and_tombstones_old() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "obsolete fact", None);
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Supersede(old_id), "replacement fact", None, None, true,
            &fake_embed("replacement fact"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::AddedAndTombstoned { verb, .. } => assert_eq!(verb, "superseded"),
            _ => panic!("expected AddedAndTombstoned"),
        }
    }

    #[test]
    fn apply_verdict_noop_skips_insert() {
        let conn = mem_conn();
        let existing_id = insert5(&conn, "already known fact", None);
        let now = now_rfc3339();
        let before = list(&conn, None, false).unwrap().len();
        let outcome = apply_verdict(
            &conn, Verdict::Noop, "already known fact", None, None, true,
            &fake_embed("already known fact"), 5, &now, Some(existing_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::Skipped { reason } => assert!(reason.contains(&existing_id.to_string())),
            _ => panic!("expected Skipped"),
        }
        assert_eq!(list(&conn, None, false).unwrap().len(), before);
    }

    #[test]
    fn apply_verdict_update_with_stale_id_still_adds() {
        // The id in the verdict doesn't exist (classifier hallucinated,
        // or the row was deleted between the k-NN check and the verdict)
        // — the new fact must never be lost over it.
        let conn = mem_conn();
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Update(999_999), "a fact that must survive", None, None, true,
            &fake_embed("a fact that must survive"), 5, &now, Some(999_999),
        )
        .unwrap();
        match outcome {
            AddOutcome::Added { id } => assert!(get(&conn, id).unwrap().is_some()),
            _ => panic!("expected a plain Added fallback"),
        }
    }

    #[test]
    fn apply_verdict_supersede_of_pinned_row_adds_instead() {
        // A restored (pinned) row must never be re-tombstoned by an
        // automatic verdict — the whole point of pinning it.
        let conn = mem_conn();
        let old_id = insert5(&conn, "restored fact", None);
        let now = now_rfc3339();
        assert!(restore_pins_via_supersede_then_restore(&conn, old_id, &now));

        let outcome = apply_verdict(
            &conn, Verdict::Supersede(old_id), "a would-be replacement", None, None, true,
            &fake_embed("a would-be replacement"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::Added { id } => assert!(get(&conn, id).unwrap().is_some()),
            _ => panic!("expected a plain Added fallback, not a tombstone of the pinned row"),
        }
        let old = get(&conn, old_id).unwrap().unwrap();
        assert!(old.invalidated_at.is_none(), "the pinned row must stay active");
        assert!(old.superseded_by.is_none());
    }

    #[test]
    fn apply_verdict_update_of_pinned_row_keeps_it_active() {
        // Update never tombstones at all, so a pinned row is no different
        // from any other here — it still comes back as AddedRefining, and
        // pin status never even gets consulted.
        let conn = mem_conn();
        let old_id = insert5(&conn, "restored fact", None);
        let now = now_rfc3339();
        assert!(restore_pins_via_supersede_then_restore(&conn, old_id, &now));

        let outcome = apply_verdict(
            &conn, Verdict::Update(old_id), "a would-be update", None, None, true,
            &fake_embed("a would-be update"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::AddedRefining { old_id: o, .. } => assert_eq!(o, old_id),
            _ => panic!("expected AddedRefining, not a tombstone of the pinned row"),
        }
        assert!(!get(&conn, old_id).unwrap().unwrap().is_superseded());
    }

    /// Test helper: tombstones `id` behind a throwaway sibling row, then
    /// immediately restores it, so `id` ends up pinned and active — the
    /// exact state `mach kb restore` leaves a row in.
    fn restore_pins_via_supersede_then_restore(conn: &Connection, id: i64, now: &str) -> bool {
        let sibling = insert(conn, "throwaway", None, None, true, None, 5).unwrap();
        supersede(conn, id, sibling, now).unwrap();
        restore(conn, id, now).unwrap()
    }

    #[test]
    fn apply_verdict_supersede_blocked_by_date_guard_adds_instead() {
        // Same fail-safe shape as the pinned-row test above, but via the
        // deterministic guard instead of a pin: the loser names a date the
        // winner doesn't restate, so the classifier's SUPERSEDE must never
        // tombstone it.
        let conn = mem_conn();
        let old_id = insert5(&conn, "as of 2026-09-07 the bank held 150 memories", None);
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Supersede(old_id), "the bank holds many memories now", None, None, true,
            &fake_embed("the bank holds many memories now"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::Added { id } => assert!(get(&conn, id).unwrap().is_some()),
            _ => panic!("expected a plain Added fallback, not a tombstone of the dated loser"),
        }
        let old = get(&conn, old_id).unwrap().unwrap();
        assert!(old.invalidated_at.is_none(), "the dated loser must stay active");
        assert!(old.superseded_by.is_none());
    }

    #[test]
    fn apply_verdict_supersede_of_index_owned_loser_adds_instead() {
        // Fix-list item 6: same fail-safe shape as the pinned-row and
        // date-guard tests above, but for an index-owned loser -- the
        // classifier's SUPERSEDE verdict must never tombstone a code-index
        // summary's mirrored memory.
        let conn = mem_conn();
        let old_id = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking", None, "2026-09-24T00:00:00Z").unwrap();
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Supersede(old_id), "src/ handles picking and rendering now", None, None, true,
            &fake_embed("src/ handles picking and rendering now"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::Added { id } => assert!(get(&conn, id).unwrap().is_some()),
            _ => panic!("expected a plain Added fallback, not a tombstone of the index-owned loser"),
        }
        let old = get(&conn, old_id).unwrap().unwrap();
        assert!(old.invalidated_at.is_none(), "the index-owned loser must stay active");
        assert!(old.superseded_by.is_none());
    }

    // --- supersession_guard ---

    #[test]
    fn supersession_guard_blocks_a_dated_loser_for_every_kind() {
        let conn = mem_conn();
        let loser_id = insert5(&conn, "as of 2026-09-07 the bank held 150 memories", None);
        let winner_id = insert5(&conn, "the bank holds many memories now", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        assert!(loser.occurred_from.is_some(), "test setup: the date must have been backfilled at insert time");
        for kind in [GuardKind::Dedupe, GuardKind::Contradiction, GuardKind::AddSupersede] {
            assert_eq!(supersession_guard(&conn, &loser, &winner, kind), Some("dated fact"), "{:?}", kind);
        }
    }

    #[test]
    fn supersession_guard_undated_content_naming_a_date_also_blocks() {
        // occurred_from is NULL (no explicit occurrence set), but the loser's
        // own text names a date the winner drops -- the second half of the
        // date guard's rule, not just the occurred_from column.
        let conn = mem_conn();
        let loser_id = insert5(&conn, "shipped the fix on 2026-08-31", None);
        let winner_id = insert5(&conn, "shipped the fix", None);
        conn.execute("UPDATE memories SET occurred_from = NULL, occurred_to = NULL WHERE id = ?1", params![loser_id])
            .unwrap();
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        assert!(loser.occurred_from.is_none(), "test setup: occurred_from must be cleared");
        assert_eq!(supersession_guard(&conn, &loser, &winner, GuardKind::Dedupe), Some("dated fact"));
    }

    #[test]
    fn supersession_guard_date_present_in_both_is_not_blocked_by_the_date_guard() {
        let conn = mem_conn();
        let loser_id = insert5(&conn, "as of 2026-09-07 the bank held 150 memories", None);
        let winner_id = insert5(&conn, "as of 2026-09-07 the bank held 150 memories, restated", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        for kind in [GuardKind::Dedupe, GuardKind::Contradiction, GuardKind::AddSupersede] {
            assert_eq!(supersession_guard(&conn, &loser, &winner, kind), None, "{:?}", kind);
        }
    }

    #[test]
    fn supersession_guard_dedupe_blocks_a_short_summary_winner_over_a_detailed_loser() {
        let conn = mem_conn();
        let loser_id = insert5(
            &conn,
            "staging deploy uses postgres pgbouncer pooling flyway migrations nightly reindex vacuum",
            None,
        );
        let winner_id = insert5(&conn, "staging deploy runs smoothly", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        assert_eq!(
            supersession_guard(&conn, &loser, &winner, GuardKind::Dedupe),
            Some("winner does not carry loser's content")
        );
    }

    #[test]
    fn supersession_guard_dedupe_passes_a_true_duplicate() {
        let conn = mem_conn();
        let loser_id = insert5(
            &conn,
            "staging deploy uses postgres pgbouncer pooling flyway migrations nightly reindex vacuum",
            None,
        );
        let winner_id = insert5(
            &conn,
            "staging deploy uses postgres pgbouncer pooling flyway migrations nightly reindex vacuum, restated",
            None,
        );
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        assert_eq!(supersession_guard(&conn, &loser, &winner, GuardKind::Dedupe), None);
    }

    #[test]
    fn supersession_guard_coverage_check_is_dedupe_only() {
        // The exact short-summary-over-detailed shape that blocks Dedupe
        // above must NOT block Contradiction or AddSupersede: those two
        // never run the coverage guard at all.
        let conn = mem_conn();
        let loser_id = insert5(
            &conn,
            "staging deploy uses postgres pgbouncer pooling flyway migrations nightly reindex vacuum",
            None,
        );
        let winner_id = insert5(&conn, "staging deploy runs smoothly", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        assert_eq!(
            supersession_guard(&conn, &loser, &winner, GuardKind::Contradiction),
            None,
            "coverage guard must not apply outside Dedupe"
        );
        assert_eq!(supersession_guard(&conn, &loser, &winner, GuardKind::AddSupersede), None);
    }

    #[test]
    fn winner_term_coverage_is_full_when_loser_has_no_usable_terms() {
        let conn = mem_conn();
        let winner_id = insert5(&conn, "anything at all", None);
        assert_eq!(winner_term_coverage(&conn, "as it to a", winner_id).unwrap(), 1.0);
    }

    // --- reflection subsystem ---

    #[test]
    fn migration_v1_to_v2_creates_reflection_tables_and_preserves_rows() {
        // Build a v1-era database by hand: memories table with every v1
        // column but no insights/reflect_state tables, user_version = 1.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY,
                content TEXT NOT NULL,
                source TEXT,
                project TEXT,
                created_at TEXT NOT NULL,
                reviewed INTEGER NOT NULL DEFAULT 1,
                embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5,
                stability REAL,
                access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT,
                last_accessed_at TEXT,
                valid_from TEXT,
                invalidated_at TEXT,
                superseded_by INTEGER
            );
            INSERT INTO memories (content, created_at, valid_from, stability)
            VALUES ('a v1 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0);
            PRAGMA user_version = 1;",
        )
        .unwrap();

        // init_schema (CREATE TABLE IF NOT EXISTS) + migrate is exactly what
        // open_with_path does; drive it the same way here since we built the
        // v1 db by hand instead of through open_with_path.
        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "v1->v2 (reflection tables), v2->v3 (insights level column), v3->v4 (dormant_at), \
             v4->v5 (dedupe_seen), v5->v6 (last_verified_at + contradiction_seen), \
             v6->v7 (AUTOINCREMENT rebuild), v7->v8 (ingested_sessions), v8->v9 (graph layer), \
             v9->v10 (last_completed_at + entity_merge_seen) all run"
        );

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1, "existing memory row must survive the migration");
        assert_eq!(rows[0].content, "a v1 memory");

        let state = get_reflect_state(&conn).unwrap();
        assert!(state.last_run_at.is_none());
        assert!(state.last_memory_id.is_none());

        assert!(list_insights(&conn, false).unwrap().is_empty());

        // idempotent on repeat, like the v0->v1 migration
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    #[test]
    fn fresh_database_lands_at_current_user_version() {
        let conn = mem_conn();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn insight_insert_get_roundtrip() {
        let conn = mem_conn();
        let emb = fake_embed("insight text");
        let ids = vec!["1".to_string(), "2".to_string()];
        let id = insert_insight(&conn, "the user prefers X", 0.6, &ids, Some(&emb)).unwrap();
        let ins = get_insight(&conn, id).unwrap().expect("row exists");
        assert_eq!(ins.text, "the user prefers X");
        assert_eq!(ins.confidence, 0.6);
        assert_eq!(ins.source_ids, ids);
        assert_eq!(ins.embedding.clone().unwrap(), emb);
        assert!(ins.invalidated_at.is_none());
        assert!(ins.flagged_at.is_none());
        assert!(ins.last_verified_at.is_none());
        assert!(ins.is_active());
        assert!(!ins.is_flagged());
    }

    #[test]
    fn list_insights_flagged_only_filters_correctly() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "insight a", 0.5, &["1".into(), "2".into()], None).unwrap();
        let _b = insert_insight(&conn, "insight b", 0.5, &["3".into(), "4".into()], None).unwrap();
        flag_insight(&conn, a, &now_rfc3339()).unwrap();

        let all = list_insights(&conn, false).unwrap();
        assert_eq!(all.len(), 2);
        let flagged = list_insights(&conn, true).unwrap();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].id, a);
    }

    #[test]
    fn delete_insight_removes_row() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "gone soon", 0.5, &["1".into(), "2".into()], None).unwrap();
        assert!(delete_insight(&conn, id).unwrap());
        assert!(get_insight(&conn, id).unwrap().is_none());
        assert!(!delete_insight(&conn, id).unwrap());
    }

    #[test]
    fn insights_due_for_verification_prioritizes_never_verified_then_oldest() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "a", 0.5, &["1".into(), "2".into()], None).unwrap();
        let b = insert_insight(&conn, "b", 0.5, &["1".into(), "2".into()], None).unwrap();
        let c = insert_insight(&conn, "c", 0.5, &["1".into(), "2".into()], None).unwrap();
        // b gets verified (no longer "never verified"); a and c stay NULL
        mark_insight_verified(&conn, b, &now_rfc3339()).unwrap();

        let due = insights_due_for_verification(&conn, 5).unwrap();
        let ids: Vec<i64> = due.iter().map(|i| i.id).collect();
        // never-verified (a, c) must both sort ahead of the verified one (b)
        let pos_a = ids.iter().position(|&x| x == a).unwrap();
        let pos_b = ids.iter().position(|&x| x == b).unwrap();
        let pos_c = ids.iter().position(|&x| x == c).unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_c < pos_b);
    }

    #[test]
    fn reflect_state_defaults_to_empty_then_watermark_advances() {
        let conn = mem_conn();
        let initial = get_reflect_state(&conn).unwrap();
        assert!(initial.last_run_at.is_none());
        assert!(initial.last_memory_id.is_none());

        update_reflect_state(&conn, "2026-01-01T00:00:00Z", Some(5)).unwrap();
        let mid = get_reflect_state(&conn).unwrap();
        assert_eq!(mid.last_run_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(mid.last_memory_id, Some(5));

        update_reflect_state(&conn, "2026-01-02T00:00:00Z", Some(11)).unwrap();
        let advanced = get_reflect_state(&conn).unwrap();
        assert_eq!(advanced.last_run_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        assert_eq!(advanced.last_memory_id, Some(11), "watermark must advance, not reset");
        assert!(advanced.last_completed_at.is_none(), "update_reflect_state must never touch last_completed_at");
    }

    #[test]
    fn mark_reflect_completed_touches_only_that_column() {
        let conn = mem_conn();
        // A --meta-only run with nothing new must never clobber the
        // watermark -- setting last_completed_at first, before the
        // watermark is ever advanced, must leave it untouched.
        mark_reflect_completed(&conn, "2026-01-01T00:00:00Z").unwrap();
        let state = get_reflect_state(&conn).unwrap();
        assert_eq!(state.last_completed_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert!(state.last_run_at.is_none());
        assert!(state.last_memory_id.is_none());

        update_reflect_state(&conn, "2026-01-02T00:00:00Z", Some(9)).unwrap();
        mark_reflect_completed(&conn, "2026-01-02T00:05:00Z").unwrap();
        let after = get_reflect_state(&conn).unwrap();
        assert_eq!(after.last_completed_at.as_deref(), Some("2026-01-02T00:05:00Z"));
        assert_eq!(after.last_run_at.as_deref(), Some("2026-01-02T00:00:00Z"), "must never touch last_run_at");
        assert_eq!(after.last_memory_id, Some(9), "must never touch last_memory_id");
    }

    #[test]
    fn memories_since_returns_only_newer_active_rows_ascending() {
        let conn = mem_conn();
        let a = insert5(&conn, "one", None);
        let b = insert5(&conn, "two", None);
        let c = insert5(&conn, "three", None);
        let d_id = insert5(&conn, "four (will be tombstoned)", None);
        supersede(&conn, d_id, c, &now_rfc3339()).unwrap();

        let since_a = memories_since(&conn, a).unwrap();
        let ids: Vec<i64> = since_a.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![b, c], "tombstoned row must be excluded, order ascending");
    }

    #[test]
    fn top_similar_active_includes_unreviewed_unlike_top_similar() {
        let conn = mem_conn();
        let emb = fake_embed("shared reflection topic");
        insert5(&conn, "reviewed shared reflection topic", Some(&emb));
        insert(&conn, "unreviewed shared reflection topic", None, None, false, Some(&emb), 5).unwrap();

        let reviewed_only = top_similar(&conn, &emb, 10).unwrap();
        assert_eq!(reviewed_only.len(), 1);

        let active_all = top_similar_active(&conn, &emb, 10).unwrap();
        assert_eq!(active_all.len(), 2, "top_similar_active must bridge into unreviewed rows too");
    }

    #[test]
    fn search_insights_ranked_favors_high_similarity() {
        let conn = mem_conn();
        insert_insight(
            &conn, "the user prefers dark mode", 0.5, &["1".into(), "2".into()],
            Some(&fake_embed("the user prefers dark mode")),
        )
        .unwrap();
        insert_insight(
            &conn, "the user likes hiking", 0.5, &["3".into(), "4".into()],
            Some(&fake_embed("the user likes hiking")),
        )
        .unwrap();

        let now = now_rfc3339();
        let query = fake_embed("does the user prefer dark mode");
        let hits = search_insights_ranked(&conn, &query, 5, &now).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].insight.text, "the user prefers dark mode");
    }

    #[test]
    fn search_insights_ranked_excludes_invalidated() {
        let conn = mem_conn();
        let emb = fake_embed("temporary belief");
        let id = insert_insight(&conn, "temporary belief", 0.5, &["1".into(), "2".into()], Some(&emb)).unwrap();
        conn.execute("UPDATE insights SET invalidated_at = ?1 WHERE id = ?2", params![now_rfc3339(), id])
            .unwrap();

        let now = now_rfc3339();
        let hits = search_insights_ranked(&conn, &emb, 5, &now).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn touch_only_ever_targets_memories_table_insights_unaffected() {
        // Insights have no access_count/stability columns at all, so a
        // `--touch` call is structurally incapable of reinforcing them —
        // this pins that invariant: touching a memory id must never change
        // an insight, even one that happens to share the same integer id
        // (the two tables have independent id sequences).
        let conn = mem_conn();
        let mem_id = insert5(&conn, "touchable memory", None);
        let insight = insert_insight(&conn, "untouchable insight", 0.5, &["1".into(), "2".into()], None).unwrap();
        let now = now_rfc3339();
        touch(&conn, &[mem_id, insight], &now).unwrap();

        let ins = get_insight(&conn, insight).unwrap().unwrap();
        assert!(ins.last_verified_at.is_none());
        assert!(ins.flagged_at.is_none());
    }

    // --- meta-reflection: level-2 themes ---

    #[test]
    fn insert_insight_defaults_to_level_1_insert_theme_to_level_2() {
        let conn = mem_conn();
        let ins_id = insert_insight(&conn, "a plain insight", 0.5, &["1".into(), "2".into()], None).unwrap();
        let theme_id = insert_theme(&conn, "a unifying theme", 0.5, &[format!("i{}", ins_id), "3".into()], None).unwrap();

        let ins = get_insight(&conn, ins_id).unwrap().unwrap();
        assert_eq!(ins.level, 1);
        assert!(!ins.is_theme());

        let theme = get_insight(&conn, theme_id).unwrap().unwrap();
        assert_eq!(theme.level, 2);
        assert!(theme.is_theme());
    }

    #[test]
    fn migration_v2_to_v3_backfills_level_column_defaulting_existing_rows_to_1() {
        // Build a v2-era database by hand: insights table without the
        // `level` column, user_version = 2.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            INSERT INTO insights (text, created_at, confidence, source_ids)
            VALUES ('a pre-existing insight', '2026-01-01T00:00:00Z', 0.6, '[\"1\",\"2\"]');
            PRAGMA user_version = 2;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "v2->v3 (level column), v3->v4 (dormant_at), v4->v5 (dedupe_seen), v5->v6, \
             v6->v7 (AUTOINCREMENT rebuild), v7->v8 (ingested_sessions), v8->v9, v9->v10 all run"
        );

        let rows = list_insights(&conn, false).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].level, 1, "pre-existing insight must backfill to level 1");

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list_insights(&conn, false).unwrap().len(), 1);
    }

    #[test]
    fn migration_v3_to_v4_backfills_dormant_at_column_and_keeps_rows_active() {
        // Build a v3-era database by hand: memories table without the
        // `dormant_at` column, user_version = 3.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            INSERT INTO memories (content, created_at, valid_from, stability)
            VALUES ('a v3 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0);
            PRAGMA user_version = 3;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "v3->v4 (dormant_at), v4->v5 (dedupe_seen), v5->v6, v6->v7 (AUTOINCREMENT rebuild), \
             v7->v8 (ingested_sessions), v8->v9, v9->v10 all run"
        );

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1, "existing memory row must survive the migration");
        assert!(rows[0].dormant_at.is_none(), "pre-existing row must backfill to active (not dormant)");
        assert!(!rows[0].is_dormant());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    #[test]
    fn migration_v4_to_v5_creates_dedupe_seen_table_and_keeps_rows() {
        // Build a v4-era database by hand: every table up through
        // `dormant_at`, but no `dedupe_seen` table, user_version = 4.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            INSERT INTO memories (content, created_at, valid_from, stability)
            VALUES ('a v4 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0);
            PRAGMA user_version = 4;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "v4->v5 (dedupe_seen), v5->v6, v6->v7 (AUTOINCREMENT rebuild), then v7->v8 \
             (ingested_sessions), v8->v9, v9->v10 all run"
        );

        // The table exists and behaves — round-trips through the store
        // functions that use it.
        mark_dedupe_seen(&conn, 3, 1).unwrap();
        assert_eq!(dedupe_seen_pairs(&conn).unwrap(), [(1, 3)].into_iter().collect());

        assert_eq!(list(&conn, None, false).unwrap().len(), 1, "existing memory row must survive the migration");

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    #[test]
    fn migration_v5_to_v6_backfills_last_verified_at_and_creates_contradiction_seen() {
        // Build a v5-era database by hand: every table up through
        // `dedupe_seen`, but `memories` has no `last_verified_at` column and
        // there's no `contradiction_seen` table yet, user_version = 5.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            CREATE TABLE dedupe_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            INSERT INTO memories (content, created_at, valid_from, stability, importance)
            VALUES ('a v5 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0, 7);
            PRAGMA user_version = 5;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "v5->v6, v6->v7 (AUTOINCREMENT rebuild), then v7->v8 (ingested_sessions) all run");

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1, "existing memory row must survive the migration");
        assert!(rows[0].last_verified_at.is_none(), "pre-existing row must backfill to never-verified (NULL)");

        // The new table exists and behaves — round-trips through the store
        // functions that use it.
        mark_contradiction_seen(&conn, 9, 4).unwrap();
        assert_eq!(contradiction_seen_pairs(&conn).unwrap(), [(4, 9)].into_iter().collect());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    /// Builds a v6-era database by hand: every table exactly as
    /// `migrate_v5_to_v6` leaves it (plain `INTEGER PRIMARY KEY` on both
    /// `memories` and `insights`, no AUTOINCREMENT), user_version = 6. Used
    /// by every v6->v7 test below so each one only has to state what it's
    /// actually checking.
    fn v6_era_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            CREATE TABLE dedupe_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE contradiction_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            PRAGMA user_version = 6;",
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_v6_to_v7_rebuilds_with_autoincrement_and_preserves_rows_and_data() {
        let conn = v6_era_db();
        conn.execute_batch(
            "INSERT INTO memories (id, content, source, project, created_at, reviewed, importance, stability, valid_from)
             VALUES (5, 'a v6 memory', 'src', 'proj', '2026-01-01T00:00:00Z', 1, 5, 35.0, '2026-01-01T00:00:00Z'),
                    (133, 'gap-numbered v6 memory', NULL, NULL, '2026-02-01T00:00:00Z', 1, 5, 35.0, '2026-02-01T00:00:00Z');
             INSERT INTO insights (id, text, created_at, confidence, source_ids)
             VALUES (7, 'a v6 insight', '2026-01-01T00:00:00Z', 0.6, '[\"5\"]');",
        )
        .unwrap();

        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "v6->v7 (AUTOINCREMENT rebuild), then v7->v8 (ingested_sessions) both run");

        // Every row and its data survive the rebuild, ids included.
        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 2);
        let m5 = get(&conn, 5).unwrap().unwrap();
        assert_eq!(m5.content, "a v6 memory");
        assert_eq!(m5.source.as_deref(), Some("src"));
        assert_eq!(m5.project.as_deref(), Some("proj"));
        let m133 = get(&conn, 133).unwrap().unwrap();
        assert_eq!(m133.content, "gap-numbered v6 memory");

        let insights = list_insights(&conn, false).unwrap();
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].id, 7);
        assert_eq!(insights[0].text, "a v6 insight");

        // The table really is AUTOINCREMENT now: sqlite_master's own DDL
        // says so directly, for both tables.
        let mem_sql: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'", [], |r| r.get(0))
            .unwrap();
        assert!(mem_sql.contains("AUTOINCREMENT"));
        let ins_sql: String = conn
            .query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name='insights'", [], |r| r.get(0))
            .unwrap();
        assert!(ins_sql.contains("AUTOINCREMENT"));

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 2);
    }

    #[test]
    fn migration_v6_to_v7_purges_seen_pairs_citing_ids_no_longer_in_memories() {
        // A dangling pair like this is exactly what the reused-rowid bug
        // could produce pre-migration: a forgotten id sitting in
        // dedupe_seen/contradiction_seen while some other still-live row
        // now uses that same id. Nothing in `memories` may be trusted to
        // still hold either historical id, so the pair must simply go.
        let conn = v6_era_db();
        conn.execute_batch(
            "INSERT INTO memories (id, content, created_at, valid_from) VALUES (1, 'still here', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO dedupe_seen (id_a, id_b) VALUES (1, 99);
             INSERT INTO contradiction_seen (id_a, id_b) VALUES (1, 99);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert!(dedupe_seen_pairs(&conn).unwrap().is_empty(), "pair cites id 99, which no longer exists");
        assert!(contradiction_seen_pairs(&conn).unwrap().is_empty(), "pair cites id 99, which no longer exists");
    }

    #[test]
    fn migration_v6_to_v7_keeps_seen_pairs_whose_ids_both_still_exist() {
        let conn = v6_era_db();
        conn.execute_batch(
            "INSERT INTO memories (id, content, created_at, valid_from) VALUES
                (1, 'first', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                (2, 'second', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             INSERT INTO dedupe_seen (id_a, id_b) VALUES (1, 2);
             INSERT INTO contradiction_seen (id_a, id_b) VALUES (1, 2);",
        )
        .unwrap();

        migrate(&conn).unwrap();

        assert_eq!(dedupe_seen_pairs(&conn).unwrap(), [(1, 2)].into_iter().collect());
        assert_eq!(contradiction_seen_pairs(&conn).unwrap(), [(1, 2)].into_iter().collect());
    }

    #[test]
    fn migration_v6_to_v7_then_deleting_the_highest_row_and_inserting_never_reuses_its_id() {
        // The exact bug this whole migration exists to fix: pre-migration, a
        // plain INTEGER PRIMARY KEY reissues a deleted max-id row's id to the
        // very next insert. Post-migration this must be structurally
        // impossible, not just improbable.
        let conn = v6_era_db();
        conn.execute_batch(
            "INSERT INTO memories (id, content, created_at, valid_from) VALUES
                (133, 'a', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                (134, 'b', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                (135, 'c', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'),
                (136, 'd', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
        )
        .unwrap();
        migrate(&conn).unwrap();

        // Delete every row from the historical high-water mark down --
        // mirrors the live bug report's own repro (rows 133-136 forgotten).
        for id in [133, 134, 135, 136] {
            assert!(delete(&conn, id).unwrap());
        }
        assert!(list(&conn, None, false).unwrap().is_empty());

        let new_id = insert(&conn, "a fresh memory", None, None, true, None, 5).unwrap();
        assert!(new_id > 136, "the new id ({}) must exceed every historical id, not reuse one", new_id);

        // Same guarantee for insights: id 7 was the highest pre-migration
        // insight id; deleting it must not free it up for reuse either.
        let ins_conn = v6_era_db();
        ins_conn
            .execute(
                "INSERT INTO insights (id, text, created_at, confidence, source_ids) VALUES (7, 'x', '2026-01-01T00:00:00Z', 0.5, '[]')",
                [],
            )
            .unwrap();
        migrate(&ins_conn).unwrap();
        assert!(delete_insight(&ins_conn, 7).unwrap());
        let new_insight_id = insert_insight(&ins_conn, "fresh insight", 0.5, &["1".into(), "2".into()], None).unwrap();
        assert!(new_insight_id > 7, "the new insight id ({}) must exceed the deleted historical max", new_insight_id);
    }

    #[test]
    fn migration_v6_to_v7_sqlite_sequence_starts_at_or_above_the_historical_max_id() {
        let conn = v6_era_db();
        conn.execute(
            "INSERT INTO memories (id, content, created_at, valid_from) VALUES (250, 'x', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        migrate(&conn).unwrap();

        let seq: i64 = conn
            .query_row("SELECT seq FROM sqlite_sequence WHERE name = 'memories'", [], |r| r.get(0))
            .unwrap();
        assert!(seq >= 250, "sqlite_sequence ({}) must be seeded from the highest id this table ever held", seq);
    }

    #[test]
    fn migration_v7_to_v8_creates_ingested_sessions_table_and_keeps_rows() {
        // Build a v7-era database by hand: current memories/insights shape,
        // but no `ingested_sessions` table yet, user_version = 7.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            CREATE TABLE dedupe_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE contradiction_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            INSERT INTO memories (content, created_at, valid_from, stability)
            VALUES ('a v7 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0);
            PRAGMA user_version = 7;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "v7->v8 (ingested_sessions) runs");

        assert_eq!(list(&conn, None, false).unwrap().len(), 1, "existing memory row must survive the migration");

        // The table exists and behaves — round-trips through the store
        // functions that use it.
        assert!(!is_session_ingested(&conn, "abc-123").unwrap());
        mark_session_ingested(&conn, "abc-123", &now_rfc3339()).unwrap();
        assert!(is_session_ingested(&conn, "abc-123").unwrap());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    #[test]
    fn mark_session_ingested_is_idempotent() {
        let conn = mem_conn();
        let now = now_rfc3339();
        mark_session_ingested(&conn, "sess-1", &now).unwrap();
        mark_session_ingested(&conn, "sess-1", &now).unwrap(); // INSERT OR IGNORE — must not error
        assert!(is_session_ingested(&conn, "sess-1").unwrap());
        assert!(!is_session_ingested(&conn, "sess-2").unwrap(), "a different session id must be unaffected");
    }

    #[test]
    fn delete_purges_dedupe_and_contradiction_seen_pairs_citing_the_forgotten_id() {
        let conn = mem_conn();
        let a = insert5(&conn, "memory a", None);
        let b = insert5(&conn, "memory b", None);
        let c = insert5(&conn, "memory c", None);
        mark_dedupe_seen(&conn, a, b).unwrap();
        mark_contradiction_seen(&conn, a, c).unwrap();
        // A pair not touching `a` at all must survive untouched.
        mark_dedupe_seen(&conn, b, c).unwrap();

        assert!(delete(&conn, a).unwrap());

        let (lo, hi) = if b < c { (b, c) } else { (c, b) };
        assert_eq!(dedupe_seen_pairs(&conn).unwrap(), [(lo, hi)].into_iter().collect(), "the a-b pair must be purged, b-c must survive");
        assert!(contradiction_seen_pairs(&conn).unwrap().is_empty(), "the a-c pair must be purged");
    }

    #[test]
    fn delete_of_nonexistent_id_touches_no_seen_pairs() {
        let conn = mem_conn();
        let a = insert5(&conn, "memory a", None);
        let b = insert5(&conn, "memory b", None);
        mark_dedupe_seen(&conn, a, b).unwrap();

        assert!(!delete(&conn, 999_999).unwrap());
        assert_eq!(dedupe_seen_pairs(&conn).unwrap().len(), 1, "an unrelated delete must not purge anything");
    }

    #[test]
    fn count_active_insights_level_filters_by_level_and_excludes_invalidated() {
        let conn = mem_conn();
        insert_insight(&conn, "a", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "b", 0.5, &["1".into(), "2".into()], None).unwrap();
        let invalidated_id = insert_insight(&conn, "c", 0.5, &["1".into(), "2".into()], None).unwrap();
        conn.execute(
            "UPDATE insights SET invalidated_at = ?1 WHERE id = ?2",
            params![now_rfc3339(), invalidated_id],
        )
        .unwrap();
        insert_theme(&conn, "a theme", 0.5, &["i1".into(), "i2".into(), "3".into()], None).unwrap();

        assert_eq!(count_active_insights_level(&conn, 1).unwrap(), 2, "invalidated row excluded");
        assert_eq!(count_active_insights_level(&conn, 2).unwrap(), 1);
    }

    #[test]
    fn newest_active_theme_is_none_until_one_exists_then_tracks_highest_id() {
        let conn = mem_conn();
        assert!(newest_active_theme(&conn).unwrap().is_none());

        let _first = insert_theme(&conn, "first theme", 0.5, &["i1".into(), "i2".into(), "3".into()], None).unwrap();
        let second = insert_theme(&conn, "second theme", 0.5, &["i1".into(), "i2".into(), "3".into()], None).unwrap();

        let newest = newest_active_theme(&conn).unwrap().expect("a theme exists");
        assert_eq!(newest.id, second);
    }

    #[test]
    fn count_active_level1_created_after_counts_only_newer_ids() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "a", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "b", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "c", 0.5, &["1".into(), "2".into()], None).unwrap();

        assert_eq!(count_active_level1_created_after(&conn, a).unwrap(), 2);
        assert_eq!(count_active_level1_created_after(&conn, 0).unwrap(), 3);
    }

    #[test]
    fn active_insights_by_level_separates_insights_from_themes() {
        let conn = mem_conn();
        insert_insight(&conn, "a", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "b", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_theme(&conn, "a theme", 0.5, &["i1".into(), "i2".into(), "3".into()], None).unwrap();

        assert_eq!(active_insights_by_level(&conn, 1).unwrap().len(), 2);
        assert_eq!(active_insights_by_level(&conn, 2).unwrap().len(), 1);
    }

    #[test]
    fn themed_insight_ids_collects_i_refs_from_active_themes_only() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "a", 0.5, &["1".into(), "2".into()], None).unwrap();
        let b = insert_insight(&conn, "b", 0.5, &["1".into(), "2".into()], None).unwrap();
        let c = insert_insight(&conn, "c", 0.5, &["1".into(), "2".into()], None).unwrap();
        insert_theme(&conn, "theme 1", 0.5, &[format!("i{}", a), format!("i{}", b), "5".into()], None).unwrap();
        let invalidated_theme =
            insert_theme(&conn, "theme 2", 0.5, &[format!("i{}", c), "i1".into(), "6".into()], None).unwrap();
        conn.execute(
            "UPDATE insights SET invalidated_at = ?1 WHERE id = ?2",
            params![now_rfc3339(), invalidated_theme],
        )
        .unwrap();

        let themed = themed_insight_ids(&conn).unwrap();
        assert!(themed.contains(&a));
        assert!(themed.contains(&b));
        assert!(!themed.contains(&c), "c is only cited by an invalidated theme");
    }

    #[test]
    fn mental_model_is_empty_for_an_empty_store() {
        let conn = mem_conn();
        assert!(mental_model(&conn).unwrap().is_empty());
    }

    #[test]
    fn mental_model_ranks_themes_and_unthemed_insights_together_but_keeps_nesting() {
        // Updated for Task 4: themes no longer sort ahead of every belief
        // by construction -- everything created "now" has ~equal recency,
        // so the merged top-level list falls back to confidence, and the
        // 0.7 unthemed insight now outranks the 0.55 theme. A theme's own
        // nested insights are still ranked among themselves by confidence
        // (insight b's 0.6 over insight a's 0.5) and still immediately
        // follow their theme, wherever that theme lands.
        let conn = mem_conn();
        let a = insert_insight(&conn, "insight a", 0.5, &["1".into(), "2".into()], None).unwrap();
        let b = insert_insight(&conn, "insight b", 0.6, &["1".into(), "2".into()], None).unwrap();
        let unthemed = insert_insight(&conn, "unthemed insight", 0.7, &["1".into(), "2".into()], None).unwrap();
        insert_theme(&conn, "a theme", 0.55, &[format!("i{}", a), format!("i{}", b), "3".into()], None).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 4, "1 theme + 2 nested insights + 1 unthemed insight");

        assert_eq!(rows[0].kind, ModelKind::Belief);
        assert_eq!(rows[0].text, "unthemed insight");
        assert!(!rows[0].nested, "not folded into any theme");
        assert_eq!(rows[0].confidence, 0.7);

        assert_eq!(rows[1].kind, ModelKind::Theme);
        assert_eq!(rows[1].text, "a theme");
        assert!(!rows[1].nested);

        assert_eq!(rows[2].kind, ModelKind::Belief);
        assert_eq!(rows[2].text, "insight b");
        assert!(rows[2].nested, "insight b is nested under its theme, immediately after it");

        assert_eq!(rows[3].kind, ModelKind::Belief);
        assert_eq!(rows[3].text, "insight a");
        assert!(rows[3].nested, "insight a is nested under its theme");
        let _ = unthemed; // id only asserted via ordering/content above
    }

    #[test]
    fn mental_model_ranks_a_recent_high_confidence_belief_ahead_of_an_old_low_confidence_theme() {
        let conn = mem_conn();
        let theme_id = insert_theme(&conn, "old theme", 0.5, &["1".into(), "2".into()], None).unwrap();
        let old_at = now_rfc3339_from_secs(now_secs() - 60 * 86400);
        conn.execute("UPDATE insights SET created_at = ?1 WHERE id = ?2", params![old_at, theme_id]).unwrap();

        insert_insight(&conn, "fresh belief", 0.9, &["3".into(), "4".into()], None).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "fresh belief", "a new 0.9 belief must outrank an old 0.5 theme");
        assert_eq!(rows[1].text, "old theme");
    }

    #[test]
    fn model_score_ignores_last_verified_at_for_recency() {
        // Fix-list item 4: recency uses max(created_at, revised_at) only.
        // Two insights, both created 60 days ago and never revised, but one
        // was just re-verified (last_verified_at = now) and the other was
        // never verified at all -- they must score identically, since a
        // routine "still holds" check is not "recent learning."
        let old_at = now_rfc3339_from_secs(now_secs() - 60 * 86400);
        let now_ts = now_rfc3339_from_secs(now_secs());
        let base = Insight {
            id: 1,
            text: "an old belief".to_string(),
            created_at: old_at.clone(),
            confidence: 0.6,
            source_ids: vec!["1".into(), "2".into()],
            embedding: None,
            invalidated_at: None,
            flagged_at: None,
            last_verified_at: None,
            level: 1,
            revised_at: None,
            prev_text: None,
        };
        let just_reverified = Insight { last_verified_at: Some(now_ts.clone()), ..base.clone() };
        let never_verified = Insight { last_verified_at: None, ..base };

        let now = parse_rfc3339(&now_ts).unwrap();
        assert_eq!(
            model_score(&just_reverified, now),
            model_score(&never_verified, now),
            "last_verified_at alone must not change the recency score"
        );

        // A REVISE, by contrast, DOES move the score -- revised_at is one
        // of the two timestamps recency is computed from.
        let just_revised = Insight { revised_at: Some(now_ts), last_verified_at: None, ..just_reverified.clone() };
        assert!(
            model_score(&just_revised, now) > model_score(&never_verified, now),
            "revised_at, unlike last_verified_at, must boost the score"
        );
    }

    #[test]
    fn mental_model_orders_non_doubted_high_confidence_first() {
        let conn = mem_conn();
        let low_flagged = insert_insight(&conn, "shaky", 0.3, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "highest", 0.9, &["1".into(), "2".into()], None).unwrap();
        insert_insight(&conn, "middle", 0.5, &["1".into(), "2".into()], None).unwrap();
        let now = now_rfc3339();
        flag_insight(&conn, low_flagged, &now).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "highest");
        assert_eq!(rows[1].text, "middle");
        assert_eq!(rows[2].text, "shaky");
        assert!(rows[2].doubted, "flagged row sorts last despite none of these being themed");
    }

    #[test]
    fn mental_model_marks_flagged_rows_as_doubted_at_both_levels() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "a flagged insight", 0.5, &["1".into(), "2".into()], None).unwrap();
        let theme_id =
            insert_theme(&conn, "a flagged theme", 0.5, &[format!("i{}", a), "3".into()], None).unwrap();
        let now = now_rfc3339();
        flag_insight(&conn, a, &now).unwrap();
        flag_insight(&conn, theme_id, &now).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].doubted, "theme was flagged");
        assert!(rows[1].doubted, "nested insight was flagged");
    }

    #[test]
    fn mental_model_excludes_invalidated_insights_and_themes() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "still active", 0.5, &["1".into(), "2".into()], None).unwrap();
        let gone = insert_insight(&conn, "invalidated", 0.5, &["1".into(), "2".into()], None).unwrap();
        let now = now_rfc3339();
        conn.execute("UPDATE insights SET invalidated_at = ?1 WHERE id = ?2", params![now, gone]).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "still active");
        let _ = a;
    }

    #[test]
    fn reinforce_insight_appends_new_ids_dedupes_bumps_confidence_and_sets_verified() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "the user prefers X", 0.5, &["1".into(), "2".into()], None).unwrap();
        let now = now_rfc3339();

        assert!(reinforce_insight(&conn, id, &[3, 1], &now).unwrap(), "1 already cited, 3 is new");
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["1", "2", "3"], "no duplicate id, new one appended");
        assert!((ins.confidence - 0.55).abs() < 1e-9, "+0.05 per NEW row — only 1 of the 2 cited ids was new");
        assert_eq!(ins.last_verified_at.as_deref(), Some(now.as_str()));
    }

    #[test]
    fn reinforce_insight_confidence_caps_at_point_nine() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "heavily reinforced", 0.8, &["1".into(), "2".into()], None).unwrap();
        let now = now_rfc3339();
        assert!(reinforce_insight(&conn, id, &[3, 4, 5, 6], &now).unwrap());
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.confidence, 0.9, "0.8 + 4*0.05 = 1.0, must cap at 0.9");
    }

    #[test]
    fn reinforce_insight_missing_id_returns_false() {
        let conn = mem_conn();
        assert!(!reinforce_insight(&conn, 999, &[1, 2], &now_rfc3339()).unwrap());
    }

    // --- nightly dedupe pass ---

    #[test]
    fn mark_dedupe_seen_and_dedupe_seen_pairs_roundtrip_normalized() {
        let conn = mem_conn();
        mark_dedupe_seen(&conn, 5, 2).unwrap(); // reversed order on input
        let seen = dedupe_seen_pairs(&conn).unwrap();
        assert_eq!(seen, [(2, 5)].into_iter().collect());
    }

    #[test]
    fn mark_dedupe_seen_is_idempotent() {
        let conn = mem_conn();
        mark_dedupe_seen(&conn, 2, 5).unwrap();
        mark_dedupe_seen(&conn, 2, 5).unwrap();
        mark_dedupe_seen(&conn, 5, 2).unwrap(); // same pair, reversed
        assert_eq!(dedupe_seen_pairs(&conn).unwrap().len(), 1);
    }

    #[test]
    fn merge_and_supersede_sums_access_count_and_takes_max_stability() {
        let conn = mem_conn();
        let winner = insert(&conn, "the user likes tea", None, None, true, None, 5).unwrap();
        let loser = insert(&conn, "the user is fond of tea", None, None, true, None, 5).unwrap();
        // Backdate both rows so a touch is a genuinely spaced repetition
        // (elapsed >= stability) rather than a same-instant no-op under
        // the interval-aware gain.
        let created = now_rfc3339_from_secs(now_secs() - 100 * 86400);
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![created, winner]).unwrap();
        conn.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", params![created, loser]).unwrap();

        let now = now_rfc3339();
        touch(&conn, &[winner], &now).unwrap(); // access_count 1, stability 35*1.3=45.5
        touch(&conn, &[loser], &now).unwrap(); // access_count 1, same growth as winner so far
        // A second touch after another gap at least as long as the loser's
        // current stability earns another full 1.3x, pushing it decisively
        // above the winner's.
        let later_secs = parse_rfc3339(&now).unwrap() + 60 * 86400;
        let later = now_rfc3339_from_secs(later_secs as u64);
        touch(&conn, &[loser], &later).unwrap(); // access_count 2, stability higher than winner's

        let winner_before = get(&conn, winner).unwrap().unwrap();
        let loser_before = get(&conn, loser).unwrap().unwrap();
        assert!(loser_before.effective_stability() > winner_before.effective_stability());

        assert!(merge_and_supersede(&conn, loser, winner, &now).unwrap());
        let winner_after = get(&conn, winner).unwrap().unwrap();
        assert_eq!(
            winner_after.access_count,
            winner_before.access_count + loser_before.access_count,
            "winner absorbs the loser's access_count (summed)"
        );
        assert_eq!(
            winner_after.effective_stability(),
            loser_before.effective_stability(),
            "stability becomes the max of the two — here the loser's"
        );
    }

    #[test]
    fn merge_and_supersede_tombstones_the_loser() {
        let conn = mem_conn();
        let winner = insert(&conn, "fact A", None, None, true, None, 5).unwrap();
        let loser = insert(&conn, "fact A, restated", None, None, true, None, 5).unwrap();
        let now = now_rfc3339();
        assert!(merge_and_supersede(&conn, loser, winner, &now).unwrap());
        let loser_after = get(&conn, loser).unwrap().unwrap();
        assert!(loser_after.is_superseded());
        assert_eq!(loser_after.superseded_by, Some(winner));
        let winner_after = get(&conn, winner).unwrap().unwrap();
        assert!(!winner_after.is_superseded());
    }

    #[test]
    fn merge_and_supersede_is_a_noop_when_loser_missing_or_already_gone() {
        let conn = mem_conn();
        let winner = insert(&conn, "fact A", None, None, true, None, 5).unwrap();
        let now = now_rfc3339();
        assert!(!merge_and_supersede(&conn, 999, winner, &now).unwrap(), "loser id doesn't exist");

        let loser = insert(&conn, "fact A, restated", None, None, true, None, 5).unwrap();
        let other = insert(&conn, "fact A, restated again", None, None, true, None, 5).unwrap();
        assert!(supersede(&conn, loser, other, &now).unwrap()); // already tombstoned by something else
        assert!(!merge_and_supersede(&conn, loser, winner, &now).unwrap(), "already-tombstoned loser is a no-op");
    }

    #[test]
    fn merge_and_supersede_is_a_noop_when_winner_missing() {
        let conn = mem_conn();
        let loser = insert(&conn, "fact A", None, None, true, None, 5).unwrap();
        assert!(!merge_and_supersede(&conn, loser, 999, &now_rfc3339()).unwrap());
        assert!(!get(&conn, loser).unwrap().unwrap().is_superseded(), "never tombstoned against a missing winner");
    }

    #[test]
    fn repoint_insight_citations_rewrites_matching_raw_ids_only() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "an insight", 0.6, &["3".into(), "12".into(), "i2".into()], None).unwrap();
        let updated = repoint_insight_citations(&conn, 12, 40, 1).unwrap();
        assert_eq!(updated, 1);
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["3", "40", "i2"], "only the raw id 12 is rewritten, i2 left alone");
    }

    #[test]
    fn repoint_insight_citations_dedupes_when_winner_already_cited() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "an insight", 0.6, &["12".into(), "40".into()], None).unwrap();
        let updated = repoint_insight_citations(&conn, 12, 40, 1).unwrap();
        assert_eq!(updated, 1);
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["40"], "12 rewritten to 40 collapses with the already-cited 40");
    }

    #[test]
    fn repoint_insight_citations_leaves_non_citing_insights_untouched() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "unrelated insight", 0.6, &["7".into(), "8".into()], None).unwrap();
        let updated = repoint_insight_citations(&conn, 12, 40, 1).unwrap();
        assert_eq!(updated, 0);
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["7", "8"]);
    }

    // --- dormancy (forgetting) ---

    /// Builds a `Memory` fixture with fields the dormancy criteria actually
    /// read set directly (bypassing `insert`, which can't backdate
    /// `created_at`/`last_accessed_at`) — everything else at harmless
    /// defaults. Not persisted to any connection; `memory_qualifies_for_dormancy`
    /// is pure over its `Memory` argument.
    fn dormancy_fixture(
        age_days: f64,
        access_count: i64,
        last_accessed_days_ago: Option<f64>,
        importance: i64,
        reviewed: bool,
    ) -> Memory {
        let now = now_secs() as i64;
        let created_at = now_rfc3339_from_secs((now as f64 - age_days * 86400.0) as u64);
        let last_accessed_at = last_accessed_days_ago
            .map(|d| now_rfc3339_from_secs((now as f64 - d * 86400.0) as u64));
        Memory {
            id: 1,
            content: "fixture".to_string(),
            source: None,
            project: None,
            created_at: created_at.clone(),
            reviewed,
            embedding: None,
            importance,
            stability: None,
            access_count,
            first_accessed_at: last_accessed_at.clone(),
            last_accessed_at,
            valid_from: created_at,
            invalidated_at: None,
            superseded_by: None,
            dormant_at: None,
            last_verified_at: None,
            graph_extracted_at: None,
            basis: None,
            occurred_from: None,
            occurred_to: None,
            pinned_at: None,
        }
    }

    #[test]
    fn dormancy_qualifies_when_all_four_ordinary_criteria_hold() {
        // 100 days old, never touched, importance 5 (<=5), uncited.
        let m = dormancy_fixture(100.0, 0, None, 5, true);
        assert!(memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_qualifies_when_touched_but_stale() {
        // 100 days old, touched once but 70 days ago (> 60-day staleness).
        let m = dormancy_fixture(100.0, 3, Some(70.0), 5, true);
        assert!(memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_rejects_when_too_young() {
        // Only 80 days old — under the 90-day floor — regardless of everything else.
        let m = dormancy_fixture(80.0, 0, None, 5, true);
        assert!(!memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_rejects_when_recently_touched() {
        // Old enough, but touched only 10 days ago (well under the 60-day floor).
        let m = dormancy_fixture(100.0, 5, Some(10.0), 5, true);
        assert!(!memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_rejects_when_importance_too_high() {
        let m = dormancy_fixture(100.0, 0, None, 8, true);
        assert!(!memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_rejects_when_cited_by_an_active_insight() {
        let m = dormancy_fixture(100.0, 0, None, 5, true);
        assert!(!memory_qualifies_for_dormancy(&m, true, &now_rfc3339()));
    }

    #[test]
    fn dormancy_importance_boundary_at_five_is_inclusive() {
        let at_floor = dormancy_fixture(100.0, 0, None, 5, true);
        assert!(memory_qualifies_for_dormancy(&at_floor, false, &now_rfc3339()), "importance == 5 must qualify");
        let above_floor = dormancy_fixture(100.0, 0, None, 6, true);
        assert!(!memory_qualifies_for_dormancy(&above_floor, false, &now_rfc3339()), "importance == 6 must not");
    }

    #[test]
    fn dormancy_unreviewed_row_young_and_engaged_does_not_qualify_merely_for_being_unreviewed() {
        // Only 35 days old (well under the 90-day floor), touched recently,
        // importance 10, cited — every ordinary criterion says "no", and
        // there is no longer an absolute unreviewed-age rule to override
        // them: an unreviewed row gets no special treatment here at all.
        let m = dormancy_fixture(35.0, 5, Some(1.0), 10, false);
        assert!(!memory_qualifies_for_dormancy(&m, true, &now_rfc3339()), "unreviewed alone must never force dormancy");
    }

    #[test]
    fn dormancy_unreviewed_row_meeting_the_ordinary_criteria_qualifies_same_as_a_reviewed_one() {
        // Old, never touched, low importance, uncited — the ordinary rule's
        // full set — reviewed status must be irrelevant to the outcome.
        let unreviewed = dormancy_fixture(100.0, 0, None, 5, false);
        let reviewed = dormancy_fixture(100.0, 0, None, 5, true);
        let now = now_rfc3339();
        assert!(memory_qualifies_for_dormancy(&unreviewed, false, &now), "unreviewed must qualify on the same ordinary terms as reviewed");
        assert!(memory_qualifies_for_dormancy(&reviewed, false, &now));
    }

    #[test]
    fn dormancy_old_low_importance_uncited_row_under_90_days_old_still_needs_the_90_day_floor() {
        // Reviewed or not, must fall through to the ordinary >=90-day check
        // and fail it — no shortcut for either.
        let reviewed = dormancy_fixture(20.0, 0, None, 3, true);
        let unreviewed = dormancy_fixture(20.0, 0, None, 3, false);
        assert!(!memory_qualifies_for_dormancy(&reviewed, false, &now_rfc3339()));
        assert!(!memory_qualifies_for_dormancy(&unreviewed, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_already_dormant_or_superseded_never_requalifies() {
        let mut dormant = dormancy_fixture(100.0, 0, None, 5, true);
        dormant.dormant_at = Some(now_rfc3339());
        assert!(!memory_qualifies_for_dormancy(&dormant, false, &now_rfc3339()));

        let mut superseded = dormancy_fixture(100.0, 0, None, 5, true);
        superseded.invalidated_at = Some(now_rfc3339());
        assert!(!memory_qualifies_for_dormancy(&superseded, false, &now_rfc3339()));
    }

    #[test]
    fn set_dormant_marks_row_and_is_a_noop_the_second_time() {
        let conn = mem_conn();
        let id = insert5(&conn, "will sleep", None);
        let now = now_rfc3339();
        assert!(set_dormant(&conn, id, &now).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert!(m.is_dormant());
        assert_eq!(m.dormant_at.as_deref(), Some(now.as_str()));

        // already dormant — a second call is a no-op, not a re-stamp
        assert!(!set_dormant(&conn, id, &now_rfc3339()).unwrap());
    }

    #[test]
    fn set_dormant_missing_id_returns_false() {
        let conn = mem_conn();
        assert!(!set_dormant(&conn, 999, &now_rfc3339()).unwrap());
    }

    #[test]
    fn wake_clears_dormant_at_and_resets_last_accessed() {
        let conn = mem_conn();
        let id = insert5(&conn, "will wake", None);
        set_dormant(&conn, id, &now_rfc3339()).unwrap();

        let wake_time = now_rfc3339();
        assert!(wake(&conn, id, &wake_time).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert!(!m.is_dormant());
        assert_eq!(m.last_accessed_at.as_deref(), Some(wake_time.as_str()), "must reset so it doesn't instantly re-sleep");
    }

    #[test]
    fn wake_on_a_non_dormant_or_missing_row_is_a_noop() {
        let conn = mem_conn();
        let id = insert5(&conn, "never slept", None);
        assert!(!wake(&conn, id, &now_rfc3339()).unwrap(), "row is active, not dormant");
        assert!(!wake(&conn, 999, &now_rfc3339()).unwrap(), "row doesn't exist");
    }

    #[test]
    fn list_default_excludes_dormant_list_dormant_shows_only_them() {
        let conn = mem_conn();
        let awake_id = insert5(&conn, "awake", None);
        let sleeping_id = insert5(&conn, "sleeping", None);
        set_dormant(&conn, sleeping_id, &now_rfc3339()).unwrap();

        let default_view = list(&conn, None, false).unwrap();
        assert!(default_view.iter().any(|m| m.id == awake_id));
        assert!(default_view.iter().all(|m| m.id != sleeping_id), "dormant row must be excluded by default");

        let dormant_view = list_dormant(&conn, None).unwrap();
        assert_eq!(dormant_view.len(), 1);
        assert_eq!(dormant_view[0].id, sleeping_id);
    }

    #[test]
    fn list_pinned_shows_only_pinned_rows_regardless_of_tombstone_state() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let plain_id = insert5(&conn, "plain", None);
        let old_id = insert5(&conn, "old fact", None);
        let new_id = insert5(&conn, "new fact", None);
        assert!(supersede(&conn, old_id, new_id, &now).unwrap());
        assert!(restore(&conn, old_id, &now).unwrap()); // restore pins it, active again
        assert!(is_pinned(&conn, old_id).unwrap());

        // A later manual re-supersede tombstones it again but does not
        // clear the pin (pin state is an independent axis).
        let later_id = insert5(&conn, "later fact", None);
        assert!(supersede(&conn, old_id, later_id, &now).unwrap());
        assert!(is_pinned(&conn, old_id).unwrap(), "pin must survive a manual re-supersede");

        let pinned_view = list_pinned(&conn, None).unwrap();
        assert_eq!(pinned_view.len(), 1);
        assert_eq!(pinned_view[0].id, old_id);

        // Default `list` is unaffected by pin state -- pinned is an
        // independent axis, not a visibility filter like superseded/dormant.
        let default_view = list(&conn, None, false).unwrap();
        assert!(default_view.iter().all(|m| m.id != old_id), "old_id is tombstoned again -- excluded by the ordinary superseded filter regardless of its pin");
        assert!(default_view.iter().any(|m| m.id == plain_id));
        let _ = new_id;
    }

    #[test]
    fn dormant_rows_are_excluded_from_search_recall_and_reflection_working_sets() {
        let conn = mem_conn();
        let emb = fake_embed("shared dormancy topic");
        let awake_id = insert5(&conn, "awake shared dormancy topic", Some(&emb));
        let sleeping_id = insert(&conn, "sleeping shared dormancy topic", None, None, false, Some(&emb), 5).unwrap();
        set_dormant(&conn, sleeping_id, &now_rfc3339()).unwrap();

        let now = now_rfc3339();
        // reviewed_only = false — organic default, unreviewed rows included
        // too — proves dormant exclusion holds even then, not just under
        // the stricter --reviewed-only path.
        let ranked = search_ranked(&conn, &emb, 10, false, false, 0.0, &now).unwrap();
        assert!(ranked.iter().all(|h| h.memory.id != sleeping_id), "search_ranked must exclude dormant rows even when unreviewed rows are included");
        assert!(ranked.iter().any(|h| h.memory.id == awake_id));

        let top = top_similar_active(&conn, &emb, 10).unwrap();
        assert!(top.iter().all(|(m, _)| m.id != sleeping_id), "top_similar_active must exclude dormant rows");

        let since = memories_since(&conn, 0).unwrap();
        assert!(since.iter().all(|m| m.id != sleeping_id), "memories_since must exclude dormant rows");
    }

    #[test]
    fn cited_memory_ids_collects_raw_ids_from_active_insights_and_themes_only() {
        let conn = mem_conn();
        let cited_by_insight = insert5(&conn, "cited by a plain insight", None);
        let cited_by_theme = insert5(&conn, "cited by a theme", None);
        let uncited = insert5(&conn, "cited by nothing", None);
        let only_by_invalidated = insert5(&conn, "cited only by an invalidated insight", None);

        insert_insight(&conn, "insight", 0.5, &[cited_by_insight.to_string()], None).unwrap();
        insert_theme(&conn, "theme", 0.5, &["i1".into(), cited_by_theme.to_string()], None).unwrap();
        let invalidated_ins =
            insert_insight(&conn, "gone", 0.5, &[only_by_invalidated.to_string()], None).unwrap();
        conn.execute(
            "UPDATE insights SET invalidated_at = ?1 WHERE id = ?2",
            params![now_rfc3339(), invalidated_ins],
        )
        .unwrap();

        let cited = cited_memory_ids(&conn).unwrap();
        assert!(cited.contains(&cited_by_insight));
        assert!(cited.contains(&cited_by_theme));
        assert!(!cited.contains(&uncited));
        assert!(!cited.contains(&only_by_invalidated), "citation from an invalidated insight doesn't count");
    }

    #[test]
    fn active_memories_for_dormancy_excludes_dormant_and_superseded_but_includes_unreviewed() {
        let conn = mem_conn();
        let active_reviewed = insert5(&conn, "active reviewed", None);
        let active_unreviewed = insert(&conn, "active unreviewed", None, None, false, None, 5).unwrap();
        let dormant_id = insert5(&conn, "dormant", None);
        set_dormant(&conn, dormant_id, &now_rfc3339()).unwrap();
        let old_id = insert5(&conn, "superseded", None);
        let new_id = insert5(&conn, "superseding", None);
        supersede(&conn, old_id, new_id, &now_rfc3339()).unwrap();

        let pool = active_memories_for_dormancy(&conn).unwrap();
        let ids: std::collections::HashSet<i64> = pool.iter().map(|m| m.id).collect();
        assert!(ids.contains(&active_reviewed));
        assert!(ids.contains(&active_unreviewed), "unreviewed rows must be in the dormancy pool too");
        assert!(!ids.contains(&dormant_id));
        assert!(!ids.contains(&old_id), "the superseded row itself must be excluded");
    }

    // --- export/import support helpers ---

    #[test]
    fn memory_last_modified_picks_the_latest_of_created_last_accessed_invalidated_dormant() {
        let mut m = dormancy_fixture(100.0, 0, None, 5, true);
        let created = m.created_at.clone();
        assert_eq!(memory_last_modified(&m), created, "no other timestamp set — created_at wins");

        m.last_accessed_at = Some("2027-01-01T00:00:00Z".to_string());
        assert_eq!(memory_last_modified(&m), "2027-01-01T00:00:00Z");

        m.invalidated_at = Some("2028-01-01T00:00:00Z".to_string());
        assert_eq!(memory_last_modified(&m), "2028-01-01T00:00:00Z", "a later invalidated_at must win over last_accessed_at");

        m.dormant_at = Some("2026-06-01T00:00:00Z".to_string());
        assert_eq!(memory_last_modified(&m), "2028-01-01T00:00:00Z", "an earlier dormant_at must not override the later invalidated_at");
    }

    #[test]
    fn insight_last_modified_picks_the_latest_of_created_verified_flagged_invalidated() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "x", 0.5, &["1".into(), "2".into()], None).unwrap();
        let ins = get_insight(&conn, id).unwrap().unwrap();
        let created = ins.created_at.clone();
        assert_eq!(insight_last_modified(&ins), created);

        let mut later = ins.clone();
        later.flagged_at = Some("2099-01-01T00:00:00Z".to_string());
        assert_eq!(insight_last_modified(&later), "2099-01-01T00:00:00Z");
    }

    #[test]
    fn is_db_empty_true_only_when_both_tables_have_no_rows() {
        let conn = mem_conn();
        assert!(is_db_empty(&conn).unwrap());
        insert5(&conn, "something", None);
        assert!(!is_db_empty(&conn).unwrap());
    }

    #[test]
    fn all_memories_and_all_insights_include_every_state() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old", None);
        let new_id = insert5(&conn, "new", None);
        supersede(&conn, old_id, new_id, &now_rfc3339()).unwrap();
        let dormant_id = insert5(&conn, "dormant", None);
        set_dormant(&conn, dormant_id, &now_rfc3339()).unwrap();
        let unreviewed_id = insert(&conn, "unreviewed", None, None, false, None, 5).unwrap();

        let all = all_memories(&conn).unwrap();
        let ids: std::collections::HashSet<i64> = all.iter().map(|m| m.id).collect();
        assert!(ids.contains(&old_id));
        assert!(ids.contains(&new_id));
        assert!(ids.contains(&dormant_id));
        assert!(ids.contains(&unreviewed_id));
        assert_eq!(all.len(), 4);

        let ins_id = insert_insight(&conn, "an insight", 0.5, &["1".into(), "2".into()], None).unwrap();
        flag_insight(&conn, ins_id, &now_rfc3339()).unwrap();
        let all_ins = all_insights(&conn).unwrap();
        assert_eq!(all_ins.len(), 1);
        assert!(all_ins[0].is_flagged());
    }

    #[test]
    fn raw_upsert_memory_inserts_at_explicit_id_then_overwrites_in_place() {
        let conn = mem_conn();
        let mut m = dormancy_fixture(0.0, 0, None, 5, true);
        m.id = 42;
        m.content = "explicit id row".to_string();
        raw_upsert_memory(&conn, &m).unwrap();
        assert_eq!(get(&conn, 42).unwrap().unwrap().content, "explicit id row");

        m.content = "overwritten".to_string();
        raw_upsert_memory(&conn, &m).unwrap();
        assert_eq!(get(&conn, 42).unwrap().unwrap().content, "overwritten");
        assert_eq!(list(&conn, None, false).unwrap().len(), 1, "must overwrite in place, not duplicate");
    }

    #[test]
    fn raw_upsert_insight_inserts_at_explicit_id_then_overwrites_in_place() {
        let conn = mem_conn();
        let i = Insight {
            id: 7,
            text: "explicit id insight".to_string(),
            created_at: now_rfc3339(),
            confidence: 0.5,
            source_ids: vec!["1".into(), "2".into()],
            embedding: None,
            invalidated_at: None,
            flagged_at: None,
            last_verified_at: None,
            level: 1,
            revised_at: None,
            prev_text: None,
        };
        raw_upsert_insight(&conn, &i).unwrap();
        assert_eq!(get_insight(&conn, 7).unwrap().unwrap().text, "explicit id insight");

        let mut updated = i.clone();
        updated.text = "overwritten insight".to_string();
        raw_upsert_insight(&conn, &updated).unwrap();
        assert_eq!(get_insight(&conn, 7).unwrap().unwrap().text, "overwritten insight");
    }

    // --- entities/relations: the graph layer ---

    fn unit_vec(dims: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dims];
        v[hot] = 1.0;
        v
    }

    #[test]
    fn migration_v8_to_v9_adds_graph_extracted_at_and_creates_entities_relations() {
        // Build a v8-era database by hand: every table exactly as
        // `migrate_v7_to_v8` leaves it, no `graph_extracted_at` column, no
        // `entities`/`relations` tables, user_version = 8.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            CREATE TABLE dedupe_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE contradiction_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE ingested_sessions (session_id TEXT PRIMARY KEY, ingested_at TEXT NOT NULL);
            INSERT INTO memories (content, created_at, valid_from, stability, importance)
            VALUES ('a v8 memory', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 35.0, 7);
            PRAGMA user_version = 8;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "v8->v9 (graph layer) runs");

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1, "existing memory row must survive the migration");
        assert!(rows[0].graph_extracted_at.is_none(), "pre-existing row must backfill to never-extracted (NULL)");

        // entities/relations exist and behave — round-trip through the
        // store functions that use them.
        let e1 = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let e2 = insert_entity(&conn, "user", None, None).unwrap();
        let rel_id = insert_relation(&conn, e2, "works-on", e1, Some(rows[0].id), Some(0.8), "2026-01-02T00:00:00Z").unwrap();
        assert!(get_relation(&conn, rel_id).unwrap().unwrap().is_active());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
    }

    #[test]
    fn migration_v9_to_v10_adds_last_completed_at_and_creates_entity_merge_seen() {
        // Build a v9-era database by hand: every table exactly as
        // `migrate_v8_to_v9` leaves it -- `reflect_state` with no
        // `last_completed_at` column, no `entity_merge_seen` table.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id INTEGER PRIMARY KEY AUTOINCREMENT, content TEXT NOT NULL, source TEXT, project TEXT,
                created_at TEXT NOT NULL, reviewed INTEGER NOT NULL DEFAULT 1, embedding BLOB,
                importance INTEGER NOT NULL DEFAULT 5, stability REAL, access_count INTEGER NOT NULL DEFAULT 0,
                first_accessed_at TEXT, last_accessed_at TEXT, valid_from TEXT, invalidated_at TEXT,
                superseded_by INTEGER, dormant_at TEXT, last_verified_at TEXT, graph_extracted_at TEXT
            );
            CREATE TABLE insights (
                id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, created_at TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.5, source_ids TEXT NOT NULL, embedding BLOB,
                invalidated_at TEXT, flagged_at TEXT, last_verified_at TEXT, level INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE reflect_state (id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER);
            INSERT INTO reflect_state (id, last_run_at, last_memory_id) VALUES (1, '2026-01-01T00:00:00Z', 5);
            CREATE TABLE dedupe_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE contradiction_seen (id_a INTEGER NOT NULL, id_b INTEGER NOT NULL, PRIMARY KEY (id_a, id_b));
            CREATE TABLE ingested_sessions (session_id TEXT PRIMARY KEY, ingested_at TEXT NOT NULL);
            CREATE TABLE entities (
                id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, kind TEXT, embedding BLOB,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX idx_entities_name_nocase ON entities (name COLLATE NOCASE);
            CREATE TABLE relations (
                id INTEGER PRIMARY KEY AUTOINCREMENT, src INTEGER NOT NULL REFERENCES entities(id),
                predicate TEXT NOT NULL, dst INTEGER NOT NULL REFERENCES entities(id),
                evidence_memory_id INTEGER REFERENCES memories(id), confidence REAL, created_at TEXT NOT NULL,
                valid_from TEXT, invalidated_at TEXT, superseded_by INTEGER
            );
            PRAGMA user_version = 9;",
        )
        .unwrap();

        init_schema(&conn).unwrap();
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION, "v9->v10 (last_completed_at + entity_merge_seen) runs");

        // The pre-existing watermark row survives, and last_completed_at
        // backfills to NULL (never completed under the new field yet).
        let state = get_reflect_state(&conn).unwrap();
        assert_eq!(state.last_run_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(state.last_memory_id, Some(5));
        assert!(state.last_completed_at.is_none());

        // The new field and table both behave via the store functions that
        // use them.
        mark_reflect_completed(&conn, "2026-01-02T00:00:00Z").unwrap();
        assert_eq!(get_reflect_state(&conn).unwrap().last_completed_at.as_deref(), Some("2026-01-02T00:00:00Z"));
        mark_entity_merge_seen(&conn, 3, 1).unwrap();
        assert_eq!(entity_merge_seen_pairs(&conn).unwrap(), [(1, 3)].into_iter().collect());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(get_reflect_state(&conn).unwrap().last_run_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn find_entity_by_name_is_case_insensitive() {
        let conn = mem_conn();
        let id = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let found = find_entity_by_name(&conn, "moses").unwrap().unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.name, "Moses");
        assert!(find_entity_by_name(&conn, "nobody").unwrap().is_none());
    }

    #[test]
    fn insert_entity_rejects_case_insensitive_duplicate_name() {
        let conn = mem_conn();
        insert_entity(&conn, "Moses", None, None).unwrap();
        let err = insert_entity(&conn, "MOSES", None, None);
        assert!(err.is_err(), "the case-insensitive unique index must reject a duplicate name");
    }

    #[test]
    fn find_entity_by_similarity_picks_the_closest_match_above_threshold() {
        let conn = mem_conn();
        insert_entity(&conn, "Umoja", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let far = insert_entity(&conn, "C++", Some("technology"), Some(&unit_vec(4, 1))).unwrap();

        let close_match = find_entity_by_similarity(&conn, &unit_vec(4, 0), 0.85).unwrap();
        assert_eq!(close_match.unwrap().0.name, "Umoja");

        // A query embedding orthogonal to every entity's own must clear
        // nothing at a high threshold.
        let no_match = find_entity_by_similarity(&conn, &unit_vec(4, 2), 0.85).unwrap();
        assert!(no_match.is_none());
        let _ = far;
    }

    #[test]
    fn entity_counts_by_kind_folds_null_and_blank_into_unspecified() {
        let conn = mem_conn();
        insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        insert_entity(&conn, "Umoja", Some(""), None).unwrap();
        insert_entity(&conn, "mystery", None, None).unwrap();

        let counts = entity_counts_by_kind(&conn).unwrap();
        assert_eq!(counts.iter().find(|(k, _)| k == "person").unwrap().1, 2);
        assert_eq!(counts.iter().find(|(k, _)| k == "unspecified").unwrap().1, 2);
        assert_eq!(entity_count(&conn).unwrap(), 4);
    }

    #[test]
    fn active_relations_for_entity_finds_both_directions_and_excludes_invalidated() {
        let conn = mem_conn();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let now = now_rfc3339();
        let r1 = insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        let r2 = insert_relation(&conn, user, "works-on", moses, None, Some(0.5), &now).unwrap();

        let for_moses = active_relations_for_entity(&conn, moses).unwrap();
        assert_eq!(for_moses.len(), 2);

        supersede_relation(&conn, r1, r2, &now).unwrap();
        let for_moses_after = active_relations_for_entity(&conn, moses).unwrap();
        assert_eq!(for_moses_after.len(), 1, "the invalidated edge must be excluded");
        assert_eq!(invalidated_relation_count_for_entity(&conn, moses).unwrap(), 1);
        assert!(!get_relation(&conn, r1).unwrap().unwrap().is_active());
    }

    #[test]
    fn relations_conflicting_with_finds_the_boss_test_shape() {
        // "Moses boss-of user" already active; a candidate "Ivar boss-of
        // user" edge shares predicate + dst (user), src differs -- exactly
        // the shape the edge-contradiction judge needs to see.
        let conn = mem_conn();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = insert_entity(&conn, "Ivar", Some("person"), None).unwrap();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let now = now_rfc3339();
        insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();

        let candidates = relations_conflicting_with(&conn, ivar, "boss-of", user).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].src, moses);

        // A different predicate on the same pair must never match.
        assert!(relations_conflicting_with(&conn, ivar, "friend-of", user).unwrap().is_empty());
        // A same-predicate edge sharing neither src nor dst must never match.
        let other = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        assert!(relations_conflicting_with(&conn, ivar, "boss-of", other).unwrap().is_empty());
    }

    #[test]
    fn relations_conflicting_with_excludes_the_literal_same_edge() {
        let conn = mem_conn();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        let now = now_rfc3339();
        insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();
        assert!(
            relations_conflicting_with(&conn, moses, "boss-of", user).unwrap().is_empty(),
            "the exact same (src, predicate, dst) must never be its own conflict candidate"
        );
    }

    #[test]
    fn graph_extraction_candidates_excludes_extracted_dormant_and_superseded() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let a = insert(&conn, "never extracted", None, None, true, None, 5).unwrap();
        let b = insert(&conn, "already extracted", None, None, true, None, 5).unwrap();
        let c = insert(&conn, "dormant memory", None, None, true, None, 5).unwrap();
        let d = insert(&conn, "superseded memory", None, None, true, None, 5).unwrap();

        assert!(has_graph_extraction_candidates(&conn).unwrap());
        mark_graph_extracted(&conn, b, &now).unwrap();
        set_dormant(&conn, c, &now).unwrap();
        supersede(&conn, d, a, &now).unwrap();

        let candidates = graph_extraction_candidates(&conn, 10).unwrap();
        let ids: Vec<i64> = candidates.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![a], "only the never-extracted, active, non-dormant row remains");
    }

    #[test]
    fn index_owned_rows_are_excluded_from_graph_extraction_strength_review_and_supersession_audit() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let user = insert(&conn, "a user fact", None, None, true, None, 7).unwrap();
        let idx1 = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ v1", None, &now).unwrap();
        let idx2 = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ v2", None, &now).unwrap();
        assert!(get(&conn, idx1).unwrap().unwrap().is_superseded(), "precondition: an index->index supersession exists");

        let g: Vec<i64> = graph_extraction_candidates(&conn, 10).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(g, vec![user]);
        let sr: Vec<i64> = memories_due_for_strength_review(&conn, 10, 1).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(sr, vec![user]);
        mark_graph_extracted(&conn, user, &now).unwrap();
        assert!(!has_graph_extraction_candidates(&conn).unwrap(), "an index row alone is no graph backlog");

        let audit: Vec<(i64, i64)> = supersession_candidates(&conn).unwrap().iter().map(|c| (c.old_id, c.new_id)).collect();
        assert!(!audit.contains(&(idx1, idx2)), "index regenerations are never audited as lossy supersessions");
    }

    #[test]
    fn code_history_owned_rows_are_also_excluded_from_graph_extraction_strength_review_and_supersession_audit() {
        // Task-3 extension of the test above: `code-history:` must be
        // covered by the exact same shared helper as `code-index:`.
        let conn = mem_conn();
        let now = now_rfc3339();
        let user = insert(&conn, "a user fact", None, None, true, None, 7).unwrap();
        let h1 = upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: v1", None, &now).unwrap();
        let h2 = upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: v2", None, &now).unwrap();
        assert!(get(&conn, h1).unwrap().unwrap().is_superseded(), "precondition: a history->history regeneration exists");

        let g: Vec<i64> = graph_extraction_candidates(&conn, 10).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(g, vec![user]);
        let sr: Vec<i64> = memories_due_for_strength_review(&conn, 10, 1).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(sr, vec![user]);
        mark_graph_extracted(&conn, user, &now).unwrap();
        assert!(!has_graph_extraction_candidates(&conn).unwrap(), "a history row alone is no graph backlog");

        let audit: Vec<(i64, i64)> = supersession_candidates(&conn).unwrap().iter().map(|c| (c.old_id, c.new_id)).collect();
        assert!(!audit.contains(&(h1, h2)), "history regenerations are never audited as lossy supersessions");
    }

    #[test]
    fn has_graph_extraction_candidates_is_false_once_everything_is_marked() {
        let conn = mem_conn();
        let now = now_rfc3339();
        assert!(!has_graph_extraction_candidates(&conn).unwrap(), "an empty store has nothing to extract");
        let a = insert(&conn, "a fact", None, None, true, None, 5).unwrap();
        assert!(has_graph_extraction_candidates(&conn).unwrap());
        mark_graph_extracted(&conn, a, &now).unwrap();
        assert!(!has_graph_extraction_candidates(&conn).unwrap());
    }

    // --- multiply_stability: schema-accelerated consolidation ---

    #[test]
    fn multiply_stability_scales_by_the_given_factor() {
        let conn = mem_conn();
        let id = insert(&conn, "a fact", None, None, true, None, 5).unwrap(); // stability = 35.0
        assert!(multiply_stability(&conn, id, 1.5).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(52.5));
    }

    #[test]
    fn multiply_stability_caps_at_365_days() {
        let conn = mem_conn();
        let id = insert(&conn, "a fact", None, None, true, None, 10).unwrap(); // stability = 70.0
        for _ in 0..10 {
            multiply_stability(&conn, id, 1.5).unwrap();
        }
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(365.0));
    }

    #[test]
    fn multiply_stability_is_a_noop_on_a_superseded_or_missing_row() {
        let conn = mem_conn();
        let old = insert(&conn, "old fact", None, None, true, None, 5).unwrap();
        let new = insert(&conn, "new fact", None, None, true, None, 5).unwrap();
        supersede(&conn, old, new, &now_rfc3339()).unwrap();
        assert!(!multiply_stability(&conn, old, 1.5).unwrap(), "a superseded row must not be boosted");
        assert!(!multiply_stability(&conn, 999_999, 1.5).unwrap());
    }

    // --- graph hygiene: evidence-death propagation ---

    #[test]
    fn relations_with_dead_evidence_finds_invalidated_evidence_only() {
        // The "hard-deleted evidence" leg of this query's own LEFT JOIN
        // (`m.id IS NULL`) is defensive rather than reachable through this
        // store's own API today: `relations.evidence_memory_id REFERENCES
        // memories(id)` with this build's foreign_keys=ON means a memory
        // cited as evidence can never actually be hard-deleted (`delete`
        // itself would fail the FK constraint first) — so this test covers
        // the one path that's actually reachable: invalidation.
        let conn = mem_conn();
        let now = now_rfc3339();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let umoja = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();

        let alive_mem = insert(&conn, "Moses proposes a feature for Umoja", None, None, true, None, 5).unwrap();
        let dying_mem = insert(&conn, "a memory about to be superseded", None, None, true, None, 5).unwrap();

        let r_alive = insert_relation(&conn, moses, "proposes-feature-for", umoja, Some(alive_mem), Some(0.9), &now).unwrap();
        let r_dead_invalidated =
            insert_relation(&conn, moses, "used-to-lead", umoja, Some(dying_mem), Some(0.7), &now).unwrap();
        let r_no_evidence = insert_relation(&conn, moses, "no-evidence-edge", umoja, None, Some(0.5), &now).unwrap();

        let replacement = insert(&conn, "replacement fact", None, None, true, None, 5).unwrap();
        supersede(&conn, dying_mem, replacement, &now).unwrap();

        let dying = relations_with_dead_evidence(&conn).unwrap();
        let dying_ids: Vec<i64> = dying.iter().map(|r| r.id).collect();
        assert_eq!(dying_ids, vec![r_dead_invalidated]);
        assert!(!dying_ids.contains(&r_alive), "an edge with live evidence must never be a candidate");
        assert!(!dying_ids.contains(&r_no_evidence), "an edge with no evidence at all has nothing to die");
    }

    #[test]
    fn invalidate_relation_never_sets_superseded_by() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let a = insert_entity(&conn, "A", None, None).unwrap();
        let b = insert_entity(&conn, "B", None, None).unwrap();
        let r = insert_relation(&conn, a, "rel", b, None, Some(0.9), &now).unwrap();

        assert!(invalidate_relation(&conn, r, &now).unwrap());
        let row = get_relation(&conn, r).unwrap().unwrap();
        assert!(!row.is_active());
        assert_eq!(row.superseded_by, None, "died with its evidence -- never beaten by a rival edge");

        assert!(!invalidate_relation(&conn, r, &now).unwrap(), "already invalidated -- no-op");
        assert!(!invalidate_relation(&conn, 999_999, &now).unwrap());
    }

    #[test]
    fn evidence_death_propagation_leaves_dormant_evidence_alone() {
        // A dormant (not invalidated, not deleted) evidence memory must
        // never appear as a dead-evidence candidate -- it can still wake.
        let conn = mem_conn();
        let now = now_rfc3339();
        let a = insert_entity(&conn, "A", None, None).unwrap();
        let b = insert_entity(&conn, "B", None, None).unwrap();
        let dormant_mem = insert(&conn, "a fact that will nap", None, None, true, None, 5).unwrap();
        let r = insert_relation(&conn, a, "rel", b, Some(dormant_mem), Some(0.9), &now).unwrap();

        set_dormant(&conn, dormant_mem, &now).unwrap();
        assert!(relations_with_dead_evidence(&conn).unwrap().is_empty());
        assert!(relation_evidence_is_dormant(&conn, &get_relation(&conn, r).unwrap().unwrap()).unwrap());
    }

    #[test]
    fn relation_evidence_is_dormant_false_for_live_or_absent_evidence() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let a = insert_entity(&conn, "A", None, None).unwrap();
        let b = insert_entity(&conn, "B", None, None).unwrap();
        let live_mem = insert(&conn, "a lively fact", None, None, true, None, 5).unwrap();
        let r_live = insert_relation(&conn, a, "rel", b, Some(live_mem), Some(0.9), &now).unwrap();
        let r_none = insert_relation(&conn, a, "rel2", b, None, Some(0.9), &now).unwrap();

        assert!(!relation_evidence_is_dormant(&conn, &get_relation(&conn, r_live).unwrap().unwrap()).unwrap());
        assert!(!relation_evidence_is_dormant(&conn, &get_relation(&conn, r_none).unwrap().unwrap()).unwrap());
    }

    // --- graph hygiene: entity merge ---

    #[test]
    fn entity_merge_candidate_pairs_matches_by_similarity_or_normalized_name() {
        let conn = mem_conn();
        let umoja = insert_entity(&conn, "Umoja", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let umoja_near = insert_entity(&conn, "umoja project", Some("project"), Some(&unit_vec(4, 0))).unwrap();
        let cplusplus = insert_entity(&conn, "C++", Some("technology"), None).unwrap();
        let cplusplus_punct = insert_entity(&conn, "c ++", Some("technology"), None).unwrap();
        let unrelated = insert_entity(&conn, "Moses", Some("person"), Some(&unit_vec(4, 2))).unwrap();

        let seen = std::collections::HashSet::new();
        let pairs = entity_merge_candidate_pairs(&conn, 0.9, &seen, 10).unwrap();

        let (a, b) = normalize_pair(umoja, umoja_near);
        assert!(pairs.contains(&(a, b)), "identical embeddings above the similarity floor must be a candidate");
        let (a, b) = normalize_pair(cplusplus, cplusplus_punct);
        assert!(pairs.contains(&(a, b)), "case/punctuation-insensitive name match must be a candidate with no embedding at all");
        assert!(!pairs.iter().any(|&(x, y)| x == unrelated || y == unrelated), "an unrelated entity must never be a candidate");
    }

    #[test]
    fn entity_merge_candidate_pairs_excludes_seen_pairs() {
        let conn = mem_conn();
        // "Umoja" / "umoja project" differ under the case-insensitive exact
        // name index (so both can coexist) but share a name-embedding, which
        // is what makes them a merge candidate at all.
        let a = insert_entity(&conn, "Umoja", None, Some(&unit_vec(4, 0))).unwrap();
        let b = insert_entity(&conn, "umoja project", None, Some(&unit_vec(4, 0))).unwrap();
        let mut seen = std::collections::HashSet::new();
        seen.insert(normalize_pair(a, b));
        assert!(entity_merge_candidate_pairs(&conn, 0.9, &seen, 10).unwrap().is_empty());
    }

    #[test]
    fn mark_entity_merge_seen_roundtrips_normalized() {
        let conn = mem_conn();
        mark_entity_merge_seen(&conn, 5, 2).unwrap();
        let seen = entity_merge_seen_pairs(&conn).unwrap();
        assert!(seen.contains(&(2, 5)));
        // Idempotent -- asking again (either order) never errors or duplicates.
        mark_entity_merge_seen(&conn, 2, 5).unwrap();
        assert_eq!(entity_merge_seen_pairs(&conn).unwrap().len(), 1);
    }

    #[test]
    fn sample_relation_descriptions_renders_up_to_n_edges_by_name() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let umoja = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let user = insert_entity(&conn, "user", None, None).unwrap();
        insert_relation(&conn, moses, "proposes-feature-for", umoja, None, Some(0.9), &now).unwrap();
        insert_relation(&conn, moses, "boss-of", user, None, Some(0.9), &now).unwrap();

        let samples = sample_relation_descriptions(&conn, moses, 2).unwrap();
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().any(|s| s.contains("Moses") && s.contains("Umoja")));
        assert!(samples.iter().any(|s| s.contains("Moses") && s.contains("user")));
    }

    #[test]
    fn repoint_entity_relations_moves_edges_and_dedupes_resulting_identicals() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let older = insert_entity(&conn, "Umoja", Some("project"), None).unwrap();
        let newer = insert_entity(&conn, "umoja project", Some("project"), None).unwrap();
        let moses = insert_entity(&conn, "Moses", Some("person"), None).unwrap();
        let ivar = insert_entity(&conn, "Ivar", Some("person"), None).unwrap();

        // An edge already shared between the two names (will become
        // identical once repointed -- one must survive, the other must be
        // invalidated as a dedup, never both left active).
        let r_older = insert_relation(&conn, moses, "proposes-feature-for", older, None, Some(0.9), &now).unwrap();
        let r_newer_dup = insert_relation(&conn, moses, "proposes-feature-for", newer, None, Some(0.8), &now).unwrap();
        // An edge unique to the newer entity -- must simply move over, not collide.
        let r_newer_unique = insert_relation(&conn, ivar, "boss-of", newer, None, Some(0.9), &now).unwrap();

        let touched = repoint_entity_relations(&conn, newer, older, &now).unwrap();
        assert_eq!(touched, 2, "both edges touching the newer entity must be repointed");

        let unique_row = get_relation(&conn, r_newer_unique).unwrap().unwrap();
        assert_eq!(unique_row.dst, older, "repointed to the surviving (older) entity");
        assert!(unique_row.is_active());

        // Exactly one of the two now-identical edges survives active.
        let older_row = get_relation(&conn, r_older).unwrap().unwrap();
        let dup_row = get_relation(&conn, r_newer_dup).unwrap().unwrap();
        assert!(older_row.is_active(), "the older (lower-id) duplicate survives");
        assert!(!dup_row.is_active(), "the newer duplicate must be invalidated, not left active alongside an identical edge");
        assert_eq!(dup_row.superseded_by, Some(r_older));
    }

    #[test]
    fn delete_entity_removes_the_row() {
        let conn = mem_conn();
        let id = insert_entity(&conn, "throwaway", None, None).unwrap();
        assert!(delete_entity(&conn, id).unwrap());
        assert!(get_entity(&conn, id).unwrap().is_none());
        assert!(!delete_entity(&conn, id).unwrap(), "already gone -- no-op");
    }

    #[test]
    fn active_relations_all_excludes_invalidated() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let a = insert_entity(&conn, "A", None, None).unwrap();
        let b = insert_entity(&conn, "B", None, None).unwrap();
        let r1 = insert_relation(&conn, a, "rel", b, None, Some(0.9), &now).unwrap();
        let r2 = insert_relation(&conn, a, "rel2", b, None, Some(0.9), &now).unwrap();
        invalidate_relation(&conn, r2, &now).unwrap();

        let all = active_relations_all(&conn).unwrap();
        assert_eq!(all.iter().map(|r| r.id).collect::<Vec<_>>(), vec![r1]);
    }

    #[test]
    fn migration_v10_to_v11_seeds_improve_state_and_adds_skill_usage() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE ingested_sessions (session_id TEXT PRIMARY KEY, ingested_at TEXT NOT NULL);
             INSERT INTO ingested_sessions VALUES ('s1', '2026-01-01T00:00:00Z');
             CREATE TABLE improve_state (
                id INTEGER PRIMARY KEY CHECK (id = 1), last_run_at TEXT, last_memory_id INTEGER,
                last_relation_id INTEGER, last_completed_at TEXT
             );
             PRAGMA user_version = 10;",
        )
        .unwrap();
        migrate_v10_to_v11(&conn).unwrap();
        // idempotent
        migrate_v10_to_v11(&conn).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 11);
        migrate_v11_to_v12(&conn).unwrap();
        migrate_v11_to_v12(&conn).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 12);
        migrate_v12_to_v13(&conn).unwrap();
        migrate_v12_to_v13(&conn).unwrap();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 13);
        assert_eq!(count_assoc(&conn).unwrap(), 0);
        let cols = existing_ingested_sessions_columns(&conn).unwrap();
        assert!(cols.iter().any(|c| c == "skill_usage"));
        assert_eq!(get_improve_state(&conn).unwrap(), ImproveState::default());
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM ingested_sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn improve_state_and_skill_usage_round_trip() {
        let conn = mem_conn();
        let now = "2026-09-08T10:00:00Z";
        mark_improve_completed(&conn, now).unwrap();
        update_improve_state(&conn, now, Some(7), Some(3)).unwrap();
        let st = get_improve_state(&conn).unwrap();
        assert_eq!(st.last_memory_id, Some(7));
        assert_eq!(st.last_relation_id, Some(3));
        assert_eq!(st.last_completed_at.as_deref(), Some(now));

        mark_session_ingested_with_usage(&conn, "s1", "2026-09-01T00:00:00Z", Some("{\"kb\":{\"invocations\":2,\"corrections\":0}}")).unwrap();
        mark_session_ingested(&conn, "s2", "2026-09-02T00:00:00Z").unwrap();
        mark_session_ingested_with_usage(&conn, "s0", "2026-08-01T00:00:00Z", Some("{}")).unwrap();
        let rows = skill_usage_since(&conn, "2026-08-15T00:00:00Z").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "2026-09-01T00:00:00Z");

        let a = insert(&conn, "outcome", Some("improve 2026-09-08T10:00:00Z abc"), Some("claude-config"), true, None, 6).unwrap();
        let b = insert(&conn, "other", Some("session-digest"), Some("claude-config"), true, None, 5).unwrap();
        let _c = insert(&conn, "unrelated", Some("improvement idea"), None, true, None, 5).unwrap();
        let hits: Vec<i64> = memories_by_source_prefix_or_project(&conn, "improve ", "claude-config").unwrap().iter().map(|m| m.id).collect();
        assert_eq!(hits, vec![a, b]);
        assert_eq!(latest_memory_id(&conn).unwrap(), _c);
        assert_eq!(latest_relation_id(&conn).unwrap(), 0);
    }

    #[test]
    fn session_progress_round_trip() {
        let conn = mem_conn();
        assert_eq!(session_progress(&conn, "s").unwrap(), 0);
        set_session_progress(&conn, "s", 120, "2026-09-08T10:00:00Z").unwrap();
        set_session_progress(&conn, "s", 340, "2026-09-08T11:00:00Z").unwrap();
        assert_eq!(session_progress(&conn, "s").unwrap(), 340);
        assert_eq!(session_progress(&conn, "other").unwrap(), 0);
    }

    #[test]
    fn assoc_reinforces_pairs_decays_and_prunes() {
        let conn = mem_conn();
        let a = insert(&conn, "audit hub endpoints first", None, None, true, None, 5).unwrap();
        let b = insert(&conn, "strangler pattern on overhaul branch", None, None, true, None, 5).unwrap();
        let c = insert(&conn, "unrelated", None, None, true, None, 5).unwrap();
        let t0 = "2026-09-01T00:00:00Z";
        assert_eq!(reinforce_assoc(&conn, &[b, a, a], t0).unwrap(), 1, "dup ids collapse, self-pairs skipped");
        reinforce_assoc(&conn, &[a, b, c], "2026-09-02T00:00:00Z").unwrap();
        assert_eq!(count_assoc(&conn).unwrap(), 3);

        let n = assoc_neighbors(&conn, a, "2026-09-02T00:00:00Z").unwrap();
        assert_eq!(n[0].0, b, "a-b engaged twice ranks above a-c engaged once");
        assert!((n[0].1 - 2f32.sqrt()).abs() < 1e-3);
        assert!((n[1].1 - 1.0).abs() < 1e-3);

        // decay: a year later everything is under the prune floor
        assert!(assoc_weight(2, t0, "2027-09-01T00:00:00Z") < ASSOC_PRUNE_FLOOR);
        assert!(assoc_weight(2, t0, "2026-09-03T00:00:00Z") > 1.0);

        // a forgotten memory takes its edges with it at prune time
        delete(&conn, c).unwrap();
        assert_eq!(prune_assoc(&conn, "2026-09-03T00:00:00Z").unwrap(), 2);
        assert_eq!(count_assoc(&conn).unwrap(), 1);
        assert_eq!(prune_assoc(&conn, "2027-09-01T00:00:00Z").unwrap(), 1);
        assert_eq!(count_assoc(&conn).unwrap(), 0);
    }

    #[test]
    fn a_fresh_database_lands_on_schema_25() {
        let conn = mem_conn();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert!(table_exists(&conn, "projects").unwrap());
    }

    #[test]
    fn count_project_memories_counts_both_the_project_column_and_the_index_source_string() {
        let conn = mem_conn();
        insert(&conn, "umoja fact tagged by project column", None, Some("umoja"), true, None, 5).unwrap();
        insert(&conn, "umoja index row tagged by source only", Some("project-index:umoja"), None, true, None, 5).unwrap();
        insert(&conn, "an unrelated project", None, Some("helios"), true, None, 5).unwrap();
        assert_eq!(count_project_memories(&conn, "umoja").unwrap(), 2);
        assert_eq!(count_project_memories(&conn, "nonexistent").unwrap(), 0);
    }

    #[test]
    fn count_project_memories_surfaces_a_real_sql_error_instead_of_silently_returning_zero() {
        // The bug this guards: `.unwrap_or(0)` on this same query used to
        // make a genuine SQL failure indistinguishable from "no memories",
        // silently skipping the project in `mach kb projects refresh`.
        let conn = mem_conn();
        conn.execute("DROP TABLE memories", []).unwrap();
        assert!(count_project_memories(&conn, "umoja").is_err(), "a broken query must surface as an error, not a silent 0");
    }

    #[test]
    fn get_project_by_name_finds_the_row_or_reports_absence() {
        let conn = mem_conn();
        let now = now_rfc3339();
        upsert_project(&conn, "git:umoja", "umoja", "/home/x/programming/umoja", &now).unwrap();
        upsert_project(&conn, "git:helios", "helios", "/home/x/programming/helios", &now).unwrap();
        let found = get_project_by_name(&conn, "helios").unwrap().unwrap();
        assert_eq!(found.fingerprint, "git:helios");
        assert!(get_project_by_name(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn project_for_path_picks_longest_root_prefix() {
        let conn = mem_conn();
        let now = now_rfc3339();
        upsert_project(&conn, "git:a", "a", "/a", &now).unwrap();
        upsert_project(&conn, "git:ab", "ab", "/a/b", &now).unwrap();

        // A path under both roots resolves to the more specific (longer)
        // one, not just the first row a table scan happens to reach.
        let found = project_for_path(&conn, "/a/b/c").unwrap().unwrap();
        assert_eq!(found.name, "ab");

        // A path under only the outer root resolves to it.
        let found = project_for_path(&conn, "/a/x").unwrap().unwrap();
        assert_eq!(found.name, "a");

        // The root path itself matches too, not just its children.
        let found = project_for_path(&conn, "/a/b").unwrap().unwrap();
        assert_eq!(found.name, "ab");

        // A sibling that merely shares a prefix string ("/ab" vs root
        // "/a") must NOT match -- prefix matching is on path components,
        // not raw strings.
        assert!(project_for_path(&conn, "/ab").unwrap().is_none());

        // No registered root contains this path at all.
        assert!(project_for_path(&conn, "/elsewhere").unwrap().is_none());
    }

    #[test]
    fn list_projects_orders_by_name_ascending() {
        let conn = mem_conn();
        let now = now_rfc3339();
        // Registered in reverse alphabetical order, so a missing or wrong
        // ORDER BY fails the assertion instead of passing by insertion luck.
        upsert_project(&conn, "git:umoja", "umoja", "/home/x/programming/umoja", &now).unwrap();
        upsert_project(&conn, "git:helios", "helios", "/home/x/programming/helios", &now).unwrap();
        let names: Vec<String> = list_projects(&conn).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["helios".to_string(), "umoja".to_string()]);
    }

    #[test]
    fn upsert_project_reports_a_rename_and_retag_moves_every_reference() {
        let conn = mem_conn();
        let now = now_rfc3339();
        // Indexed once under the old name, with both the project column and
        // the source string carrying it -- the two places a rename must reach.
        let id = insert(&conn, "umoja uses a ModemDriver trait", Some("project-index:umoja"), Some("umoja"), true, None, 5).unwrap();

        let (_pid, prev) = upsert_project(&conn, "git:abc123", "umoja", "/home/x/programming/umoja", &now).unwrap();
        assert_eq!(prev, None, "first registration is not a rename");

        let (_pid, prev) = upsert_project(&conn, "git:abc123", "umoja-v2", "/home/x/programming/umoja-v2", &now).unwrap();
        assert_eq!(prev.as_deref(), Some("umoja"), "same fingerprint, new name = rename");

        let moved = retag_project(&conn, "umoja", "umoja-v2").unwrap();
        assert_eq!(moved, 2, "one project column and one source string");
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.project.as_deref(), Some("umoja-v2"));
        assert_eq!(m.source.as_deref(), Some("project-index:umoja-v2"));
    }

    #[test]
    fn retagging_is_idempotent_and_leaves_other_projects_alone() {
        let conn = mem_conn();
        let other = insert(&conn, "helios renders with OpenGL", Some("project-index:helios"), Some("helios"), true, None, 5).unwrap();
        insert(&conn, "umoja fact", Some("project-index:umoja"), Some("umoja"), true, None, 5).unwrap();
        retag_project(&conn, "umoja", "umoja-v2").unwrap();
        assert_eq!(retag_project(&conn, "umoja", "umoja-v2").unwrap(), 0, "nothing left to move");
        let h = get(&conn, other).unwrap().unwrap();
        assert_eq!(h.project.as_deref(), Some("helios"), "an unrelated project is untouched");
    }

    #[test]
    fn forgetting_a_project_deletes_its_registry_row_but_leaves_its_memories() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let mem_id = insert(&conn, "umoja uses a ModemDriver trait", Some("project-index:umoja"), Some("umoja"), true, None, 5).unwrap();
        let (pid, _) = upsert_project(&conn, "git:abc123", "umoja", "/home/x/programming/umoja", &now).unwrap();

        let removed = forget_project(&conn, pid).unwrap();
        assert!(removed, "the row should report as removed");
        assert!(get_project_by_fingerprint(&conn, "git:abc123").unwrap().is_none(), "the registry row must be gone");

        // Only the registry entry goes -- the knowledge stays.
        let m = get(&conn, mem_id).unwrap().unwrap();
        assert_eq!(m.project.as_deref(), Some("umoja"), "project-index memories tagged to the project must remain untouched");
        assert_eq!(m.source.as_deref(), Some("project-index:umoja"));

        assert!(!forget_project(&conn, pid).unwrap(), "forgetting an already-gone project id reports nothing removed");
    }

    #[test]
    fn mark_indexed_refuses_a_git_projects_unreadable_commit_count_and_leaves_the_watermark_untouched() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let (pid, _) = upsert_project(&conn, "git:abc123", "umoja", "/home/x/programming/umoja", &now).unwrap();

        // The root is unreadable (deleted, moved, whatever) -- commit_count
        // came back None for a git-fingerprinted project.
        let wrote = mark_project_indexed_if_readable(&conn, pid, true, None, &now).unwrap();
        assert!(!wrote, "an unreadable git root must refuse the write");
        let row = get_project_by_fingerprint(&conn, "git:abc123").unwrap().unwrap();
        assert_eq!(row.indexed_at, None, "the watermark must be left exactly as it was");
        assert_eq!(row.indexed_commits, None);

        // A readable root proceeds normally.
        let wrote = mark_project_indexed_if_readable(&conn, pid, true, Some(7), &now).unwrap();
        assert!(wrote);
        let row = get_project_by_fingerprint(&conn, "git:abc123").unwrap().unwrap();
        assert_eq!(row.indexed_commits, Some(7));
        assert!(row.indexed_at.is_some());

        // A non-git project has no commit concept at all -- None there is
        // normal, not a failure, and must not be refused.
        let (pid2, _) = upsert_project(&conn, "path:/home/x/scratch", "scratch", "/home/x/scratch", &now).unwrap();
        let wrote = mark_project_indexed_if_readable(&conn, pid2, false, None, &now).unwrap();
        assert!(wrote, "a non-git project's absent commit count is expected, not a refusal reason");
    }

    #[test]
    fn the_registry_never_populates_the_dead_indexed_head_or_card_built_at_columns() {
        // indexed_head and card_built_at are written-never-read columns
        // (confirmed dead in the final review). The schema keeps them --
        // migrations here are additive only -- but nothing writes into them
        // any more, so they stay NULL forever going forward.
        let conn = mem_conn();
        let now = now_rfc3339();
        let (pid, _) = upsert_project(&conn, "git:dead", "deadcols", "/home/x/programming/deadcols", &now).unwrap();
        set_project_card(&conn, pid, "layout: src/").unwrap();
        mark_project_indexed(&conn, pid, Some(3), &now).unwrap();
        let row = get_project_by_fingerprint(&conn, "git:dead").unwrap().unwrap();
        assert_eq!(row.card.as_deref(), Some("layout: src/"), "the card itself is still written");
        assert_eq!(row.indexed_commits, Some(3), "the commit watermark is still written");
        assert_eq!(row.indexed_head, None, "indexed_head is dead -- never populated");
        assert_eq!(row.card_built_at, None, "card_built_at is dead -- never populated");
    }

    #[test]
    fn a_project_card_is_replaced_wholesale_and_the_watermark_is_separate() {
        let conn = mem_conn();
        let now = now_rfc3339();
        let (pid, _) = upsert_project(&conn, "git:def", "helios", "/home/x/programming/helios", &now).unwrap();
        set_project_card(&conn, pid, "layout: src/, tests/").unwrap();
        set_project_card(&conn, pid, "layout: src/, tests/, docs/").unwrap();
        let row = get_project_by_fingerprint(&conn, "git:def").unwrap().unwrap();
        assert_eq!(row.card.as_deref(), Some("layout: src/, tests/, docs/"), "regenerated, not appended");
        assert_eq!(row.indexed_at, None, "rebuilding a card is not indexing");

        mark_project_indexed(&conn, pid, Some(42), &now).unwrap();
        let row = get_project_by_fingerprint(&conn, "git:def").unwrap().unwrap();
        assert_eq!(row.indexed_commits, Some(42));
        assert!(row.indexed_at.is_some());
    }

    #[test]
    fn open_migrates_to_v26_with_empty_judge_log() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v26");
        let n: i64 = conn.query_row("SELECT count(*) FROM judge_log", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn log_judge_call_records_success_and_failure_rows() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        log_judge_call(&conn, "dedupe", "haiku", "prompt one", Ok("DISTINCT"), 12, "2026-09-23T00:00:00Z").unwrap();
        log_judge_call(&conn, "contradiction", "haiku", "prompt two", Err("offline"), 5, "2026-09-23T00:00:01Z").unwrap();

        let mut stmt = conn
            .prepare("SELECT pass, model, prompt, reply, error, latency_ms, created_at FROM judge_log ORDER BY id")
            .unwrap();
        let rows: Vec<(String, String, String, Option<String>, Option<String>, i64, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0],
            ("dedupe".into(), "haiku".into(), "prompt one".into(), Some("DISTINCT".into()), None, 12, "2026-09-23T00:00:00Z".into())
        );
        assert_eq!(
            rows[1],
            ("contradiction".into(), "haiku".into(), "prompt two".into(), None, Some("offline".into()), 5, "2026-09-23T00:00:01Z".into())
        );
    }

    #[test]
    fn migrate_v25_to_v26_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v25_to_v26(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 26);
    }

    #[test]
    fn open_migrates_to_v27_with_empty_recall_engagement() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let n: i64 = conn.query_row("SELECT count(*) FROM recall_engagement", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn migrate_v26_to_v27_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v26_to_v27(&conn).unwrap();
        migrate_v26_to_v27(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 27);
    }

    #[test]
    fn open_migrates_to_v28_with_pinned_at_column() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v27");
        let cols = existing_columns(&conn).unwrap();
        assert!(cols.iter().any(|c| c == "pinned_at"), "memories.pinned_at must exist");
    }

    #[test]
    fn migrate_v27_to_v28_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v27_to_v28(&conn).unwrap();
        migrate_v27_to_v28(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 28);
    }

    #[test]
    fn open_migrates_to_v29_with_empty_supersession_audit_table() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v28");
        let n: i64 = conn.query_row("SELECT count(*) FROM supersession_audit", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn migrate_v28_to_v29_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v28_to_v29(&conn).unwrap();
        migrate_v28_to_v29(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 29);
    }

    #[test]
    fn open_migrates_to_v30_with_reflected_at_column() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v29");
        let cols = existing_columns(&conn).unwrap();
        assert!(cols.iter().any(|c| c == "reflected_at"), "memories.reflected_at must exist");
    }

    #[test]
    fn migrate_v29_to_v30_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v29_to_v30(&conn).unwrap();
        migrate_v29_to_v30(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 30);
    }

    #[test]
    fn open_migrates_to_v31_with_revised_at_and_prev_text_columns() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v30");
        let mut stmt = conn.prepare("PRAGMA table_info(insights)").unwrap();
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(cols.iter().any(|c| c == "revised_at"), "insights.revised_at must exist");
        assert!(cols.iter().any(|c| c == "prev_text"), "insights.prev_text must exist");
    }

    #[test]
    fn migrate_v30_to_v31_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v30_to_v31(&conn).unwrap();
        migrate_v30_to_v31(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 31);
    }

    #[test]
    fn a_fresh_open_lands_on_v32_with_the_code_index_tables_and_columns() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v31");

        for table in ["code_scope", "code_files", "code_chunks", "code_chunks_fts"] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1", params![table], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(exists, 1, "{table} must exist after a fresh open");
        }

        let mut stmt = conn.prepare("PRAGMA table_info(projects)").unwrap();
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(cols.iter().any(|c| c == "code_indexed_head"), "projects.code_indexed_head must exist");
        assert!(cols.iter().any(|c| c == "code_indexed_at"), "projects.code_indexed_at must exist");
    }

    #[test]
    fn migrate_v31_to_v32_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v31_to_v32(&conn).unwrap();
        migrate_v31_to_v32(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 32);
    }

    #[test]
    fn a_fresh_open_lands_on_v33_with_the_code_summaries_tables() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v32");

        for table in ["code_summaries", "code_summaries_fts"] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type IN ('table','view') AND name = ?1", params![table], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(exists, 1, "{table} must exist after a fresh open");
        }
    }

    #[test]
    fn migrate_v32_to_v33_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v32_to_v33(&conn).unwrap();
        migrate_v32_to_v33(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 33);
    }

    #[test]
    fn a_fresh_open_lands_on_v34_with_summary_attempts_and_seedable_placeholders() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v33");

        let mut stmt = conn.prepare("PRAGMA table_info(code_files)").unwrap();
        let cols: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().collect::<Result<_, _>>().unwrap();
        assert!(cols.iter().any(|c| c == "summary_attempts"), "code_files.summary_attempts must exist");
    }

    #[test]
    fn migrate_v33_to_v34_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v33_to_v34(&conn).unwrap();
        migrate_v33_to_v34(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 34);
    }

    #[test]
    fn backfill_reflected_at_marks_ids_at_or_below_the_frozen_watermark() {
        let conn = mem_conn();
        let below = insert(&conn, "below the old watermark", None, None, true, None, 5).unwrap();
        let at = insert(&conn, "exactly at the old watermark", None, None, true, None, 5).unwrap();
        let above = insert(&conn, "above the old watermark -- the starved backlog", None, None, true, None, 5).unwrap();
        update_reflect_state(&conn, "2026-09-08T01:21:26Z", Some(at)).unwrap();

        let n = backfill_reflected_at(&conn).unwrap();
        assert_eq!(n, 2, "both below and at the watermark get backfilled");

        let get_reflected = |id: i64| -> Option<String> {
            conn.query_row("SELECT reflected_at FROM memories WHERE id = ?1", params![id], |r| r.get(0)).unwrap()
        };
        assert_eq!(get_reflected(below).as_deref(), Some("2026-09-08T01:21:26Z"));
        assert_eq!(get_reflected(at).as_deref(), Some("2026-09-08T01:21:26Z"));
        assert_eq!(get_reflected(above), None, "the starved backlog above the watermark must stay due");
    }

    #[test]
    fn backfill_reflected_at_falls_back_to_now_when_last_run_at_is_unset() {
        let conn = mem_conn();
        let below = insert(&conn, "below the watermark, but the run that set it never recorded when", None, None, true, None, 5).unwrap();
        // last_run_at left NULL, only last_memory_id set -- an edge case a
        // hand-edited or partially-imported reflect_state row can produce.
        conn.execute(
            "INSERT INTO reflect_state (id, last_run_at, last_memory_id) VALUES (1, NULL, ?1) \
             ON CONFLICT(id) DO UPDATE SET last_memory_id = excluded.last_memory_id",
            params![below],
        )
        .unwrap();

        let n = backfill_reflected_at(&conn).unwrap();
        assert_eq!(n, 1);
        let reflected: Option<String> =
            conn.query_row("SELECT reflected_at FROM memories WHERE id = ?1", params![below], |r| r.get(0)).unwrap();
        assert!(reflected.is_some(), "must fall back to now rather than stay NULL");
    }

    #[test]
    fn backfill_reflected_at_is_a_noop_when_the_watermark_was_never_set() {
        let conn = mem_conn();
        insert(&conn, "a fresh database with no reflect history", None, None, true, None, 5).unwrap();
        assert_eq!(backfill_reflected_at(&conn).unwrap(), 0);
    }

    #[test]
    fn backfill_reflected_at_never_overwrites_an_existing_timestamp() {
        let conn = mem_conn();
        let id = insert(&conn, "already reflected by ordinary use", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[id], "2026-09-20T00:00:00Z").unwrap();
        update_reflect_state(&conn, "2026-09-23T00:00:00Z", Some(id)).unwrap();

        backfill_reflected_at(&conn).unwrap();
        let reflected: Option<String> =
            conn.query_row("SELECT reflected_at FROM memories WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(reflected.as_deref(), Some("2026-09-20T00:00:00Z"), "the real timestamp must survive the backfill");
    }

    // --- reflected_at query/write primitives (schema v30) ---

    #[test]
    fn unreflected_active_count_excludes_reflected_dormant_and_superseded_rows() {
        let conn = mem_conn();
        let unreflected = insert(&conn, "still due", None, None, true, None, 5).unwrap();
        let reflected = insert(&conn, "already examined", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[reflected], &now_rfc3339()).unwrap();
        let dormant = insert(&conn, "asleep", None, None, true, None, 5).unwrap();
        set_dormant(&conn, dormant, &now_rfc3339()).unwrap();
        let old = insert(&conn, "superseded", None, None, true, None, 5).unwrap();
        let new = insert(&conn, "superseding", None, None, true, None, 5).unwrap();
        supersede(&conn, old, new, &now_rfc3339()).unwrap();
        mark_memories_reflected(&conn, &[new], &now_rfc3339()).unwrap();

        assert_eq!(unreflected_active_count(&conn).unwrap(), 1);
        let _ = unreflected;
    }

    #[test]
    fn unreflected_active_newest_and_oldest_split_without_overlap() {
        let conn = mem_conn();
        let mut ids = Vec::new();
        for i in 0..10 {
            ids.push(insert(&conn, &format!("backlog row {}", i), None, None, true, None, 5).unwrap());
        }
        let newest = unreflected_active_newest(&conn, 3).unwrap();
        assert_eq!(newest.iter().map(|m| m.id).collect::<Vec<_>>(), vec![ids[9], ids[8], ids[7]]);

        let newest_ids: HashSet<i64> = newest.iter().map(|m| m.id).collect();
        let oldest = unreflected_active_oldest(&conn, 3, &newest_ids).unwrap();
        assert_eq!(oldest.iter().map(|m| m.id).collect::<Vec<_>>(), vec![ids[0], ids[1], ids[2]]);

        let oldest_ids: HashSet<i64> = oldest.iter().map(|m| m.id).collect();
        assert!(newest_ids.is_disjoint(&oldest_ids));
    }

    #[test]
    fn unreflected_active_oldest_excludes_the_whole_backlog_when_everything_is_already_chosen() {
        let conn = mem_conn();
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(insert(&conn, &format!("small backlog {}", i), None, None, true, None, 5).unwrap());
        }
        let exclude: HashSet<i64> = ids.iter().copied().collect();
        let oldest = unreflected_active_oldest(&conn, 50, &exclude).unwrap();
        assert!(oldest.is_empty(), "every id is already excluded, so nothing more is left to add");
    }

    #[test]
    fn mark_memories_reflected_sets_exactly_the_given_ids() {
        let conn = mem_conn();
        let a = insert(&conn, "a", None, None, true, None, 5).unwrap();
        let b = insert(&conn, "b", None, None, true, None, 5).unwrap();
        let c = insert(&conn, "c", None, None, true, None, 5).unwrap();
        let n = mark_memories_reflected(&conn, &[a, c], "2026-09-23T00:00:00Z").unwrap();
        assert_eq!(n, 2);
        assert_eq!(unreflected_active_count(&conn).unwrap(), 1, "only b is left unreflected");
        let get_reflected = |id: i64| -> Option<String> {
            conn.query_row("SELECT reflected_at FROM memories WHERE id = ?1", params![id], |r| r.get(0)).unwrap()
        };
        assert_eq!(get_reflected(a).as_deref(), Some("2026-09-23T00:00:00Z"));
        assert_eq!(get_reflected(b), None);
        assert_eq!(get_reflected(c).as_deref(), Some("2026-09-23T00:00:00Z"));
    }

    #[test]
    fn mark_memories_reflected_on_an_empty_slice_is_a_noop() {
        let conn = mem_conn();
        assert_eq!(mark_memories_reflected(&conn, &[], "2026-09-23T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn mark_memories_reflected_returns_only_the_rows_actually_updated() {
        let conn = mem_conn();
        let a = insert(&conn, "a", None, None, true, None, 5).unwrap();
        let vanished = a + 9999; // never inserted -- simulates a row deleted mid-run
        let n = mark_memories_reflected(&conn, &[a, vanished], "2026-09-23T00:00:00Z").unwrap();
        assert_eq!(n, 1, "the summed row count from each UPDATE, not ids.len()");
    }

    #[test]
    fn ids_new_since_reflect_includes_unreflected_backlog_and_recently_reflected_rows() {
        let conn = mem_conn();
        let old_reflected = insert(&conn, "reflected long ago", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[old_reflected], "2026-09-01T00:00:00Z").unwrap();
        let recently_reflected = insert(&conn, "reflected by this run's own working set", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[recently_reflected], "2026-09-23T09:00:00Z").unwrap();
        let still_unreflected = insert(&conn, "still backlog", None, None, true, None, 5).unwrap();

        let ids = ids_new_since_reflect(&conn, Some("2026-09-23T00:00:00Z")).unwrap();
        assert!(ids.contains(&recently_reflected), "reflected at/after previous_run_start counts as new");
        assert!(ids.contains(&still_unreflected), "unreflected backlog always counts as new");
        assert!(!ids.contains(&old_reflected), "reflected well before previous_run_start is no longer new");
    }

    #[test]
    fn ids_new_since_reflect_treats_reflected_at_equal_to_previous_run_start_as_new() {
        // The boundary case: reflected_at == previous_run_start exactly
        // (the common case in practice -- the previous run's own `now`,
        // stored as this run's previous_run_start, is the SAME timestamp
        // it used to mark its own working set reflected). The comparison
        // is `>=`, not `>`, so this must count as new.
        let conn = mem_conn();
        let boundary = insert(&conn, "reflected at exactly the cutoff", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[boundary], "2026-09-23T00:00:00Z").unwrap();

        let ids = ids_new_since_reflect(&conn, Some("2026-09-23T00:00:00Z")).unwrap();
        assert!(ids.contains(&boundary), "reflected_at == previous_run_start must count as new (>=, not >)");
    }

    #[test]
    fn ids_new_since_reflect_with_no_previous_run_treats_only_unreflected_rows_as_new() {
        let conn = mem_conn();
        let reflected = insert(&conn, "reflected", None, None, true, None, 5).unwrap();
        mark_memories_reflected(&conn, &[reflected], &now_rfc3339()).unwrap();
        let unreflected = insert(&conn, "unreflected", None, None, true, None, 5).unwrap();

        let ids = ids_new_since_reflect(&conn, None).unwrap();
        assert_eq!(ids, [unreflected].into_iter().collect::<HashSet<i64>>());
    }

    #[test]
    fn record_engagement_writes_shown_and_engaged_flags() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_engagement(&conn, "sess1", &[1, 2, 3], &[2], "2026-09-23T00:00:00Z").unwrap();

        let mut stmt = conn
            .prepare("SELECT memory_id, engaged, judged_at FROM recall_engagement WHERE session_id = 'sess1' ORDER BY memory_id")
            .unwrap();
        let rows: Vec<(i64, i64, String)> =
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(
            rows,
            vec![
                (1, 0, "2026-09-23T00:00:00Z".to_string()),
                (2, 1, "2026-09-23T00:00:00Z".to_string()),
                (3, 0, "2026-09-23T00:00:00Z".to_string()),
            ]
        );
    }

    #[test]
    fn record_engagement_is_idempotent_and_overwrites_on_rerun() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_engagement(&conn, "sess1", &[1], &[], "2026-09-23T00:00:00Z").unwrap();
        record_engagement(&conn, "sess1", &[1], &[1], "2026-09-23T01:00:00Z").unwrap();

        let n: i64 = conn.query_row("SELECT count(*) FROM recall_engagement", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "re-running the same session overwrites, never duplicates");
        let (engaged, judged_at): (i64, String) = conn
            .query_row(
                "SELECT engaged, judged_at FROM recall_engagement WHERE session_id = 'sess1' AND memory_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(engaged, 1, "the second run's verdict wins");
        assert_eq!(judged_at, "2026-09-23T01:00:00Z");
    }

    #[test]
    fn record_engagement_distinguishes_sessions_sharing_a_memory_id() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_engagement(&conn, "sessA", &[7], &[7], "2026-09-23T00:00:00Z").unwrap();
        record_engagement(&conn, "sessB", &[7], &[], "2026-09-23T00:00:01Z").unwrap();

        let n: i64 = conn.query_row("SELECT count(*) FROM recall_engagement", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "same memory id in two sessions is two rows, not a collision");
    }

    #[test]
    fn recall_stats_since_groups_by_session_and_filters_the_window() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_engagement(&conn, "sessA", &[1, 2], &[1], "2026-09-20T00:00:00Z").unwrap();
        record_engagement(&conn, "sessA", &[3], &[3], "2026-09-21T00:00:00Z").unwrap();
        record_engagement(&conn, "sessB", &[4], &[], "2026-09-01T00:00:00Z").unwrap(); // outside window

        let rows = recall_stats_since(&conn, "2026-09-10T00:00:00Z").unwrap();
        assert_eq!(rows, vec![RecallStatsRow { session_id: "sessA".to_string(), shown: 3, engaged: 2 }]);
    }

    #[test]
    fn recall_stats_since_empty_table_is_empty() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let rows = recall_stats_since(&conn, "2000-01-01T00:00:00Z").unwrap();
        assert!(rows.is_empty());
    }

    // --- retention: judge_log / recall_engagement ---

    #[test]
    fn prune_judge_log_deletes_older_rows_keeps_newer_and_returns_count() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        log_judge_call(&conn, "dedupe", "haiku", "prompt A", Ok("DISTINCT"), 100, "2026-01-01T00:00:00Z").unwrap();
        log_judge_call(&conn, "dedupe", "haiku", "prompt B", Ok("DISTINCT"), 100, "2026-06-01T00:00:00Z").unwrap();
        log_judge_call(&conn, "dedupe", "haiku", "prompt C", Ok("DISTINCT"), 100, "2026-09-01T00:00:00Z").unwrap();

        let pruned = prune_judge_log(&conn, "2026-03-01T00:00:00Z").unwrap();
        assert_eq!(pruned, 1, "only the 2026-01-01 row is older than the cutoff");

        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM judge_log", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 2);
        let mut stmt = conn.prepare("SELECT prompt FROM judge_log ORDER BY created_at").unwrap();
        let prompts: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(prompts, vec!["prompt B".to_string(), "prompt C".to_string()], "newer rows survive untouched");
    }

    #[test]
    fn prune_judge_log_never_touches_supersession_audit() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_supersession_audit(&conn, 1, 2, "OK", "identical facts", "2020-01-01T00:00:00Z").unwrap();
        log_judge_call(&conn, "dedupe", "haiku", "an old prompt", Ok("DISTINCT"), 100, "2020-01-01T00:00:00Z").unwrap();

        prune_judge_log(&conn, "2026-01-01T00:00:00Z").unwrap();

        let audit_count: i64 = conn.query_row("SELECT COUNT(*) FROM supersession_audit", [], |r| r.get(0)).unwrap();
        assert_eq!(audit_count, 1, "supersession_audit is a permanent record and must never be pruned");
    }

    #[test]
    fn prune_recall_engagement_deletes_older_rows_keeps_newer_and_returns_count() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_engagement(&conn, "sess-old", &[1], &[1], "2025-01-01T00:00:00Z").unwrap();
        record_engagement(&conn, "sess-mid", &[2], &[], "2026-06-01T00:00:00Z").unwrap();
        record_engagement(&conn, "sess-new", &[3], &[3], "2026-09-01T00:00:00Z").unwrap();

        let pruned = prune_recall_engagement(&conn, "2026-01-01T00:00:00Z").unwrap();
        assert_eq!(pruned, 1, "only sess-old's row is older than the cutoff");

        let remaining: i64 = conn.query_row("SELECT COUNT(*) FROM recall_engagement", [], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 2);
        let mut stmt = conn.prepare("SELECT session_id FROM recall_engagement ORDER BY session_id").unwrap();
        let sessions: Vec<String> = stmt.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(sessions, vec!["sess-mid".to_string(), "sess-new".to_string()], "newer rows survive untouched");
    }

    #[test]
    fn prune_recall_engagement_never_touches_supersession_audit() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        record_supersession_audit(&conn, 1, 2, "OK", "identical facts", "2020-01-01T00:00:00Z").unwrap();
        record_engagement(&conn, "sess-old", &[1], &[], "2020-01-01T00:00:00Z").unwrap();

        prune_recall_engagement(&conn, "2026-01-01T00:00:00Z").unwrap();

        let audit_count: i64 = conn.query_row("SELECT COUNT(*) FROM supersession_audit", [], |r| r.get(0)).unwrap();
        assert_eq!(audit_count, 1, "supersession_audit is a permanent record and must never be pruned");
    }

    // -------------------------------------------------------------
    // Code index
    // -------------------------------------------------------------

    fn new_project(conn: &Connection, name: &str) -> i64 {
        upsert_project(conn, &format!("fp-{name}"), name, &format!("/repo/{name}"), "2026-09-23T00:00:00Z").unwrap().0
    }

    fn a_chunk(symbol: &str, text: &str) -> NewCodeChunk {
        NewCodeChunk {
            symbol: Some(symbol.to_string()),
            kind: "function".to_string(),
            scope: None,
            start_line: 1,
            end_line: 10,
            text: text.to_string(),
            header: None,
            embedding: None,
            content_hash: format!("hash-{symbol}"),
        }
    }

    #[test]
    fn code_scope_set_never_lets_an_llm_write_overwrite_a_user_row() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");

        code_scope_set(&conn, pid, "src/vendor", "vendored", "user", "2026-09-20T00:00:00Z").unwrap();
        code_scope_set(&conn, pid, "src/vendor", "product", "llm", "2026-09-23T00:00:00Z").unwrap();

        let rows = code_scope_get(&conn, pid).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].category, "vendored", "the llm write must not clobber the user's override");
        assert_eq!(rows[0].source, "user");
        assert_eq!(rows[0].decided_at, "2026-09-20T00:00:00Z");

        // A later user write still updates a user row.
        code_scope_set(&conn, pid, "src/vendor", "assets", "user", "2026-09-24T00:00:00Z").unwrap();
        let rows = code_scope_get(&conn, pid).unwrap();
        assert_eq!(rows[0].category, "assets");

        // And an llm write still lands on a directory with no prior decision.
        code_scope_set(&conn, pid, "src/core", "product", "llm", "2026-09-23T00:00:00Z").unwrap();
        let rows = code_scope_get(&conn, pid).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn replace_code_chunks_replaces_rows_and_keeps_fts_in_sync() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");

        let ids1 = replace_code_chunks(&conn, pid, "src/main.cpp", &[a_chunk("old_symbol", "old body text kumquatzzy")]).unwrap();
        assert_eq!(ids1.len(), 1);

        let before = search_code_chunks(&conn, Some(pid), "kumquatzzy", None, 10).unwrap();
        assert_eq!(before.len(), 1, "the old text must be found before the file is re-chunked");

        let ids2 = replace_code_chunks(&conn, pid, "src/main.cpp", &[a_chunk("new_symbol", "new body text wombatqqq")]).unwrap();
        assert_eq!(ids2.len(), 1);
        // `code_chunks.id` is a plain `INTEGER PRIMARY KEY` (no
        // AUTOINCREMENT, per the exact schema), so SQLite is free to reuse
        // `ids1[0]` here once the table is empty -- that's expected, not a
        // bug; what matters is the FTS index tracking whichever id is live.

        let old_hits = search_code_chunks(&conn, Some(pid), "kumquatzzy", None, 10).unwrap();
        assert!(old_hits.is_empty(), "the old chunk's text must no longer be findable via FTS");

        let new_hits = search_code_chunks(&conn, Some(pid), "wombatqqq", None, 10).unwrap();
        assert_eq!(new_hits.len(), 1, "the new chunk's text must be findable via FTS");
        assert_eq!(new_hits[0].id, ids2[0]);

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM code_chunks WHERE project_id = ?1 AND path = ?2", params![pid, "src/main.cpp"], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the old row must actually be gone, not just unindexed");
    }

    #[test]
    fn set_code_chunk_header_keeps_fts_searchable_by_the_new_header() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let ids = replace_code_chunks(&conn, pid, "src/a.rs", &[a_chunk("do_thing", "fn body")]).unwrap();

        set_code_chunk_header(&conn, ids[0], "handles the frobnicator_widget subsystem").unwrap();

        let hits = search_code_chunks(&conn, Some(pid), "frobnicator_widget", None, 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].header.as_deref(), Some("handles the frobnicator_widget subsystem"));
    }

    #[test]
    fn search_code_chunks_filters_by_project_and_ranks_an_exact_symbol_match_first() {
        let conn = mem_conn();
        let helios = new_project(&conn, "helios");
        let other = new_project(&conn, "other");

        // In `helios`: one chunk whose SYMBOL is the query, one chunk that
        // only mentions the same token in its body text.
        replace_code_chunks(&conn, helios, "src/picking.cpp", &[a_chunk("ray_cast_terrain", "definition of ray_cast_terrain")]).unwrap();
        replace_code_chunks(&conn, helios, "src/notes.cpp", &[a_chunk("unrelated_fn", "calls ray_cast_terrain from elsewhere")]).unwrap();
        // In `other`: a same-named symbol that must never surface when the
        // search is scoped to `helios`.
        replace_code_chunks(&conn, other, "src/picking.cpp", &[a_chunk("ray_cast_terrain", "a different project's own definition")]).unwrap();

        let hits = search_code_chunks(&conn, Some(helios), "ray_cast_terrain", None, 10).unwrap();
        assert_eq!(hits.len(), 2, "only helios's two chunks must be returned");
        assert!(hits.iter().all(|h| h.project_id == helios), "no hit from the other project must leak through");
        assert_eq!(
            hits[0].symbol.as_deref(),
            Some("ray_cast_terrain"),
            "the chunk whose symbol IS the query must outrank the one that merely mentions it in text"
        );
        assert!(hits[0].score > hits[1].score);
    }

    // -------------------------------------------------------------
    // Code summaries (phase 2, schema 33)
    // -------------------------------------------------------------

    #[test]
    fn code_summary_upsert_then_get_round_trips_every_field() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");

        code_summary_upsert(&conn, pid, "src/picking.cpp", "file", "does terrain picking", "digest-1", "2026-09-24T00:00:00Z")
            .unwrap();

        let row = code_summary_get(&conn, pid, "src/picking.cpp").unwrap().unwrap();
        assert_eq!(row.project_id, pid);
        assert_eq!(row.path, "src/picking.cpp");
        assert_eq!(row.level, "file");
        assert_eq!(row.text, "does terrain picking");
        assert_eq!(row.source_digest, "digest-1");
        assert!(!row.stale);
        assert!(row.stale_since.is_none());
        assert!(row.embedding.is_none());
        assert!(row.memory_id.is_none());
        assert_eq!(row.updated_at, "2026-09-24T00:00:00Z");
    }

    #[test]
    fn code_summary_get_on_a_never_summarised_path_is_none() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        assert!(code_summary_get(&conn, pid, "src/never.cpp").unwrap().is_none());
    }

    #[test]
    fn code_summary_upsert_clears_stale_on_regeneration() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "v1", "digest-1", "2026-09-24T00:00:00Z").unwrap();
        mark_code_summaries_stale(&conn, pid, &["src/a.cpp".to_string()], "2026-09-24T01:00:00Z").unwrap();
        assert!(code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap().stale);

        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "v2, regenerated", "digest-2", "2026-09-24T02:00:00Z").unwrap();

        let row = code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap();
        assert!(!row.stale, "regenerating must clear stale");
        assert!(row.stale_since.is_none(), "regenerating must clear stale_since too");
        assert_eq!(row.text, "v2, regenerated");
        assert_eq!(row.source_digest, "digest-2");
    }

    #[test]
    fn code_summary_upsert_clears_a_previous_embedding_on_regeneration() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "v1", "digest-1", "2026-09-24T00:00:00Z").unwrap();
        set_code_summary_embedding(&conn, pid, "src/a.cpp", &fake_embed("v1")).unwrap();
        assert!(code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap().embedding.is_some());

        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "v2", "digest-2", "2026-09-24T01:00:00Z").unwrap();

        assert!(
            code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap().embedding.is_none(),
            "text and its embedding must never drift apart -- a regenerated summary needs a fresh embedding"
        );
    }

    #[test]
    fn mark_code_summaries_stale_keeps_the_first_stale_since_timestamp() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "v1", "digest-1", "2026-09-24T00:00:00Z").unwrap();

        mark_code_summaries_stale(&conn, pid, &["src/a.cpp".to_string()], "2026-09-24T01:00:00Z").unwrap();
        let first = code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap();
        assert_eq!(first.stale_since.as_deref(), Some("2026-09-24T01:00:00Z"));

        // A second sibling change re-marks it stale (still stale) but must
        // not push stale_since forward -- the "how long has this been
        // stale" clock starts at the FIRST staleness, not the latest.
        mark_code_summaries_stale(&conn, pid, &["src/a.cpp".to_string()], "2026-09-24T05:00:00Z").unwrap();
        let second = code_summary_get(&conn, pid, "src/a.cpp").unwrap().unwrap();
        assert!(second.stale);
        assert_eq!(second.stale_since.as_deref(), Some("2026-09-24T01:00:00Z"), "stale_since must not move once set");
    }

    #[test]
    fn mark_code_summaries_stale_is_scoped_to_the_given_project_and_paths() {
        let conn = mem_conn();
        let helios = new_project(&conn, "helios");
        let other = new_project(&conn, "other");
        code_summary_upsert(&conn, helios, "src/a.cpp", "file", "a", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, helios, "src/b.cpp", "file", "b", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, other, "src/a.cpp", "file", "a-other", "d", "2026-09-24T00:00:00Z").unwrap();

        let n = mark_code_summaries_stale(&conn, helios, &["src/a.cpp".to_string()], "2026-09-24T01:00:00Z").unwrap();
        assert_eq!(n, 1);
        assert!(code_summary_get(&conn, helios, "src/a.cpp").unwrap().unwrap().stale);
        assert!(!code_summary_get(&conn, helios, "src/b.cpp").unwrap().unwrap().stale, "an unrelated path must not be marked");
        assert!(!code_summary_get(&conn, other, "src/a.cpp").unwrap().unwrap().stale, "another project's same-named path must not be marked");
    }

    #[test]
    fn mark_code_summaries_stale_with_no_paths_is_a_noop() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        assert_eq!(mark_code_summaries_stale(&conn, pid, &[], "2026-09-24T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn stale_code_summaries_returns_only_the_stale_rows_of_one_project_path_ascending() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/z.cpp", "file", "z", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "a", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, pid, "src/m.cpp", "file", "m", "d", "2026-09-24T00:00:00Z").unwrap();
        mark_code_summaries_stale(&conn, pid, &["src/z.cpp".to_string(), "src/a.cpp".to_string()], "2026-09-24T01:00:00Z").unwrap();

        let stale = stale_code_summaries(&conn, pid).unwrap();
        assert_eq!(stale.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(), vec!["src/a.cpp", "src/z.cpp"]);
    }

    #[test]
    fn code_summary_delete_removes_the_row_and_its_fts_entry() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "unique kumquatzzy content", "d", "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(search_code_summaries(&conn, Some(pid), "kumquatzzy", None, 10).unwrap().len(), 1);

        code_summary_delete(&conn, pid, "src/a.cpp").unwrap();

        assert!(code_summary_get(&conn, pid, "src/a.cpp").unwrap().is_none());
        assert!(search_code_summaries(&conn, Some(pid), "kumquatzzy", None, 10).unwrap().is_empty());
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM code_summaries WHERE project_id = ?1 AND path = ?2", params![pid, "src/a.cpp"], |r| r.get(0)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn code_summaries_missing_embedding_returns_only_unembedded_rows_oldest_first() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "a", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, pid, "src/b.cpp", "file", "b", "d", "2026-09-24T00:00:00Z").unwrap();
        set_code_summary_embedding(&conn, pid, "src/a.cpp", &fake_embed("a")).unwrap();

        let missing = code_summaries_missing_embedding(&conn, None, 10).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].0, pid);
        assert_eq!(missing[0].1, "src/b.cpp");
        assert_eq!(missing[0].2, "b");
    }

    #[test]
    fn code_summaries_missing_embedding_filters_by_project() {
        let conn = mem_conn();
        let helios = new_project(&conn, "helios");
        let other = new_project(&conn, "other");
        code_summary_upsert(&conn, helios, "src/a.cpp", "file", "a", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, other, "src/b.cpp", "file", "b", "d", "2026-09-24T00:00:00Z").unwrap();

        let missing = code_summaries_missing_embedding(&conn, Some(helios), 10).unwrap();
        assert_eq!(missing.len(), 1, "must not see the other project's row");
        assert_eq!(missing[0].0, helios);
        assert_eq!(missing[0].1, "src/a.cpp");

        assert_eq!(code_summaries_missing_embedding(&conn, None, 10).unwrap().len(), 2, "None still sees every project");
    }

    #[test]
    fn code_file_increment_summary_attempts_counts_up_and_persists() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_file_upsert(&conn, pid, "src/a.cpp", "blob1", Some("Cpp"), 3, "indexed", "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(code_file_increment_summary_attempts(&conn, pid, "src/a.cpp").unwrap(), 1);
        assert_eq!(code_file_increment_summary_attempts(&conn, pid, "src/a.cpp").unwrap(), 2);
        assert_eq!(code_file_get(&conn, pid, "src/a.cpp").unwrap().unwrap().summary_attempts, 2);
    }

    #[test]
    fn code_file_upsert_resets_summary_attempts_only_when_the_blob_changes() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_file_upsert(&conn, pid, "src/a.cpp", "blob1", Some("Cpp"), 3, "indexed", "2026-09-24T00:00:00Z").unwrap();
        code_file_increment_summary_attempts(&conn, pid, "src/a.cpp").unwrap();
        code_file_increment_summary_attempts(&conn, pid, "src/a.cpp").unwrap();

        // Same blob re-upserted (e.g. mark_skipped re-recording an
        // unchanged file): the count survives.
        code_file_upsert(&conn, pid, "src/a.cpp", "blob1", Some("Cpp"), 3, "indexed", "2026-09-24T01:00:00Z").unwrap();
        assert_eq!(code_file_get(&conn, pid, "src/a.cpp").unwrap().unwrap().summary_attempts, 2);

        // A new blob resets it -- fresh content gets fresh tries.
        code_file_upsert(&conn, pid, "src/a.cpp", "blob2", Some("Cpp"), 3, "indexed", "2026-09-24T02:00:00Z").unwrap();
        assert_eq!(code_file_get(&conn, pid, "src/a.cpp").unwrap().unwrap().summary_attempts, 0);
    }

    #[test]
    fn code_summary_seed_placeholder_records_stale_since_and_is_idempotent() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_seed_placeholder(&conn, pid, "src/", "module", "2026-09-24T00:00:00Z").unwrap();
        let row = code_summary_get(&conn, pid, "src/").unwrap().unwrap();
        assert!(row.stale);
        assert_eq!(row.stale_since.as_deref(), Some("2026-09-24T00:00:00Z"));
        assert_eq!(row.level, "module");
        assert_eq!(row.text, "");

        // Idempotent: a second seed call (a later run, still not ready)
        // must never move stale_since forward.
        code_summary_seed_placeholder(&conn, pid, "src/", "module", "2026-09-25T00:00:00Z").unwrap();
        let row2 = code_summary_get(&conn, pid, "src/").unwrap().unwrap();
        assert_eq!(row2.stale_since.as_deref(), Some("2026-09-24T00:00:00Z"));
    }

    #[test]
    fn code_summary_seed_placeholder_never_overwrites_a_real_row() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/", "module", "real summary", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_seed_placeholder(&conn, pid, "src/", "module", "2026-09-25T00:00:00Z").unwrap();
        let row = code_summary_get(&conn, pid, "src/").unwrap().unwrap();
        assert_eq!(row.text, "real summary", "a real row must never be clobbered by a placeholder seed");
        assert!(!row.stale);
    }

    #[test]
    fn set_code_summary_memory_id_persists_and_is_read_back() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/", "module", "module summary", "d", "2026-09-24T00:00:00Z").unwrap();
        set_code_summary_memory_id(&conn, pid, "src/", 42).unwrap();
        assert_eq!(code_summary_get(&conn, pid, "src/").unwrap().unwrap().memory_id, Some(42));
    }

    #[test]
    fn replace_code_chunks_fts_sync_pattern_holds_for_code_summaries_too() {
        // Same "old text unfindable, new text findable" contract
        // `replace_code_chunks_replaces_rows_and_keeps_fts_in_sync` checks for
        // chunks, exercised here for the summary table's own FTS index.
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "old body text kumquatzzy", "d1", "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(search_code_summaries(&conn, Some(pid), "kumquatzzy", None, 10).unwrap().len(), 1);

        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "new body text wombatqqq", "d2", "2026-09-24T01:00:00Z").unwrap();

        assert!(
            search_code_summaries(&conn, Some(pid), "kumquatzzy", None, 10).unwrap().is_empty(),
            "the old text must no longer be findable via FTS"
        );
        let new_hits = search_code_summaries(&conn, Some(pid), "wombatqqq", None, 10).unwrap();
        assert_eq!(new_hits.len(), 1, "the new text must be findable via FTS");
        assert_eq!(new_hits[0].path, "src/a.cpp");
    }

    #[test]
    fn search_code_summaries_filters_by_project() {
        let conn = mem_conn();
        let helios = new_project(&conn, "helios");
        let other = new_project(&conn, "other");
        code_summary_upsert(&conn, helios, "src/a.cpp", "file", "definition of ray_cast_terrain", "d", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, other, "src/a.cpp", "file", "a different project's ray_cast_terrain", "d", "2026-09-24T00:00:00Z").unwrap();

        let hits = search_code_summaries(&conn, Some(helios), "ray_cast_terrain", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "only helios's summary must be returned");
        assert_eq!(hits[0].project_id, helios);
    }

    #[test]
    fn search_code_summaries_ranks_a_closer_lexical_match_first() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(
            &conn,
            pid,
            "src/picking.cpp",
            "file",
            "ray_cast_terrain ray_cast_terrain ray_cast_terrain implementation",
            "d",
            "2026-09-24T00:00:00Z",
        )
        .unwrap();
        code_summary_upsert(&conn, pid, "src/notes.cpp", "file", "mentions ray_cast_terrain once in passing", "d", "2026-09-24T00:00:00Z")
            .unwrap();

        let hits = search_code_summaries(&conn, Some(pid), "ray_cast_terrain", None, 10).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, "src/picking.cpp");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn search_code_summaries_semantic_channel_finds_a_summary_with_no_lexical_overlap() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "alpha beta gamma", "d", "2026-09-24T00:00:00Z").unwrap();
        set_code_summary_embedding(&conn, pid, "src/a.cpp", &fake_embed("alpha beta gamma")).unwrap();

        let hits = search_code_summaries(&conn, Some(pid), "zzz_no_lexical_match_zzz", Some(&fake_embed("alpha beta gamma")), 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "src/a.cpp");
        assert!(hits[0].score > 0.0);
    }

    // -------------------------------------------------------------
    // Index-owned memories (upsert_index_memory)
    // -------------------------------------------------------------

    #[test]
    fn upsert_index_memory_supersedes_every_active_row_with_that_source_including_a_restored_pinned_one() {
        // Final-review item 5: a restored (hence pinned) old index row used
        // to stay active forever beside the new one -- the old code
        // superseded only the newest active row.
        let conn = mem_conn();
        let source = "code-index:helios:src/";
        let a = upsert_index_memory(&conn, "helios", source, "gen 1", None, "2026-09-24T00:00:00Z").unwrap();
        let b = upsert_index_memory(&conn, "helios", source, "gen 2", None, "2026-09-24T01:00:00Z").unwrap();
        assert!(restore(&conn, a, "2026-09-24T02:00:00Z").unwrap());
        assert!(is_pinned(&conn, a).unwrap());
        let c = upsert_index_memory(&conn, "helios", source, "gen 3", None, "2026-09-24T03:00:00Z").unwrap();
        for old in [a, b] {
            let m = get(&conn, old).unwrap().unwrap();
            assert!(m.invalidated_at.is_some(), "#{} must be superseded", old);
            assert_eq!(m.superseded_by, Some(c));
        }
        let active: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE source = ?1 AND invalidated_at IS NULL", params![source], |r| r.get(0))
            .unwrap();
        assert_eq!(active, 1);
    }

    #[test]
    fn upsert_index_memory_for_summary_records_the_memory_id_on_the_summary_row() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/", "module", "src text", "d", "2026-09-24T00:00:00Z").unwrap();
        let id = upsert_index_memory_for_summary(&conn, pid, "src/", "helios", "code-index:helios:src/", "lead", None, "2026-09-24T00:00:00Z")
            .unwrap();
        assert_eq!(code_summary_get(&conn, pid, "src/").unwrap().unwrap().memory_id, Some(id));
    }

    #[test]
    fn upsert_index_memory_never_links_mentions() {
        // Final-review item 7: index rows stay out of the mention graph.
        let conn = mem_conn();
        let ent = insert_entity(&conn, "Picker", None, None).unwrap();
        let id = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ holds the Picker class", None, "2026-09-24T00:00:00Z").unwrap();
        let count = |eid: i64| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM memory_entities WHERE memory_id = ?1 AND entity_id = ?2", params![id, eid], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count(ent), 0);
        // Nor does the reverse scan (a newly minted entity) reach index rows.
        let ent2 = insert_entity(&conn, "holds", None, None).unwrap();
        link_entity_mentions_by_name_scan(&conn, ent2, "holds", "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(count(ent2), 0);

        // Nor a code-history-owned row (task-3 extension of the same rule).
        let id2 = upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: touches Picker", None, "2026-09-24T00:00:00Z").unwrap();
        let count2 = |eid: i64| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM memory_entities WHERE memory_id = ?1 AND entity_id = ?2", params![id2, eid], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(count2(ent), 0);
        let ent3 = insert_entity(&conn, "touches", None, None).unwrap();
        link_entity_mentions_by_name_scan(&conn, ent3, "touches", "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(count2(ent3), 0);
    }

    #[test]
    fn invalidate_index_memories_by_source_tombstones_every_active_row() {
        let conn = mem_conn();
        let source = "code-index:helios:gone/";
        let a = upsert_index_memory(&conn, "helios", source, "gen 1", None, "2026-09-24T00:00:00Z").unwrap();
        let b = upsert_index_memory(&conn, "helios", source, "gen 2", None, "2026-09-24T01:00:00Z").unwrap();
        restore(&conn, a, "2026-09-24T02:00:00Z").unwrap();
        let other = upsert_index_memory(&conn, "helios", "code-index:helios:kept/", "kept", None, "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(invalidate_index_memories_by_source(&conn, source, "2026-09-25T00:00:00Z").unwrap(), 2);
        assert!(get(&conn, a).unwrap().unwrap().invalidated_at.is_some());
        assert!(get(&conn, b).unwrap().unwrap().invalidated_at.is_some());
        assert!(get(&conn, other).unwrap().unwrap().invalidated_at.is_none());
    }

    #[test]
    fn placeholder_summaries_are_never_backfilled_or_searched() {
        // Final-review item 3.
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_seed_placeholder(&conn, pid, "src/", "module", "2026-09-24T00:00:00Z").unwrap();
        code_summary_upsert(&conn, pid, "src/a.cpp", "file", "src picking text", "d", "2026-09-24T00:00:00Z").unwrap();
        let missing: Vec<String> = code_summaries_missing_embedding(&conn, Some(pid), 10).unwrap().into_iter().map(|r| r.1).collect();
        assert_eq!(missing, vec!["src/a.cpp".to_string()]);

        // Even a placeholder that somehow holds an embedding stays out of search.
        set_code_summary_embedding(&conn, pid, "src/", &fake_embed("src")).unwrap();
        let hits = search_code_summaries(&conn, Some(pid), "src", Some(&fake_embed("src")), 10).unwrap();
        assert!(hits.iter().all(|h| h.path != "src/"), "a placeholder must never be a search hit");
    }

    #[test]
    fn code_summary_placeholderize_clears_text_stales_and_returns_the_mirror_id() {
        // Final-review item 13.
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_summary_upsert(&conn, pid, "src/", "module", "old src text mentioning a secret file", "d", "2026-09-24T00:00:00Z").unwrap();
        set_code_summary_embedding(&conn, pid, "src/", &fake_embed("x")).unwrap();
        set_code_summary_memory_id(&conn, pid, "src/", 42).unwrap();
        assert_eq!(code_summary_placeholderize(&conn, pid, "src/", "2026-09-25T00:00:00Z").unwrap(), Some(42));
        let row = code_summary_get(&conn, pid, "src/").unwrap().unwrap();
        assert_eq!(row.text, "");
        assert!(row.stale);
        assert!(row.embedding.is_none());
        assert!(row.memory_id.is_none());
        assert!(search_code_summaries(&conn, Some(pid), "secret", None, 10).unwrap().is_empty(), "FTS entry cleared too");
        assert_eq!(code_summary_placeholderize(&conn, pid, "missing/", "2026-09-25T00:00:00Z").unwrap(), None);
    }

    #[test]
    fn top_similar_never_offers_an_index_owned_row_to_the_save_time_classifier() {
        let conn = mem_conn();
        let v = [1.0f32, 0.0, 0.0];
        upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking", Some(&v), "2026-09-24T00:00:00Z").unwrap();
        let user = insert5(&conn, "src/ handles picking", Some(&v));
        let ids: Vec<i64> = top_similar(&conn, &v, 5).unwrap().into_iter().map(|(m, _)| m.id).collect();
        assert_eq!(ids, vec![user]);
    }

    #[test]
    fn top_similar_never_offers_a_code_history_owned_row_either() {
        let conn = mem_conn();
        let v = [1.0f32, 0.0, 0.0];
        upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: aug work", Some(&v), "2026-09-24T00:00:00Z")
            .unwrap();
        let user = insert5(&conn, "aug work", Some(&v));
        let ids: Vec<i64> = top_similar(&conn, &v, 5).unwrap().into_iter().map(|(m, _)| m.id).collect();
        assert_eq!(ids, vec![user]);
    }

    #[test]
    fn upsert_index_memory_first_call_is_a_plain_insert() {
        let conn = mem_conn();
        let id = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "helios's src/ handles picking", None, "2026-09-24T00:00:00Z")
            .unwrap();

        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.content, "helios's src/ handles picking");
        assert_eq!(m.source.as_deref(), Some("code-index:helios:src/"));
        assert_eq!(m.project.as_deref(), Some("helios"));
        assert_eq!(m.importance, 6);
        assert_eq!(m.basis.as_deref(), Some(BASIS_DERIVED));
        assert!(m.reviewed);
        assert!(m.invalidated_at.is_none());
    }

    #[test]
    fn upsert_index_memory_sets_reflected_at_to_now_immediately() {
        let conn = mem_conn();
        let id = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "text", None, "2026-09-24T00:00:00Z").unwrap();
        let reflected_at: Option<String> =
            conn.query_row("SELECT reflected_at FROM memories WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(reflected_at.as_deref(), Some("2026-09-24T00:00:00Z"));
    }

    #[test]
    fn upsert_index_memory_regenerating_supersedes_the_previous_row() {
        let conn = mem_conn();
        let source = "code-index:helios:src/";
        let first = upsert_index_memory(&conn, "helios", source, "src/ handles picking", None, "2026-09-24T00:00:00Z").unwrap();
        let second = upsert_index_memory(&conn, "helios", source, "src/ handles picking and rendering now", None, "2026-09-24T01:00:00Z").unwrap();

        assert_ne!(first, second);
        let old = get(&conn, first).unwrap().unwrap();
        assert_eq!(old.superseded_by, Some(second));
        assert!(old.invalidated_at.is_some());
        let new = get(&conn, second).unwrap().unwrap();
        assert!(new.invalidated_at.is_none());

        // Exactly one active row for this source.
        let active: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM memories WHERE source = ?1 AND invalidated_at IS NULL",
                params![source],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(active, 1);
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(total, 2, "the superseded row must still exist as an audit trail, not be deleted");
    }

    #[test]
    fn upsert_index_memory_three_generations_leaves_exactly_one_active_row() {
        let conn = mem_conn();
        let source = "code-index:helios:/";
        upsert_index_memory(&conn, "helios", source, "gen 1", None, "2026-09-24T00:00:00Z").unwrap();
        upsert_index_memory(&conn, "helios", source, "gen 2", None, "2026-09-24T01:00:00Z").unwrap();
        let third = upsert_index_memory(&conn, "helios", source, "gen 3", None, "2026-09-24T02:00:00Z").unwrap();

        let active_ids: Vec<i64> = {
            let mut stmt = conn.prepare("SELECT id FROM memories WHERE source = ?1 AND invalidated_at IS NULL").unwrap();
            stmt.query_map(params![source], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
        };
        assert_eq!(active_ids, vec![third]);
    }

    #[test]
    fn upsert_index_memory_different_sources_never_interfere() {
        let conn = mem_conn();
        let a = upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src summary", None, "2026-09-24T00:00:00Z").unwrap();
        let b = upsert_index_memory(&conn, "helios", "code-index:helios:tools/", "tools summary", None, "2026-09-24T00:00:00Z").unwrap();
        assert!(get(&conn, a).unwrap().unwrap().invalidated_at.is_none());
        assert!(get(&conn, b).unwrap().unwrap().invalidated_at.is_none());
    }

    // -------------------------------------------------------------
    // supersession_guard: index-owned losers
    // -------------------------------------------------------------

    #[test]
    fn supersession_guard_blocks_an_index_owned_loser_for_every_automatic_kind() {
        let conn = mem_conn();
        let loser_id =
            upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking", None, "2026-09-24T00:00:00Z").unwrap();
        let winner_id = insert5(&conn, "src/ handles picking and rendering", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();

        for kind in [GuardKind::Dedupe, GuardKind::Contradiction, GuardKind::AddSupersede] {
            assert_eq!(supersession_guard(&conn, &loser, &winner, kind), Some("index-owned"), "{:?}", kind);
        }
    }

    #[test]
    fn supersession_guard_blocks_a_code_history_owned_loser_for_every_automatic_kind() {
        // The task-3 extension: `code-history:` must be guarded exactly
        // like `code-index:`, via the same shared helper.
        let conn = mem_conn();
        let loser_id =
            upsert_index_memory(&conn, "helios", "code-history:helios:2026-08", "helios 2026-08: aug work", None, "2026-09-24T00:00:00Z")
                .unwrap();
        let winner_id = insert5(&conn, "helios 2026-08: aug work, revised", None);
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();

        for kind in [GuardKind::Dedupe, GuardKind::Contradiction, GuardKind::AddSupersede] {
            assert_eq!(supersession_guard(&conn, &loser, &winner, kind), Some("index-owned"), "{:?}", kind);
        }
    }

    #[test]
    fn supersession_guard_blocks_when_only_the_winner_is_index_owned() {
        // An index-owned winner would tombstone a user memory into text the
        // indexer replaces wholesale on the next regeneration -- data loss.
        let conn = mem_conn();
        let loser_id = insert5(&conn, "src/ handles picking", None);
        let winner_id =
            upsert_index_memory(&conn, "helios", "code-index:helios:src/", "src/ handles picking and rendering", None, "2026-09-24T00:00:00Z")
                .unwrap();
        let loser = get(&conn, loser_id).unwrap().unwrap();
        let winner = get(&conn, winner_id).unwrap().unwrap();
        for kind in [GuardKind::Dedupe, GuardKind::Contradiction, GuardKind::AddSupersede] {
            assert_eq!(supersession_guard(&conn, &loser, &winner, kind), Some("index-owned"), "{:?}", kind);
        }
    }

    #[test]
    fn insert_with_basis_accepts_derived() {
        let conn = mem_conn();
        let id = insert_with_basis(&conn, "code-derived text", None, None, true, None, 6, Some(BASIS_DERIVED)).unwrap();
        assert_eq!(get(&conn, id).unwrap().unwrap().basis.as_deref(), Some(BASIS_DERIVED));
    }

    // -------------------------------------------------------------
    // Code symbol graph (schema 35): migration, replace/delete/move,
    // resolve_edges, symbol_definitions, callers_of/callees_of,
    // code_history. See `code_index::chunk::extract_refs`'s own tests for
    // extraction correctness -- these test the store side only, plus one
    // end-to-end round trip through both.
    // -------------------------------------------------------------

    fn a_symbol(name: &str, qualified: &str, kind: &str, start: i64, end: i64) -> NewSymbol {
        NewSymbol { name: name.to_string(), qualified: qualified.to_string(), kind: kind.to_string(), start_line: start, end_line: end }
    }

    fn an_edge(src_line: i64, dst_name: &str, kind: &str) -> NewEdge {
        NewEdge { src_line, dst_name: dst_name.to_string(), kind: kind.to_string() }
    }

    #[test]
    fn migrate_v34_to_v35_is_idempotent() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        migrate_v34_to_v35(&conn).unwrap();
        migrate_v34_to_v35(&conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 35);
    }

    #[test]
    fn a_fresh_open_lands_on_v35_with_the_symbol_graph_tables() {
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION, "a fresh open lands on the latest schema, past v34");
        for table in ["code_symbols", "code_edges", "code_history"] {
            let exists: i64 = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", params![table], |r| r.get(0))
                .unwrap();
            assert_eq!(exists, 1, "{table} must exist after a fresh open");
        }
    }

    #[test]
    fn replace_file_symbols_resolves_src_symbol_id_by_enclosing_line_range() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let symbols = vec![a_symbol("Foo", "Foo", "class", 1, 4), a_symbol("bar", "Foo::bar", "method", 6, 8)];
        let edges = vec![an_edge(7, "baz", "calls")];
        let ids = replace_file_symbols(&conn, pid, "t.cpp", &symbols, &edges).unwrap();
        assert_eq!(ids.len(), 2);

        let bar_id = ids[1];
        let (src_symbol_id, dst_name, kind): (Option<i64>, String, String) = conn
            .query_row("SELECT src_symbol_id, dst_name, kind FROM code_edges WHERE project_id = ?1", params![pid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!((src_symbol_id, dst_name.as_str(), kind.as_str()), (Some(bar_id), "baz", "calls"));
    }

    #[test]
    fn replace_file_symbols_picks_the_most_specific_enclosing_symbol() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        // Deliberate overlap, same as the chunker's own class+inline-method
        // overlap: a "class" unit covering the whole class, and a "method"
        // unit nested inside it. A call inside the method must resolve to
        // the method, not the wider enclosing class.
        let symbols = vec![a_symbol("Foo", "Foo", "class", 1, 5), a_symbol("bar", "Foo::bar", "method", 2, 4)];
        let edges = vec![an_edge(3, "baz", "calls")];
        let ids = replace_file_symbols(&conn, pid, "t.cpp", &symbols, &edges).unwrap();
        let bar_id = ids[1];
        let src_symbol_id: Option<i64> =
            conn.query_row("SELECT src_symbol_id FROM code_edges WHERE project_id = ?1", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(src_symbol_id, Some(bar_id));
    }

    #[test]
    fn replace_file_symbols_leaves_src_symbol_id_null_when_nothing_encloses_the_line() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let symbols = vec![a_symbol("bar", "bar", "function", 5, 8)];
        let edges = vec![an_edge(1, "std::cout", "includes")];
        replace_file_symbols(&conn, pid, "t.cpp", &symbols, &edges).unwrap();
        let src_symbol_id: Option<i64> =
            conn.query_row("SELECT src_symbol_id FROM code_edges WHERE project_id = ?1", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(src_symbol_id, None);
    }

    #[test]
    fn replace_file_symbols_replaces_old_rows_and_nulls_incoming_edges_pointing_at_removed_symbols() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let ids1 = replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("helper", "helper", "function", 1, 3)], &[]).unwrap();
        let helper_id = ids1[0];
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "helper", "calls")]).unwrap();
        resolve_edges(&conn, pid).unwrap();
        let linked: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        assert_eq!(linked, Some(helper_id));

        // a.cpp is re-chunked without `helper` any more.
        replace_file_symbols(&conn, pid, "a.cpp", &[], &[]).unwrap();
        let after: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        assert_eq!(after, None, "the removed symbol's id must not dangle in another file's edge");

        let remaining: i64 =
            conn.query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'a.cpp'", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn delete_file_symbols_removes_symbols_and_outgoing_edges_and_nulls_incoming() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("helper", "helper", "function", 1, 3)], &[]).unwrap();
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "helper", "calls")]).unwrap();
        resolve_edges(&conn, pid).unwrap();

        delete_file_symbols(&conn, pid, "a.cpp").unwrap();

        let sym_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'a.cpp'", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(sym_count, 0);
        let dst: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        assert_eq!(dst, None);

        // "a.cpp"'s own outgoing edges (had it had any) would be gone too;
        // here there were none, so this just documents the call succeeded
        // without an outgoing edge to check.
        let outgoing: i64 =
            conn.query_row("SELECT COUNT(*) FROM code_edges WHERE project_id = ?1 AND src_path = 'a.cpp'", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(outgoing, 0);
    }

    #[test]
    fn move_file_symbols_updates_path_on_symbols_and_outgoing_edges() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "old.cpp", &[a_symbol("helper", "helper", "function", 1, 3)], &[an_edge(2, "other", "calls")]).unwrap();

        move_file_symbols(&conn, pid, "old.cpp", "new.cpp").unwrap();

        let sym_path: String = conn.query_row("SELECT path FROM code_symbols WHERE project_id = ?1", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(sym_path, "new.cpp");
        let edge_path: String = conn.query_row("SELECT src_path FROM code_edges WHERE project_id = ?1", params![pid], |r| r.get(0)).unwrap();
        assert_eq!(edge_path, "new.cpp");
    }

    #[test]
    fn resolve_edges_unique_qualified_match_wins_over_name_fallback() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(
            &conn,
            pid,
            "a.cpp",
            &[a_symbol("bar", "Foo::bar", "method", 1, 3), a_symbol("bar", "Other::bar", "method", 5, 7)],
            &[],
        )
        .unwrap();
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "Foo::bar", "calls")]).unwrap();

        let n = resolve_edges(&conn, pid).unwrap();
        assert_eq!(n, 1);
        let dst: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        let expected: i64 = conn.query_row("SELECT id FROM code_symbols WHERE qualified = 'Foo::bar'", [], |r| r.get(0)).unwrap();
        assert_eq!(dst, Some(expected));
    }

    #[test]
    fn resolve_edges_falls_back_to_unique_name_match() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("baz", "Foo::baz", "method", 1, 3)], &[]).unwrap();
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "baz", "calls")]).unwrap();

        let n = resolve_edges(&conn, pid).unwrap();
        assert_eq!(n, 1);
        let dst: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        let expected: i64 = conn.query_row("SELECT id FROM code_symbols WHERE qualified = 'Foo::baz'", [], |r| r.get(0)).unwrap();
        assert_eq!(dst, Some(expected));
    }

    #[test]
    fn resolve_edges_stays_null_when_ambiguous() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(
            &conn,
            pid,
            "a.cpp",
            &[a_symbol("baz", "Foo::baz", "method", 1, 3), a_symbol("baz", "Bar::baz", "method", 5, 7)],
            &[],
        )
        .unwrap();
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "baz", "calls")]).unwrap();

        let n = resolve_edges(&conn, pid).unwrap();
        assert_eq!(n, 0);
        let dst: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = 'b.cpp'", params![pid], |r| r.get(0))
            .unwrap();
        assert_eq!(dst, None);
    }

    #[test]
    fn resolve_edges_resolves_includes_to_the_unique_tracked_path() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_file_upsert(&conn, pid, "src/local/thing.h", "blob1", Some("Cpp"), 3, "indexed", "2026-09-24T00:00:00Z").unwrap();
        replace_file_symbols(&conn, pid, "src/a.cpp", &[], &[an_edge(1, "local/thing.h", "includes")]).unwrap();

        let n = resolve_edges(&conn, pid).unwrap();
        assert_eq!(n, 1);
        let (dst_name, dst_path, dst_symbol_id): (String, Option<String>, Option<i64>) = conn
            .query_row("SELECT dst_name, dst_path, dst_symbol_id FROM code_edges WHERE project_id = ?1", params![pid], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(dst_name, "local/thing.h", "dst_name stays exactly as written");
        assert_eq!(dst_path.as_deref(), Some("src/local/thing.h"), "the resolved path lands in dst_path");
        assert_eq!(dst_symbol_id, None, "includes/imports resolve to a file, never a symbol id");

        // Idempotent: nothing left to touch on a second pass.
        assert_eq!(resolve_edges(&conn, pid).unwrap(), 0);
    }

    #[test]
    fn resolve_edges_leaves_include_unresolved_when_target_matches_multiple_tracked_paths() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_file_upsert(&conn, pid, "src/a/thing.h", "b1", Some("Cpp"), 3, "indexed", "2026-09-24T00:00:00Z").unwrap();
        code_file_upsert(&conn, pid, "src/b/thing.h", "b2", Some("Cpp"), 3, "indexed", "2026-09-24T00:00:00Z").unwrap();
        replace_file_symbols(&conn, pid, "src/x.cpp", &[], &[an_edge(1, "thing.h", "includes")]).unwrap();

        let n = resolve_edges(&conn, pid).unwrap();
        assert_eq!(n, 0);
        let (dst_name, dst_path): (String, Option<String>) =
            conn.query_row("SELECT dst_name, dst_path FROM code_edges WHERE project_id = ?1", params![pid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(dst_name, "thing.h", "ambiguous target must be left exactly as written");
        assert_eq!(dst_path, None);
    }

    #[test]
    fn symbol_definitions_exact_name_match() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("bar", "Foo::bar", "method", 1, 3)], &[]).unwrap();

        let exact = symbol_definitions(&conn, pid, "bar").unwrap();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].qualified, "Foo::bar");
    }

    #[test]
    fn symbol_definitions_falls_back_to_qualified_suffix_when_no_exact_name_matches() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("bar", "NS::Foo::bar", "method", 1, 3)], &[]).unwrap();

        // Nothing is literally named "Foo::bar" (the `name` column holds
        // just "bar"), so the exact-name query is empty and the
        // qualified-suffix fallback must find it via "...::Foo::bar".
        let hits = symbol_definitions(&conn, pid, "Foo::bar").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].qualified, "NS::Foo::bar");
    }

    #[test]
    fn symbol_definitions_unknown_name_returns_empty() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        assert!(symbol_definitions(&conn, pid, "nope").unwrap().is_empty());
    }

    #[test]
    fn callers_of_by_name_finds_edges_before_resolution_and_by_id_after() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let ids = replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("target", "target", "function", 1, 3)], &[]).unwrap();
        let target_id = ids[0];
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "target", "calls")]).unwrap();

        let by_name = callers_of(&conn, pid, SymbolLookup::Name("target"), 10).unwrap();
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].src_path, "b.cpp");
        assert_eq!(by_name[0].src_line, 2);

        resolve_edges(&conn, pid).unwrap();
        let by_id = callers_of(&conn, pid, SymbolLookup::Id(target_id), 10).unwrap();
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].src_path, "b.cpp");
    }

    #[test]
    fn callees_of_finds_edges_whose_enclosing_symbol_matches() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let ids = replace_file_symbols(
            &conn,
            pid,
            "a.cpp",
            &[a_symbol("caller", "caller", "function", 1, 5)],
            &[an_edge(2, "one", "calls"), an_edge(3, "two", "calls")],
        )
        .unwrap();
        let caller_id = ids[0];

        let callees = callees_of(&conn, pid, caller_id, 10).unwrap();
        assert_eq!(callees.len(), 2);
        assert_eq!(callees[0].dst_name, "one");
        assert_eq!(callees[1].dst_name, "two");
    }

    #[test]
    fn callers_and_callees_respect_limit() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let edges: Vec<NewEdge> = (0..5).map(|i| an_edge(i + 1, "target", "calls")).collect();
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("caller", "caller", "function", 1, 10)], &edges).unwrap();
        assert_eq!(callers_of(&conn, pid, SymbolLookup::Name("target"), 3).unwrap().len(), 3);
    }

    #[test]
    fn code_history_upsert_get_list_and_memory_id_roundtrip() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        assert!(code_history_get(&conn, pid, "2026-09").unwrap().is_none());

        code_history_upsert(&conn, pid, "2026-09", "did stuff", 5, "sha1", "digest1", None, "2026-09-24T00:00:00Z").unwrap();
        let row = code_history_get(&conn, pid, "2026-09").unwrap().unwrap();
        assert_eq!(row.text, "did stuff");
        assert_eq!(row.commits, 5);
        assert_eq!(row.memory_id, None);

        code_history_set_memory_id(&conn, pid, "2026-09", 42).unwrap();
        let row = code_history_get(&conn, pid, "2026-09").unwrap().unwrap();
        assert_eq!(row.memory_id, Some(42));

        // Regenerating the same month keeps the memory_id (upsert never
        // touches it) -- the constraints doc's "never superseded by
        // anything except the history job regenerating the same month"
        // rule depends on the mirrored memory being updated in place, not
        // re-minted.
        code_history_upsert(&conn, pid, "2026-09", "did more stuff", 6, "sha2", "digest2", Some("a.rs"), "2026-09-25T00:00:00Z").unwrap();
        let row = code_history_get(&conn, pid, "2026-09").unwrap().unwrap();
        assert_eq!(row.text, "did more stuff");
        assert_eq!(row.commits, 6);
        assert_eq!(row.memory_id, Some(42), "memory_id must survive a regeneration of the same month");

        code_history_upsert(&conn, pid, "2026-08", "earlier month", 2, "sha0", "digest0", None, "2026-08-24T00:00:00Z").unwrap();
        let list = code_history_list(&conn, pid).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].month, "2026-08");
        assert_eq!(list[1].month, "2026-09");
    }

    #[test]
    fn extract_refs_then_replace_file_symbols_resolves_the_cpp_out_of_line_example() {
        // End-to-end: `chunk::extract_refs` -> `replace_file_symbols` ->
        // `resolve_edges`, the exact scenario the brief calls out.
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let src = "class Foo {\npublic:\n    void bar();\n};\n\nvoid Foo::bar() {\n    baz();\n}\n";
        let refs = crate::code_index::chunk::extract_refs("t.cpp", src);
        let ids = replace_file_symbols(&conn, pid, "t.cpp", &refs.symbols, &refs.edges).unwrap();
        replace_file_symbols(&conn, pid, "other.cpp", &[a_symbol("baz", "baz", "function", 1, 1)], &[]).unwrap();
        resolve_edges(&conn, pid).unwrap();

        let bar_id = *ids.iter().zip(refs.symbols.iter()).find(|(_, s)| s.name == "bar").unwrap().0;
        let bar_qualified: String =
            conn.query_row("SELECT qualified FROM code_symbols WHERE id = ?1", params![bar_id], |r| r.get(0)).unwrap();
        assert_eq!(bar_qualified, "Foo::bar");

        let (src_symbol_id, dst_symbol_id): (Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT src_symbol_id, dst_symbol_id FROM code_edges WHERE project_id = ?1 AND dst_name = 'baz'",
                params![pid],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(src_symbol_id, Some(bar_id), "the call inside Foo::bar must resolve back to it");
        let baz_id: i64 = conn.query_row("SELECT id FROM code_symbols WHERE qualified = 'baz'", [], |r| r.get(0)).unwrap();
        assert_eq!(dst_symbol_id, Some(baz_id));
    }

    // -- final-review fix wave: graph resolution -------------------------

    fn dst_of(conn: &Connection, pid: i64, src_path: &str) -> Option<i64> {
        conn.query_row("SELECT dst_symbol_id FROM code_edges WHERE project_id = ?1 AND src_path = ?2", params![pid, src_path], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn a_qualified_call_never_falls_back_to_an_unrelated_leaf_name() {
        // Item 8: `std::find` must not resolve to the project's own `find`.
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("find", "Registry::find", "method", 1, 3)], &[]).unwrap();
        replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "std::find", "calls")]).unwrap();
        assert_eq!(resolve_edges(&conn, pid).unwrap(), 0);
        assert_eq!(dst_of(&conn, pid, "b.cpp"), None, "std::find must stay unresolved");
    }

    #[test]
    fn a_qualified_call_resolves_when_the_candidate_ends_with_it_on_a_boundary() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let ids = replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("add_event", "helios::picking::add_event", "function", 1, 3)], &[]).unwrap();
        // "king::add_event" is a suffix but not on a `::` boundary.
        replace_file_symbols(
            &conn,
            pid,
            "b.cpp",
            &[a_symbol("caller", "caller", "function", 1, 5)],
            &[an_edge(2, "picking::add_event", "calls"), an_edge(3, "king::add_event", "calls")],
        )
        .unwrap();
        assert_eq!(resolve_edges(&conn, pid).unwrap(), 1);
        let good: Option<i64> = conn
            .query_row("SELECT dst_symbol_id FROM code_edges WHERE dst_name = 'picking::add_event'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(good, Some(ids[0]));
        let bad: Option<i64> =
            conn.query_row("SELECT dst_symbol_id FROM code_edges WHERE dst_name = 'king::add_event'", [], |r| r.get(0)).unwrap();
        assert_eq!(bad, None);
    }

    #[test]
    fn self_and_crate_relative_calls_still_resolve_by_leaf() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.rs", &[a_symbol("helper", "Foo::helper", "function", 1, 3)], &[]).unwrap();
        replace_file_symbols(
            &conn,
            pid,
            "b.rs",
            &[a_symbol("caller", "caller", "function", 1, 5)],
            &[an_edge(2, "Self::helper", "calls"), an_edge(3, "crate::Foo::helper", "calls")],
        )
        .unwrap();
        assert_eq!(resolve_edges(&conn, pid).unwrap(), 2);
    }

    #[test]
    fn resolve_edges_at_scale_is_correct_and_an_unchanged_incremental_pass_examines_nothing() {
        // Item 2: a synthetic project large enough that the old per-edge
        // query loop was visibly quadratic; asserts behaviour, not timing.
        let conn = mem_conn();
        let pid = new_project(&conn, "big");
        let files = 200usize;
        for f in 0..files {
            let symbols: Vec<NewSymbol> =
                (0..20).map(|i| a_symbol(&format!("fn_{f}_{i}"), &format!("ns{f}::fn_{f}_{i}"), "function", (i * 10 + 1) as i64, (i * 10 + 9) as i64)).collect();
            // Each file calls 20 functions of the next file (unique names),
            // one shared ambiguous name, and one external std:: call.
            let g = (f + 1) % files;
            let mut edges: Vec<NewEdge> = (0..20).map(|i| an_edge((i * 10 + 2) as i64, &format!("fn_{g}_{i}"), "calls")).collect();
            edges.push(an_edge(3, "dup", "calls"));
            edges.push(an_edge(4, "std::find", "calls"));
            replace_file_symbols(&conn, pid, &format!("f{f}.cpp"), &symbols, &edges).unwrap();
        }
        replace_file_symbols(&conn, pid, "dup1.cpp", &[a_symbol("dup", "a::dup", "function", 1, 2)], &[]).unwrap();
        replace_file_symbols(&conn, pid, "dup2.cpp", &[a_symbol("dup", "b::dup", "function", 1, 2)], &[]).unwrap();

        let stats = resolve_edges_all(&conn, pid).unwrap();
        assert_eq!(stats.examined, files * 22);
        assert_eq!(stats.resolved, files * 20, "every unique project call resolves; dup and std::find do not");

        // Nothing changed since: the incremental pass examines 0 edges.
        assert_eq!(resolve_edges_incremental(&conn, pid).unwrap(), ResolveStats::default());

        // One file re-replaced: only its own edges plus edges naming its
        // symbols (the 20 incoming from f(n-1)) are examined.
        let symbols: Vec<NewSymbol> =
            (0..20).map(|i| a_symbol(&format!("fn_5_{i}"), &format!("ns5::fn_5_{i}"), "function", (i * 10 + 1) as i64, (i * 10 + 9) as i64)).collect();
        let edges: Vec<NewEdge> = (0..20).map(|i| an_edge((i * 10 + 2) as i64, &format!("fn_6_{i}"), "calls")).collect();
        replace_file_symbols(&conn, pid, "f5.cpp", &symbols, &edges).unwrap();
        let inc = resolve_edges_incremental(&conn, pid).unwrap();
        assert_eq!(inc.examined, 40, "20 own edges + 20 nulled incoming edges from f4.cpp");
        assert_eq!(inc.resolved, 40);
        let unresolved_f4: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_edges WHERE project_id = ?1 AND src_path = 'f4.cpp' AND dst_symbol_id IS NULL", params![pid], |r| r.get(0))
            .unwrap();
        assert_eq!(unresolved_f4, 2, "only dup + std::find stay unresolved");
        assert_eq!(resolve_edges_incremental(&conn, pid).unwrap(), ResolveStats::default());
    }

    #[test]
    fn deleting_one_of_two_same_named_symbols_lets_the_incremental_pass_resolve_the_survivor() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "a.cpp", &[a_symbol("dup", "a::dup", "function", 1, 2)], &[]).unwrap();
        let ids = replace_file_symbols(&conn, pid, "b.cpp", &[a_symbol("dup", "b::dup", "function", 1, 2)], &[]).unwrap();
        replace_file_symbols(&conn, pid, "c.cpp", &[a_symbol("caller", "caller", "function", 1, 3)], &[an_edge(2, "dup", "calls")]).unwrap();
        assert_eq!(resolve_edges(&conn, pid).unwrap(), 0, "ambiguous");
        delete_file_symbols(&conn, pid, "a.cpp").unwrap();
        let inc = resolve_edges_incremental(&conn, pid).unwrap();
        assert_eq!(inc.resolved, 1);
        assert_eq!(dst_of(&conn, pid, "c.cpp"), Some(ids[0]));
    }

    #[test]
    fn includes_resolve_incrementally_when_the_target_file_appears_and_unresolve_when_it_moves() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        replace_file_symbols(&conn, pid, "src/a.cpp", &[], &[an_edge(1, "thing.h", "includes")]).unwrap();
        resolve_edges(&conn, pid).unwrap();
        code_file_upsert(&conn, pid, "inc/thing.h", "b1", Some("Cpp"), 1, "indexed", "2026-09-24T00:00:00Z").unwrap();
        replace_file_symbols_for_blob(&conn, pid, "inc/thing.h", "b1", &[], &[]).unwrap();
        assert_eq!(resolve_edges_incremental(&conn, pid).unwrap().resolved, 1);
        let dst_path: Option<String> = conn.query_row("SELECT dst_path FROM code_edges WHERE src_path = 'src/a.cpp'", [], |r| r.get(0)).unwrap();
        assert_eq!(dst_path.as_deref(), Some("inc/thing.h"));
        // refs matches by either form.
        let refs_blob: Option<String> =
            conn.query_row("SELECT refs_blob FROM code_files WHERE path = 'inc/thing.h'", [], |r| r.get(0)).unwrap();
        assert_eq!(refs_blob.as_deref(), Some("b1"));

        move_file_symbols(&conn, pid, "inc/thing.h", "inc2/thing.h").unwrap();
        conn.execute("UPDATE code_files SET path = 'inc2/thing.h' WHERE path = 'inc/thing.h'", []).unwrap();
        let after_move: Option<String> = conn.query_row("SELECT dst_path FROM code_edges WHERE src_path = 'src/a.cpp'", [], |r| r.get(0)).unwrap();
        assert_eq!(after_move, None, "a moved target must not keep its stale resolved path");
        assert_eq!(resolve_edges_incremental(&conn, pid).unwrap().resolved, 1);
        let re: Option<String> = conn.query_row("SELECT dst_path FROM code_edges WHERE src_path = 'src/a.cpp'", [], |r| r.get(0)).unwrap();
        assert_eq!(re.as_deref(), Some("inc2/thing.h"));
    }

    fn query_plan(conn: &Connection, sql: &str, params: &[&dyn rusqlite::ToSql]) -> String {
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let rows = stmt.query_map(params, |r| r.get::<_, String>(3)).unwrap();
        rows.map(|r| r.unwrap()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn null_out_incoming_edges_uses_the_dst_symbol_index() {
        // Item 3: the replace/delete null-out must be an index search on
        // (project_id, dst_symbol_id), never a scan of code_edges.
        let conn = mem_conn();
        let plan = query_plan(&conn, NULL_INCOMING_SQL, &[&1i64, &"a.cpp"]);
        assert!(plan.contains("ix_code_edges_dst_symbol"), "{plan}");
        assert!(!plan.contains("SCAN code_edges"), "{plan}");
    }

    #[test]
    fn resolve_qualified_lookup_uses_the_qualified_index() {
        let conn = mem_conn();
        let plan = query_plan(&conn, "SELECT id FROM code_symbols WHERE project_id = ?1 AND qualified = ?2", &[&1i64, &"Foo::bar"]);
        assert!(plan.contains("ix_code_symbols_qualified"), "{plan}");
        let plan = query_plan(&conn, "SELECT rowid FROM code_edges WHERE project_id = ?1 AND src_symbol_id = ?2", &[&1i64, &5i64]);
        assert!(plan.contains("ix_code_edges_src_symbol"), "{plan}");
    }

    #[test]
    fn migrate_v34_to_v35_upgrades_a_first_cut_v35_database_in_place() {
        // A scratch copy created by the first v35 cut lacks the fix-wave
        // columns; reopening must add them (migrate re-runs v35 at <= 35).
        let conn = open_with_path(Path::new(":memory:")).unwrap();
        conn.execute_batch(
            "DROP TABLE code_graph_pending; DROP INDEX ix_code_edges_dst_path; ALTER TABLE code_edges DROP COLUMN dst_path;
             ALTER TABLE code_files DROP COLUMN refs_blob; ALTER TABLE code_history DROP COLUMN files;",
        )
        .unwrap();
        migrate(&conn).unwrap();
        for (table, col) in [("code_files", "refs_blob"), ("code_edges", "dst_path"), ("code_history", "files")] {
            let n: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"), params![col], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "{table}.{col}");
        }
        let t: i64 =
            conn.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name = 'code_graph_pending'", [], |r| r.get(0)).unwrap();
        assert_eq!(t, 1);
    }
}
