
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

#[derive(Debug, Clone)]
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
            superseded_by INTEGER
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
/// `stability` on existing rows. Runs on every `open`, but the version gate
/// makes every call after the first one a single cheap `PRAGMA` read.
fn migrate(conn: &Connection) -> Result<(), KbError> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version >= 1 {
        return Ok(());
    }
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
/// (`mach kb list --superseded`).
pub fn list(conn: &Connection, limit: Option<usize>, superseded_only: bool) -> Result<Vec<Memory>, KbError> {
    let where_clause = if superseded_only {
        "WHERE invalidated_at IS NOT NULL"
    } else {
        "WHERE invalidated_at IS NULL"
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

/// Candidate rows for a search: reviewed-only unless `include_all`, and
/// active-only (not tombstoned) unless `include_superseded`.
fn candidates(conn: &Connection, include_all: bool, include_superseded: bool) -> Result<Vec<Memory>, KbError> {
    let mut clauses = Vec::new();
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
        migrate(&conn).unwrap();

        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 1);

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

        // idempotent: running it again (as every `open` does) is a no-op
        migrate(&conn).unwrap();
        let rows_again = list(&conn, None, false).unwrap();
        assert_eq!(rows_again.len(), 1);
    }

    #[test]
    fn fresh_database_lands_at_user_version_1() {
        let conn = mem_conn();
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(version, 1);
    }

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
}
