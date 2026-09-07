
//! SQLite-backed store for the kb engine: schema, CRUD, ranked recall, and
//! save-time supersession bookkeeping. See the crate doc in `lib.rs` for why
//! cosine-over-BLOB was chosen over a `sqlite-vec` virtual table.
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

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
}

/// `~/.local/share/mach/kb.db`, the default store location.
pub fn db_path() -> Result<PathBuf, KbError> {
    let home = env::var("HOME").map_err(|_| KbError::Other("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".local/share/mach/kb.db"))
}

fn init_schema(conn: &Connection) -> Result<(), KbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memories (
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
            superseded_by INTEGER,
            dormant_at TEXT
        );
        CREATE TABLE IF NOT EXISTS insights (
            id INTEGER PRIMARY KEY,
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
            last_memory_id INTEGER
        );",
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
    let cols = existing_columns(conn)?;
    if !cols.iter().any(|c| c == "dormant_at") {
        conn.execute("ALTER TABLE memories ADD COLUMN dormant_at TEXT", [])?;
    }
    conn.execute("PRAGMA user_version = 4", [])?;
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

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_rfc3339_from_secs(secs: u64) -> String {
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
    let created_at = now_rfc3339();
    let stability = importance as f64 * 7.0;
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO memories
            (content, source, project, created_at, reviewed, embedding, importance, stability, valid_from)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?4)",
        params![content, source, project, created_at, reviewed as i64, blob, importance, stability],
    )?;
    Ok(conn.last_insert_rowid())
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
pub fn unreviewed(conn: &Connection) -> Result<Vec<Memory>, KbError> {
    let mut stmt = conn.prepare("SELECT * FROM memories WHERE reviewed = 0 ORDER BY id ASC")?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
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
pub fn delete(conn: &Connection, id: i64) -> Result<bool, KbError> {
    let n = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

pub fn set_reviewed(conn: &Connection, id: i64, reviewed: bool) -> Result<bool, KbError> {
    let n = conn.execute(
        "UPDATE memories SET reviewed = ?1 WHERE id = ?2",
        params![reviewed as i64, id],
    )?;
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
pub const DORMANCY_UNREVIEWED_MAX_AGE_DAYS: f64 = 30.0;

/// Whether an active memory qualifies to go dormant on tonight's `mach kb
/// reflect` pass. Two independent rules, either one sufficient:
///
/// 1. It is an unreviewed row (`reviewed = false`) older than
///    `DORMANCY_UNREVIEWED_MAX_AGE_DAYS` — the never-curated digest queue
///    goes dormant regardless of anything else (touch count, importance,
///    citations); and
/// 2. ALL of: created more than `DORMANCY_MIN_AGE_DAYS` ago; never touched
///    (`access_count == 0`) or not touched in over
///    `DORMANCY_MIN_UNTOUCHED_STALE_DAYS` days; `importance <=
///    DORMANCY_MAX_IMPORTANCE`; and not cited by any active insight or
///    theme (`cited`, computed by the caller against `cited_memory_ids`).
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

    if !m.reviewed && age_days > DORMANCY_UNREVIEWED_MAX_AGE_DAYS {
        return true;
    }

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
}

/// Ranked top-N search: `final = 0.70*sim + 0.20*recency + 0.10*strength`.
/// Tombstoned rows are excluded unless `include_superseded`, in which case
/// they're included but scored at 10% of the blend (still marked
/// `superseded` in the result so callers can label them).
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
    include_all: bool,
    include_superseded: bool,
    min_score: f32,
    now: &str,
) -> Result<Vec<RankedHit>, KbError> {
    let now_secs = parse_rfc3339(now).unwrap_or(0);
    let mut scored: Vec<RankedHit> = candidates(conn, include_all, include_superseded)?
        .into_iter()
        .filter_map(|m| {
            let sim = match &m.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e).clamp(0.0, 1.0),
                _ => return None,
            };
            let recency = compute_recency(&m, now_secs);
            let strength = compute_strength(&m, now_secs);
            let superseded = m.is_superseded();
            let mut score = 0.70 * sim + 0.20 * recency + 0.10 * strength;
            if superseded {
                score *= 0.1;
            }
            Some(RankedHit { memory: m, score, sim, recency, strength, superseded })
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    if min_score > 0.0 {
        scored.retain(|h| h.score >= min_score);
    }
    Ok(scored)
}

/// Fallback search when embedding the query failed (e.g. ollama is down):
/// a plain case-insensitive substring match over content, newest first.
/// Every non-superseded hit gets score 1.0 (not a meaningful ranking);
/// a superseded hit included via `include_superseded` gets 0.1, mirroring
/// `search_ranked`'s treatment.
pub fn search_substring(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_all: bool,
    include_superseded: bool,
) -> Result<Vec<(Memory, f32)>, KbError> {
    let needle = query.to_lowercase();
    let mut hits: Vec<(Memory, f32)> = candidates(conn, include_all, include_superseded)?
        .into_iter()
        .filter(|m| m.content.to_lowercase().contains(&needle))
        .map(|m| {
            let score = if m.is_superseded() { 0.1 } else { 1.0 };
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
        .query_row("SELECT last_run_at, last_memory_id FROM reflect_state WHERE id = 1", [], |r| {
            Ok(ReflectState { last_run_at: r.get(0)?, last_memory_id: r.get(1)? })
        })
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
             superseded_by, dormant_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
         ON CONFLICT(id) DO UPDATE SET
            content = excluded.content, source = excluded.source, project = excluded.project,
            created_at = excluded.created_at, reviewed = excluded.reviewed, embedding = excluded.embedding,
            importance = excluded.importance, stability = excluded.stability,
            access_count = excluded.access_count, first_accessed_at = excluded.first_accessed_at,
            last_accessed_at = excluded.last_accessed_at, valid_from = excluded.valid_from,
            invalidated_at = excluded.invalidated_at, superseded_by = excluded.superseded_by,
            dormant_at = excluded.dormant_at",
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
/// `created_at`, `last_accessed_at`, `invalidated_at` and `dormant_at` —
/// used by `mach kb import --merge`'s last-write-wins comparison. Every
/// timestamp this store writes is a fixed-width RFC3339 string, so a plain
/// lexicographic max is exact here, no parsing needed.
pub fn memory_last_modified(m: &Memory) -> &str {
    let mut latest = m.created_at.as_str();
    for candidate in [m.last_accessed_at.as_deref(), m.invalidated_at.as_deref(), m.dormant_at.as_deref()] {
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
    fn ranking_excludes_unreviewed_by_default_but_all_includes_them() {
        let conn = mem_conn();
        let emb = fake_embed("shared topic");
        insert5(&conn, "reviewed memory shared topic", Some(&emb));
        insert(&conn, "unreviewed memory shared topic", None, None, false, Some(&emb), 5).unwrap();

        let now = now_rfc3339();
        let default_results = search_ranked(&conn, &emb, 10, false, false, 0.0, &now).unwrap();
        assert_eq!(default_results.len(), 1);
        assert_eq!(default_results[0].memory.content, "reviewed memory shared topic");

        let all_results = search_ranked(&conn, &emb, 10, true, false, 0.0, &now).unwrap();
        assert_eq!(all_results.len(), 2);
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
        // tables), v2->v3 (the insights `level` column), then v3->v4 (the
        // memories `dormant_at` column), landing at the current version.
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 4);

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
            version, 4,
            "v1->v2 (reflection tables), v2->v3 (insights level column), v3->v4 (dormant_at) all run"
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
    fn fresh_database_lands_at_user_version_4() {
        let conn = mem_conn();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 4);
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
        assert_eq!(version, 4, "v2->v3 (level column) then v3->v4 (dormant_at) both run");

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
        assert_eq!(version, 4);

        let rows = list(&conn, None, false).unwrap();
        assert_eq!(rows.len(), 1, "existing memory row must survive the migration");
        assert!(rows[0].dormant_at.is_none(), "pre-existing row must backfill to active (not dormant)");
        assert!(!rows[0].is_dormant());

        // idempotent on repeat
        migrate(&conn).unwrap();
        assert_eq!(list(&conn, None, false).unwrap().len(), 1);
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
    fn dormancy_unreviewed_row_older_than_30_days_qualifies_regardless_of_everything_else() {
        // 35 days old (well under the 90-day floor), touched recently,
        // importance 10, cited — every ordinary criterion says "no" — but
        // it's unreviewed, so the absolute rule overrides all of them.
        let m = dormancy_fixture(35.0, 5, Some(1.0), 10, false);
        assert!(memory_qualifies_for_dormancy(&m, true, &now_rfc3339()), "unreviewed + >30 days must always qualify");
    }

    #[test]
    fn dormancy_unreviewed_row_at_or_under_30_days_does_not_qualify_via_the_absolute_rule() {
        let m = dormancy_fixture(30.0, 0, None, 10, false);
        // Fails the absolute rule (not yet strictly over 30 days) AND the
        // ordinary rule (well under the 90-day floor, importance too high).
        assert!(!memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
    }

    #[test]
    fn dormancy_reviewed_old_low_importance_uncited_row_under_30_days_old_still_needs_the_90_day_floor() {
        // Reviewed, so the unreviewed absolute rule never applies; must
        // fall through to the ordinary >=90-day check and fail it.
        let m = dormancy_fixture(20.0, 0, None, 3, true);
        assert!(!memory_qualifies_for_dormancy(&m, false, &now_rfc3339()));
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
        let ranked = search_ranked(&conn, &emb, 10, true, false, 0.0, &now).unwrap();
        assert!(ranked.iter().all(|h| h.memory.id != sleeping_id), "search_ranked must exclude dormant rows even with --all");
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
}
