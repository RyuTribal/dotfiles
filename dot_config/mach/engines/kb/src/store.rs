
//! SQLite-backed store for the kb engine: schema, CRUD, ranked recall, and
//! save-time supersession bookkeeping. See the crate doc in `lib.rs` for why
//! cosine-over-BLOB was chosen over a `sqlite-vec` virtual table.
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use std::collections::HashMap;
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
    // How this memory came to be known -- Honcho's explicit/deductive split.
    // `Some("stated")`: the user (or a named person) said it in so many
    // words: `mach kb add`, `mach note`, a decision cue, a digest line the
    // model tagged STATED. `Some("inferred")`: deduced from behavior, code,
    // or context (a digest line tagged INFERRED). `None`: written before the
    // column existed, or by a channel that does not classify (meeting
    // facts, consolidation) -- renderers fall back to source-only phrasing.
    // Never affects ranking; it exists so recall can say "you told me" vs
    // "I inferred" honestly.
    pub basis: Option<String>,
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
            basis TEXT
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
            level INTEGER NOT NULL DEFAULT 1
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
/// only the inverted index. Tokenizer `unicode61` folds case and splits on
/// punctuation: "10.8.0.63" indexes as four numeric tokens, "react-app" as
/// two words, a NORAD id "43005" as itself -- exactly the exact-token
/// matches cosine over an embedding blurs. Idempotent (`IF NOT EXISTS`
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
            tokenize='unicode61 remove_diacritics 2'
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
fn migrate(conn: &Connection) -> Result<(), KbError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
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
fn parse_rfc3339(s: &str) -> Option<i64> {
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
fn days_between(now: i64, then: i64) -> f64 {
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

pub fn is_valid_basis(b: &str) -> bool {
    b == BASIS_STATED || b == BASIS_INFERRED
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
            return Err(KbError::Other(format!("invalid basis {:?} (expected stated|inferred)", b)));
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
    Ok(conn.last_insert_rowid())
}

/// Sets `basis` on an existing row (used by `mach kb add`, whose insert
/// runs inside the classifier's `apply_verdict` and so cannot pass it at
/// insert time). Same validation as `insert_with_basis`.
pub fn set_basis(conn: &Connection, id: i64, basis: &str) -> Result<(), KbError> {
    if !is_valid_basis(basis) {
        return Err(KbError::Other(format!("invalid basis {:?} (expected stated|inferred)", basis)));
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

/// Reinforcement (`mach kb search --touch`): for each id, bumps
/// `access_count`, resets `last_accessed_at` to `now`, sets
/// `first_accessed_at` if this is the first touch, and grows `stability`
/// by 30%, capped at 365 days. One transaction for the whole batch.
pub fn touch(conn: &Connection, ids: &[i64], now: &str) -> Result<(), KbError> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE memories SET
                access_count = access_count + 1,
                last_accessed_at = ?1,
                first_accessed_at = COALESCE(first_accessed_at, ?1),
                stability = MIN(COALESCE(stability, importance * 7.0) * 1.3, 365.0)
             WHERE id = ?2",
            params![now, id],
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
    // Normalized BM25 from the FTS5 index for the query's exact tokens:
    // 1.0 for the best lexical match, 0.0 when the row matched no token
    // (or the search ran without query text). See `search_hybrid`.
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
    rank_with_lexical(conn, query_embedding, &HashMap::new(), limit, reviewed_only, include_superseded, min_score, now)
}

/// Weight of a perfect lexical match relative to a perfect cosine match.
/// Below 1.0 on purpose: an exact-token hit is strong evidence but the
/// embedding still knows about paraphrase; a row that matches both gets
/// whichever is higher, never the sum (so nothing is double counted).
pub const LEXICAL_WEIGHT: f32 = 0.9;

/// How many FTS rows to pull per query. Only the top few ever matter for
/// ranking; this bounds the join work on a large bank.
pub const LEXICAL_CANDIDATES: usize = 50;

/// `search_ranked` plus the FTS5 lexical channel (Honcho-style hybrid):
/// the query's exact tokens are looked up in `memories_fts`, BM25 is
/// normalized to `[0, 1]` against the best lexical hit, and each row's
/// similarity term becomes `max(cosine, LEXICAL_WEIGHT * lexical)`. The
/// recency/strength/penalty blend is unchanged, so scores stay on the same
/// scale the hooks threshold against (`kb-recall.py`'s 0.45). A row with no
/// embedding can now surface on a lexical hit alone; a row with neither is
/// still excluded. Any FTS failure (bad syntax that slipped past
/// `fts_query`, index missing) degrades to plain cosine, never an error.
pub fn search_hybrid(
    conn: &Connection,
    query_text: &str,
    query_embedding: &[f32],
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<RankedHit>, KbError> {
    let lexical = lexical_scores(conn, query_text, LEXICAL_CANDIDATES).unwrap_or_default();
    rank_with_lexical(conn, query_embedding, &lexical, limit, reviewed_only, include_superseded, min_score, now)
}

#[allow(clippy::too_many_arguments)]
fn rank_with_lexical(
    conn: &Connection,
    query_embedding: &[f32],
    lexical: &HashMap<i64, f32>,
    limit: usize,
    reviewed_only: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<RankedHit>, KbError> {
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    let mut scored: Vec<RankedHit> = candidates(conn, !reviewed_only, include_superseded)?
        .into_iter()
        .filter_map(|m| {
            let lex = lexical.get(&m.id).copied().unwrap_or(0.0);
            let sim = match &m.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ if lex > 0.0 => 0.0,
                _ => return None,
            };
            let sim_eff = sim.max(LEXICAL_WEIGHT * lex);
            let recency = compute_recency(&m, now_secs);
            let strength = compute_strength(&m, now_secs);
            let superseded = m.is_superseded();
            let mut score = 0.70 * sim_eff + 0.20 * recency + 0.10 * strength;
            if superseded {
                score *= 0.1;
            }
            if !m.reviewed {
                score *= UNREVIEWED_SEARCH_PENALTY;
            }
            Some(RankedHit { memory: m, score, sim, recency, strength, superseded, lexical: lex })
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
];

/// Turns free text into a safe FTS5 MATCH expression: lowercase alphanumeric
/// tokens (unicode61's own idea of a word), stopwords and 1-2 letter words
/// dropped (digits of any length kept -- "Q4", "43005"), deduped, capped at
/// 12, each double-quoted so no FTS operator syntax (`NEAR`, `*`, `:`, a
/// stray quote) can leak through, joined with OR. `None` when nothing
/// survives, in which case the lexical channel is skipped entirely.
pub fn fts_query(text: &str) -> Option<String> {
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
    if seen.is_empty() {
        return None;
    }
    Some(seen.into_iter().map(|t| format!("\"{}\"", t)).collect::<Vec<_>>().join(" OR "))
}

/// `memory id -> normalized lexical score` for the query's tokens, from the
/// FTS5 index: BM25 (negative, more negative is better) divided by the best
/// row's BM25, so the top lexical hit is exactly 1.0 and the rest fall off
/// toward 0. Empty when the query has no usable tokens or nothing matches.
/// Errors (index missing, syntax) propagate; `search_hybrid` treats them as
/// "no lexical channel".
pub fn lexical_scores(conn: &Connection, query_text: &str, limit: usize) -> Result<HashMap<i64, f32>, KbError> {
    let Some(expr) = fts_query(query_text) else {
        return Ok(HashMap::new());
    };
    let mut stmt = conn.prepare(
        "SELECT rowid, bm25(memories_fts) FROM memories_fts WHERE memories_fts MATCH ?1 ORDER BY bm25(memories_fts) LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![expr, limit as i64], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?;
    let mut ranked: Vec<(i64, f64)> = Vec::new();
    for r in rows {
        ranked.push(r?);
    }
    let mut out = HashMap::new();
    let Some(&(_, best)) = ranked.first() else {
        return Ok(out);
    };
    if best >= 0.0 {
        // bm25() is negative for every real match; a non-negative best means
        // nothing usable came back.
        return Ok(out);
    }
    for (id, bm) in ranked {
        let lex = if bm < 0.0 { (bm / best).clamp(0.0, 1.0) as f32 } else { 0.0 };
        if lex > 0.0 {
            out.insert(id, lex);
        }
    }
    Ok(out)
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
    let mut scored: Vec<(Memory, f32)> = candidates(conn, false, false)?
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

/// What `apply_verdict` actually did to the store, for the CLI to report.
pub enum AddOutcome {
    Added { id: i64 },
    AddedAndTombstoned { new_id: i64, old_id: i64, verb: &'static str },
    Skipped { reason: String },
}

/// Applies a classifier verdict (or the default `Add` when classification
/// was skipped) to the store. `UPDATE`/`SUPERSEDE` both insert the new fact
/// first and only then tombstone the old row — inserting unconditionally
/// means a stale or invalid id in the verdict never costs the new fact:
/// worst case it's just a plain add.
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
        Verdict::Update(old_id) | Verdict::Supersede(old_id) => {
            let new_id = insert(conn, content, source, project, reviewed, Some(embedding), importance)?;
            let tombstoned = supersede(conn, old_id, new_id, now)?;
            if tombstoned {
                let verb = if matches!(verdict, Verdict::Update(_)) { "updated" } else { "superseded" };
                Ok(AddOutcome::AddedAndTombstoned { new_id, old_id, verb })
            } else {
                // Referenced id was already gone/tombstoned — never lose
                // the new fact over it, just fall back to a plain add.
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

/// Marks an insight flagged by re-verification (evidence no longer
/// supports it). Never cleared automatically — `mach kb insight-forget` is
/// the only way a flagged insight goes away.
pub fn flag_insight(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE insights SET flagged_at = ?1 WHERE id = ?2", params![now, id])?;
    Ok(n > 0)
}

/// Bumps `last_verified_at` after a re-verification pass finds an insight's
/// evidence still holds.
pub fn mark_insight_verified(conn: &Connection, id: i64, now: &str) -> Result<bool, KbError> {
    let n = conn.execute("UPDATE insights SET last_verified_at = ?1 WHERE id = ?2", params![now, id])?;
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
/// search`: `score = 0.70*sim + 0.20*recency` (the 0.10 strength term of
/// the memory blend is simply omitted, i.e. fixed at 0).
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
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ => return None,
            };
            let recency = compute_insight_recency(&insight, now_secs);
            let score = 0.70 * sim + 0.20 * recency;
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
}

/// Builds the tree-ordered mental model over all active insights and
/// themes: themes first (by id), each immediately followed by its own
/// level-1 insights (in the theme's `source_ids` order), then any level-1
/// insights not yet folded into a theme (by id). Same shape `mach kb tree`
/// renders, minus the memory citations — this view exists to be cheap
/// enough to inject on every session start, not to support investigation.
pub fn mental_model(conn: &Connection) -> Result<Vec<ModelRow>, KbError> {
    let mut themes = active_insights_by_level(conn, 2)?;
    themes.sort_by_key(|t| t.id);
    let mut level1 = active_insights_by_level(conn, 1)?;
    level1.sort_by_key(|i| i.id);
    let themed = themed_insight_ids(conn)?;

    let mut rows = Vec::new();
    for theme in &themes {
        rows.push(ModelRow {
            kind: ModelKind::Theme,
            confidence: theme.confidence,
            text: theme.text.clone(),
            doubted: theme.is_flagged(),
            nested: false,
        });
        for sid in &theme.source_ids {
            if let Some(rest) = sid.strip_prefix('i').or_else(|| sid.strip_prefix('I')) {
                if let Ok(iid) = rest.parse::<i64>() {
                    if let Some(ins) = level1.iter().find(|i| i.id == iid) {
                        rows.push(ModelRow {
                            kind: ModelKind::Belief,
                            confidence: ins.confidence,
                            text: ins.text.clone(),
                            doubted: ins.is_flagged(),
                            nested: true,
                        });
                    }
                }
            }
        }
    }

    for ins in level1.iter().filter(|i| !themed.contains(&i.id)) {
        rows.push(ModelRow {
            kind: ModelKind::Belief,
            confidence: ins.confidence,
            text: ins.text.clone(),
            doubted: ins.is_flagged(),
            nested: false,
        });
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
pub fn repoint_insight_citations(conn: &Connection, old_id: i64, new_id: i64) -> Result<usize, KbError> {
    let old_s = old_id.to_string();
    let new_s = new_id.to_string();
    let mut stmt = conn.prepare("SELECT id, source_ids FROM insights")?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
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
    let needle = memory_id.to_string();
    let mut stmt = conn.prepare("SELECT id, source_ids FROM insights WHERE invalidated_at IS NULL AND flagged_at IS NULL")?;
    let rows: Vec<(i64, String)> = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut flagged = 0usize;
    for (id, json) in rows {
        let ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
        if ids.contains(&needle) && flag_insight(conn, id, now)? {
            flagged += 1;
        }
    }
    Ok(flagged)
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
         ORDER BY last_verified_at ASC, id ASC LIMIT {}",
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
             superseded_by, dormant_at, last_verified_at, graph_extracted_at, basis)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)
         ON CONFLICT(id) DO UPDATE SET
            content = excluded.content, source = excluded.source, project = excluded.project,
            created_at = excluded.created_at, reviewed = excluded.reviewed, embedding = excluded.embedding,
            importance = excluded.importance, stability = excluded.stability,
            access_count = excluded.access_count, first_accessed_at = excluded.first_accessed_at,
            last_accessed_at = excluded.last_accessed_at, valid_from = excluded.valid_from,
            invalidated_at = excluded.invalidated_at, superseded_by = excluded.superseded_by,
            dormant_at = excluded.dormant_at, last_verified_at = excluded.last_verified_at,
            graph_extracted_at = excluded.graph_extracted_at, basis = excluded.basis",
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
             flagged_at, last_verified_at, level)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(id) DO UPDATE SET
            text = excluded.text, created_at = excluded.created_at, confidence = excluded.confidence,
            source_ids = excluded.source_ids, embedding = excluded.embedding,
            invalidated_at = excluded.invalidated_at, flagged_at = excluded.flagged_at,
            last_verified_at = excluded.last_verified_at, level = excluded.level",
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
/// `last_verified_at`, `flagged_at`, `invalidated_at`.
pub fn insight_last_modified(i: &Insight) -> &str {
    let mut latest = i.created_at.as_str();
    for candidate in [i.last_verified_at.as_deref(), i.flagged_at.as_deref(), i.invalidated_at.as_deref()] {
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
         ORDER BY id ASC LIMIT {}",
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
    let n: i64 = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memories WHERE graph_extracted_at IS NULL AND invalidated_at IS NULL AND dormant_at IS NULL)",
        [],
        |r| r.get(0),
    )?;
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

        let hybrid = search_hybrid(&conn, "what is 43005", &q, 5, false, false, 0.0, &now).unwrap();
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
        let hybrid = search_hybrid(&conn, "43005", &q, 5, false, false, 0.0, &now).unwrap();
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
        let hybrid = search_hybrid(&conn, "popobawa", &q, 5, false, false, 0.0, &now).unwrap();
        assert_eq!(hybrid.len(), 1);
        assert_eq!(hybrid[0].memory.id, id);
        // and with no usable tokens at all it is still excluded
        assert!(search_hybrid(&conn, "the a", &q, 5, false, false, 0.0, &now).unwrap().is_empty());
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
        assert_eq!(v, 15);
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
        assert_eq!(v, 15);
        let m = get(&conn, 1).unwrap().unwrap();
        assert_eq!(m.basis, None, "pre-existing rows stay basis-unknown");
        // idempotent
        migrate(&conn).unwrap();
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
        insert5(&conn, "the boss wants Q4 retention metrics", Some(&fake_embed("the boss wants Q4 retention metrics")));
        insert5(&conn, "unrelated quantum chess trivia", Some(&fake_embed("unrelated quantum chess trivia")));
        insert(&conn, "no embedding stored for this one", None, None, true, None, 5).unwrap();

        let now = now_rfc3339();
        let query = fake_embed("what does my boss want for Q4");
        let results = search_ranked(&conn, &query, 5, false, false, 0.0, &now).unwrap();

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
    fn touch_resets_recency_and_grows_stability_capped_at_365() {
        let conn = mem_conn();
        let id = insert5(&conn, "reinforced fact", None);
        // importance 5 -> initial stability 35.0
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(35.0));

        let now = now_rfc3339();
        touch(&conn, &[id], &now).unwrap();
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.access_count, 1);
        assert_eq!(m.last_accessed_at.as_deref(), Some(now.as_str()));
        assert_eq!(m.first_accessed_at.as_deref(), Some(now.as_str()));
        assert!((m.stability.unwrap() - 35.0 * 1.3).abs() < 1e-9);

        // enough touches to hit the 365-day cap
        for _ in 0..40 {
            touch(&conn, &[id], &now).unwrap();
        }
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.stability, Some(365.0));
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
        assert_eq!(version, 15);

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
    fn apply_verdict_update_inserts_new_and_tombstones_old() {
        let conn = mem_conn();
        let old_id = insert5(&conn, "old version of the fact", None);
        let now = now_rfc3339();
        let outcome = apply_verdict(
            &conn, Verdict::Update(old_id), "new version of the fact", None, None, true,
            &fake_embed("new version of the fact"), 5, &now, Some(old_id),
        )
        .unwrap();
        match outcome {
            AddOutcome::AddedAndTombstoned { new_id, old_id: o, verb } => {
                assert_eq!(o, old_id);
                assert_eq!(verb, "updated");
                let old = get(&conn, old_id).unwrap().unwrap();
                assert_eq!(old.superseded_by, Some(new_id));
                assert!(old.invalidated_at.is_some());
            }
            _ => panic!("expected AddedAndTombstoned"),
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
            version, 15,
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
        assert_eq!(version, 15);
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
            version, 15,
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
            version, 15,
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
            version, 15,
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
        assert_eq!(version, 15, "v5->v6, v6->v7 (AUTOINCREMENT rebuild), then v7->v8 (ingested_sessions) all run");

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
        assert_eq!(version, 15, "v6->v7 (AUTOINCREMENT rebuild), then v7->v8 (ingested_sessions) both run");

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
        assert_eq!(version, 15, "v7->v8 (ingested_sessions) runs");

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
    fn mental_model_orders_themes_before_their_insights_then_unthemed_insights() {
        let conn = mem_conn();
        let a = insert_insight(&conn, "insight a", 0.5, &["1".into(), "2".into()], None).unwrap();
        let b = insert_insight(&conn, "insight b", 0.6, &["1".into(), "2".into()], None).unwrap();
        let unthemed = insert_insight(&conn, "unthemed insight", 0.7, &["1".into(), "2".into()], None).unwrap();
        insert_theme(&conn, "a theme", 0.55, &[format!("i{}", a), format!("i{}", b), "3".into()], None).unwrap();

        let rows = mental_model(&conn).unwrap();
        assert_eq!(rows.len(), 4, "1 theme + 2 nested insights + 1 unthemed insight");

        assert_eq!(rows[0].kind, ModelKind::Theme);
        assert_eq!(rows[0].text, "a theme");
        assert!(!rows[0].nested);

        assert_eq!(rows[1].kind, ModelKind::Belief);
        assert_eq!(rows[1].text, "insight a");
        assert!(rows[1].nested, "insight a is nested under its theme");

        assert_eq!(rows[2].kind, ModelKind::Belief);
        assert_eq!(rows[2].text, "insight b");
        assert!(rows[2].nested, "insight b is nested under its theme");

        assert_eq!(rows[3].kind, ModelKind::Belief);
        assert_eq!(rows[3].text, "unthemed insight");
        assert!(!rows[3].nested, "not folded into any theme");
        assert_eq!(rows[3].confidence, 0.7);
        let _ = unthemed; // id only asserted via ordering/content above
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
        let now = now_rfc3339();
        touch(&conn, &[winner], &now).unwrap(); // access_count 1, stability 35*1.3=45.5
        touch(&conn, &[loser], &now).unwrap();
        touch(&conn, &[loser], &now).unwrap(); // access_count 2, stability higher than winner's

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
        let updated = repoint_insight_citations(&conn, 12, 40).unwrap();
        assert_eq!(updated, 1);
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["3", "40", "i2"], "only the raw id 12 is rewritten, i2 left alone");
    }

    #[test]
    fn repoint_insight_citations_dedupes_when_winner_already_cited() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "an insight", 0.6, &["12".into(), "40".into()], None).unwrap();
        let updated = repoint_insight_citations(&conn, 12, 40).unwrap();
        assert_eq!(updated, 1);
        let ins = get_insight(&conn, id).unwrap().unwrap();
        assert_eq!(ins.source_ids, vec!["40"], "12 rewritten to 40 collapses with the already-cited 40");
    }

    #[test]
    fn repoint_insight_citations_leaves_non_citing_insights_untouched() {
        let conn = mem_conn();
        let id = insert_insight(&conn, "unrelated insight", 0.6, &["7".into(), "8".into()], None).unwrap();
        let updated = repoint_insight_citations(&conn, 12, 40).unwrap();
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
        assert_eq!(version, 15, "v8->v9 (graph layer) runs");

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
        assert_eq!(version, 15, "v9->v10 (last_completed_at + entity_merge_seen) runs");

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
}
