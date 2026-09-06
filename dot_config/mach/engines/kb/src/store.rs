
//! SQLite-backed store for the kb engine: schema, CRUD, and brute-force
//! cosine ranking over BLOB-encoded embeddings. See the crate doc in
//! `lib.rs` for why cosine-over-BLOB was chosen over a `sqlite-vec` virtual
//! table.
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

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
            embedding BLOB
        );",
    )?;
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
    init_schema(&conn)?;
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
// domain) — used instead of pulling in a date/time crate for one timestamp
// column.
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

fn now_rfc3339() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = secs / 86400;
    let tod = secs % 86400;
    let (h, mi, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn row_to_memory(row: &rusqlite::Row) -> rusqlite::Result<Memory> {
    let reviewed_int: i64 = row.get("reviewed")?;
    let blob: Option<Vec<u8>> = row.get("embedding")?;
    Ok(Memory {
        id: row.get("id")?,
        content: row.get("content")?,
        source: row.get("source")?,
        project: row.get("project")?,
        created_at: row.get("created_at")?,
        reviewed: reviewed_int != 0,
        embedding: blob.map(|b| decode_embedding(&b)),
    })
}

/// Inserts a new memory (with the current time as `created_at`) and returns
/// its id.
pub fn insert(
    conn: &Connection,
    content: &str,
    source: Option<&str>,
    project: Option<&str>,
    reviewed: bool,
    embedding: Option<&[f32]>,
) -> Result<i64, KbError> {
    let created_at = now_rfc3339();
    let blob = embedding.map(encode_embedding);
    conn.execute(
        "INSERT INTO memories (content, source, project, created_at, reviewed, embedding)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![content, source, project, created_at, reviewed as i64, blob],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Most recent memories first, optionally capped to `limit` rows.
pub fn list(conn: &Connection, limit: Option<usize>) -> Result<Vec<Memory>, KbError> {
    let sql = match limit {
        Some(n) => format!("SELECT * FROM memories ORDER BY id DESC LIMIT {}", n),
        None => "SELECT * FROM memories ORDER BY id DESC".to_string(),
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

/// Deletes a memory by id. Returns whether a row existed to delete.
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

/// Candidate rows for a search: reviewed-only unless `include_all`.
fn candidates(conn: &Connection, include_all: bool) -> Result<Vec<Memory>, KbError> {
    let sql = if include_all {
        "SELECT * FROM memories"
    } else {
        "SELECT * FROM memories WHERE reviewed = 1"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], row_to_memory)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Cosine top-N search against rows that have an embedding stored. Returns
/// `(memory, score)` pairs sorted by descending score.
pub fn search(
    conn: &Connection,
    query_embedding: &[f32],
    limit: usize,
    include_all: bool,
) -> Result<Vec<(Memory, f32)>, KbError> {
    let mut scored: Vec<(Memory, f32)> = candidates(conn, include_all)?
        .into_iter()
        .filter_map(|m| {
            let score = match &m.embedding {
                Some(e) if !e.is_empty() => cosine(query_embedding, e),
                _ => return None,
            };
            Some((m, score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(limit);
    Ok(scored)
}

/// Fallback search when embedding the query failed (e.g. ollama is down):
/// a plain case-insensitive substring match over content, newest first.
/// Scores are not meaningful ranking here — every hit gets 1.0.
pub fn search_substring(
    conn: &Connection,
    query: &str,
    limit: usize,
    include_all: bool,
) -> Result<Vec<(Memory, f32)>, KbError> {
    let needle = query.to_lowercase();
    let mut hits: Vec<(Memory, f32)> = candidates(conn, include_all)?
        .into_iter()
        .filter(|m| m.content.to_lowercase().contains(&needle))
        .map(|m| (m, 1.0f32))
        .collect();
    hits.sort_by(|a, b| b.0.id.cmp(&a.0.id));
    hits.truncate(limit);
    Ok(hits)
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
    fn insert_and_get_roundtrip() {
        let conn = mem_conn();
        let emb = fake_embed("hello");
        let id = insert(&conn, "hello", Some("test"), Some("proj"), true, Some(&emb)).unwrap();
        let m = get(&conn, id).unwrap().expect("row exists");
        assert_eq!(m.content, "hello");
        assert_eq!(m.source.as_deref(), Some("test"));
        assert_eq!(m.project.as_deref(), Some("proj"));
        assert!(m.reviewed);
        assert_eq!(m.embedding.unwrap(), emb);
        assert!(!m.created_at.is_empty());
    }

    #[test]
    fn list_orders_newest_first_and_respects_limit() {
        let conn = mem_conn();
        for c in ["one", "two", "three"] {
            insert(&conn, c, None, None, true, None).unwrap();
        }
        let all = list(&conn, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].content, "three");
        assert_eq!(all[2].content, "one");

        let capped = list(&conn, Some(2)).unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].content, "three");
    }

    #[test]
    fn unreviewed_only_returns_reviewed_zero_rows() {
        let conn = mem_conn();
        insert(&conn, "kept", None, None, true, None).unwrap();
        let candidate_id = insert(&conn, "candidate", None, None, false, None).unwrap();
        let pending = unreviewed(&conn).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, candidate_id);
        assert_eq!(pending[0].content, "candidate");
    }

    #[test]
    fn set_reviewed_flips_flag_and_removes_from_unreviewed() {
        let conn = mem_conn();
        let id = insert(&conn, "candidate", None, None, false, None).unwrap();
        assert!(set_reviewed(&conn, id, true).unwrap());
        assert!(unreviewed(&conn).unwrap().is_empty());
        assert!(get(&conn, id).unwrap().unwrap().reviewed);
    }

    #[test]
    fn update_content_with_new_embedding() {
        let conn = mem_conn();
        let id = insert(&conn, "old", None, None, true, Some(&fake_embed("old"))).unwrap();
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
        let id = insert(&conn, "old", None, None, true, Some(&old_emb)).unwrap();
        assert!(update_content(&conn, id, "edited", None).unwrap());
        let m = get(&conn, id).unwrap().unwrap();
        assert_eq!(m.content, "edited");
        assert_eq!(m.embedding.unwrap(), old_emb);
    }

    #[test]
    fn delete_removes_row_and_reports_existence() {
        let conn = mem_conn();
        let id = insert(&conn, "x", None, None, true, None).unwrap();
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
        assert!(!delete(&conn, id).unwrap(), "second delete of same id reports false");
    }

    #[test]
    fn search_ranks_by_cosine_and_skips_rows_without_embeddings() {
        let conn = mem_conn();
        insert(&conn, "the boss wants Q4 retention metrics", None, None, true,
            Some(&fake_embed("the boss wants Q4 retention metrics"))).unwrap();
        insert(&conn, "unrelated quantum chess trivia", None, None, true,
            Some(&fake_embed("unrelated quantum chess trivia"))).unwrap();
        insert(&conn, "no embedding stored for this one", None, None, true, None).unwrap();

        let query = fake_embed("what does my boss want for Q4");
        let results = search(&conn, &query, 5, false).unwrap();

        // the no-embedding row must never surface from a cosine search
        assert!(results.iter().all(|(m, _)| m.content != "no embedding stored for this one"));
        assert!(!results.is_empty());
        assert_eq!(results[0].0.content, "the boss wants Q4 retention metrics");
        assert!(results[0].1 > results.last().unwrap().1);
    }

    #[test]
    fn search_excludes_unreviewed_by_default_but_all_includes_them() {
        let conn = mem_conn();
        let emb = fake_embed("shared topic");
        insert(&conn, "reviewed memory shared topic", None, None, true, Some(&emb)).unwrap();
        insert(&conn, "unreviewed memory shared topic", None, None, false, Some(&emb)).unwrap();

        let default_results = search(&conn, &emb, 10, false).unwrap();
        assert_eq!(default_results.len(), 1);
        assert_eq!(default_results[0].0.content, "reviewed memory shared topic");

        let all_results = search(&conn, &emb, 10, true).unwrap();
        assert_eq!(all_results.len(), 2);
    }

    #[test]
    fn search_respects_limit() {
        let conn = mem_conn();
        for i in 0..5 {
            let text = format!("memory number {}", i);
            insert(&conn, &text, None, None, true, Some(&fake_embed(&text))).unwrap();
        }
        let results = search(&conn, &fake_embed("memory number 2"), 2, false).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn search_substring_fallback_matches_case_insensitively() {
        let conn = mem_conn();
        insert(&conn, "The Boss wants retention metrics", None, None, true, None).unwrap();
        insert(&conn, "completely different topic", None, None, true, None).unwrap();

        let results = search_substring(&conn, "boss", 10, false).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.content, "The Boss wants retention metrics");

        let none = search_substring(&conn, "nonexistent phrase", 10, false).unwrap();
        assert!(none.is_empty());
    }
}
