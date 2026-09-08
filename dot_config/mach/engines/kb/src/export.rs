
//! `mach kb export` / `mach kb import` — full-fidelity JSONL backup and its
//! merge-import counterpart.
//!
//! The format is deliberately the future cross-machine sync primitive: one
//! JSON object per line, a leading header (`kind`/`version`/`exported_at`),
//! then every `memories` row, then every `insights` row, then the single
//! `reflect_state` row — every column, including embeddings (base64, since
//! JSON has no native byte-string type) and dormant/invalidated rows. A
//! plain-text line-oriented format rather than a single JSON document so a
//! future incremental sync can append/stream rows without re-serializing
//! the whole export, and so `git diff` on a checked-in backup stays
//! line-granular.
//!
//! Import without `--merge` refuses outright on a non-empty database (the
//! safe default: never silently blend two stores). With `--merge`, each
//! incoming row is upserted by id using last-write-wins (`store::
//! memory_last_modified` / `insight_last_modified`) against the existing
//! row of the same id — an identical row is skipped, a strictly older one
//! is left alone, everything else is written. Embeddings are imported
//! as-is (both machines assumed to be running the same embedding model).
use std::io::{BufRead, Write};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::store::{self, Insight, KbError, Memory};

pub const EXPORT_VERSION: u32 = 1;
pub const EXPORT_KIND: &str = "mach-kb-export";

// --- hand-rolled base64 (standard alphabet, padded) ---
//
// The workspace hand-rolls its own date/time arithmetic (see store.rs) and
// its own CLI arg parsing rather than reach for a crate for either — an
// embedding blob's base64 encoding is exactly as small and self-contained,
// so it follows the same convention instead of adding a dependency.
mod b64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b0 = chunk[0];
            let b1 = *chunk.get(1).unwrap_or(&0);
            let b2 = *chunk.get(2).unwrap_or(&0);
            let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
            out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
            out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 0x3f) as usize] as char } else { '=' });
            out.push(if chunk.len() > 2 { ALPHABET[(n & 0x3f) as usize] as char } else { '=' });
        }
        out
    }

    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            other => Err(format!("invalid base64 byte: {:?}", other as char)),
        }
    }

    /// Decodes standard base64 (padding optional — `=` is simply stripped
    /// before decoding, so a value that omits it round-trips too).
    pub fn decode(s: &str) -> Result<Vec<u8>, String> {
        let bytes: Vec<u8> = s.trim().bytes().filter(|&b| b != b'=').collect();
        let mut out = Vec::with_capacity(bytes.len() * 3 / 4 + 3);
        for chunk in bytes.chunks(4) {
            let mut n: u32 = 0;
            for (i, &b) in chunk.iter().enumerate() {
                n |= (val(b)? as u32) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn roundtrips_arbitrary_byte_lengths() {
            for data in [
                Vec::<u8>::new(),
                vec![0u8],
                vec![1u8, 2],
                vec![1u8, 2, 3],
                vec![1u8, 2, 3, 4],
                vec![0xffu8, 0x00, 0x7f, 0x80, 0x01],
                (0u8..=255).collect::<Vec<u8>>(),
            ] {
                let encoded = encode(&data);
                assert_eq!(decode(&encoded).unwrap(), data, "roundtrip failed for {:?}", data);
            }
        }

        #[test]
        fn rejects_invalid_byte() {
            assert!(decode("!!!!").is_err());
        }
    }
}

// --- JSONL row shapes ---

#[derive(Serialize, Deserialize)]
struct ExportHeader {
    kind: String,
    version: u32,
    exported_at: String,
}

#[derive(Serialize, Deserialize)]
struct MemoryRow {
    table: String,
    id: i64,
    content: String,
    source: Option<String>,
    project: Option<String>,
    created_at: String,
    reviewed: bool,
    embedding_b64: Option<String>,
    importance: i64,
    stability: Option<f64>,
    access_count: i64,
    first_accessed_at: Option<String>,
    last_accessed_at: Option<String>,
    valid_from: String,
    invalidated_at: Option<String>,
    superseded_by: Option<i64>,
    dormant_at: Option<String>,
    #[serde(default)]
    last_verified_at: Option<String>,
    #[serde(default)]
    graph_extracted_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct InsightRow {
    table: String,
    id: i64,
    text: String,
    created_at: String,
    confidence: f64,
    source_ids: Vec<String>,
    embedding_b64: Option<String>,
    invalidated_at: Option<String>,
    flagged_at: Option<String>,
    last_verified_at: Option<String>,
    level: i64,
}

#[derive(Serialize, Deserialize)]
struct ReflectStateRow {
    table: String,
    last_run_at: Option<String>,
    last_memory_id: Option<i64>,
}

fn memory_to_row(m: &Memory) -> MemoryRow {
    MemoryRow {
        table: "memories".to_string(),
        id: m.id,
        content: m.content.clone(),
        source: m.source.clone(),
        project: m.project.clone(),
        created_at: m.created_at.clone(),
        reviewed: m.reviewed,
        embedding_b64: m.embedding.as_deref().map(|e| b64::encode(&store::encode_embedding(e))),
        importance: m.importance,
        stability: m.stability,
        access_count: m.access_count,
        first_accessed_at: m.first_accessed_at.clone(),
        last_accessed_at: m.last_accessed_at.clone(),
        valid_from: m.valid_from.clone(),
        invalidated_at: m.invalidated_at.clone(),
        superseded_by: m.superseded_by,
        dormant_at: m.dormant_at.clone(),
        last_verified_at: m.last_verified_at.clone(),
        graph_extracted_at: m.graph_extracted_at.clone(),
    }
}

fn row_to_memory(row: MemoryRow) -> Result<Memory, KbError> {
    let embedding = match row.embedding_b64 {
        Some(b64_str) => {
            let bytes = b64::decode(&b64_str).map_err(KbError::Other)?;
            Some(store::decode_embedding(&bytes))
        }
        None => None,
    };
    Ok(Memory {
        id: row.id,
        content: row.content,
        source: row.source,
        project: row.project,
        created_at: row.created_at,
        reviewed: row.reviewed,
        embedding,
        importance: row.importance,
        stability: row.stability,
        access_count: row.access_count,
        first_accessed_at: row.first_accessed_at,
        last_accessed_at: row.last_accessed_at,
        valid_from: row.valid_from,
        invalidated_at: row.invalidated_at,
        superseded_by: row.superseded_by,
        dormant_at: row.dormant_at,
        last_verified_at: row.last_verified_at,
        graph_extracted_at: row.graph_extracted_at,
    })
}

fn insight_to_row(i: &Insight) -> InsightRow {
    InsightRow {
        table: "insights".to_string(),
        id: i.id,
        text: i.text.clone(),
        created_at: i.created_at.clone(),
        confidence: i.confidence,
        source_ids: i.source_ids.clone(),
        embedding_b64: i.embedding.as_deref().map(|e| b64::encode(&store::encode_embedding(e))),
        invalidated_at: i.invalidated_at.clone(),
        flagged_at: i.flagged_at.clone(),
        last_verified_at: i.last_verified_at.clone(),
        level: i.level,
    }
}

fn row_to_insight(row: InsightRow) -> Result<Insight, KbError> {
    let embedding = match row.embedding_b64 {
        Some(b64_str) => {
            let bytes = b64::decode(&b64_str).map_err(KbError::Other)?;
            Some(store::decode_embedding(&bytes))
        }
        None => None,
    };
    Ok(Insight {
        id: row.id,
        text: row.text,
        created_at: row.created_at,
        confidence: row.confidence,
        source_ids: row.source_ids,
        embedding,
        invalidated_at: row.invalidated_at,
        flagged_at: row.flagged_at,
        last_verified_at: row.last_verified_at,
        level: row.level,
    })
}

// --- export ---

/// Row counts actually written — reported by `mach kb export`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExportCounts {
    pub memories: usize,
    pub insights: usize,
}

/// Writes the full-fidelity JSONL export to `out`: the header line, then
/// every memory row (any state), then every insight row (any state), then
/// the `reflect_state` singleton — in that fixed order, one JSON object per
/// line.
pub fn export_to_writer<W: Write>(conn: &Connection, out: &mut W) -> Result<ExportCounts, KbError> {
    let header =
        ExportHeader { kind: EXPORT_KIND.to_string(), version: EXPORT_VERSION, exported_at: store::now_rfc3339() };
    writeln!(out, "{}", serde_json::to_string(&header)?)?;

    let memories = store::all_memories(conn)?;
    for m in &memories {
        writeln!(out, "{}", serde_json::to_string(&memory_to_row(m))?)?;
    }

    let insights = store::all_insights(conn)?;
    for i in &insights {
        writeln!(out, "{}", serde_json::to_string(&insight_to_row(i))?)?;
    }

    let state = store::get_reflect_state(conn)?;
    let state_row =
        ReflectStateRow { table: "reflect_state".to_string(), last_run_at: state.last_run_at, last_memory_id: state.last_memory_id };
    writeln!(out, "{}", serde_json::to_string(&state_row)?)?;

    Ok(ExportCounts { memories: memories.len(), insights: insights.len() })
}

// --- import ---

/// Per-table outcome counts — reported by `mach kb import`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ImportCounts {
    pub memories_inserted: usize,
    pub memories_updated: usize,
    pub memories_skipped: usize,
    pub insights_inserted: usize,
    pub insights_updated: usize,
    pub insights_skipped: usize,
    pub reflect_state_updated: bool,
}

/// Reads a `mach kb export` JSONL stream and applies it to `conn`.
///
/// Without `merge`, refuses outright if the database already has any
/// memory or insight rows (`store::is_db_empty`) — a plain import is only
/// ever a fresh restore, never a silent blend. With `merge`, each row is
/// upserted by id: an id absent locally is inserted; one present is
/// compared by last-write-wins (`store::memory_last_modified` /
/// `insight_last_modified`) — identical rows are skipped, a strictly older
/// incoming row is skipped, everything else overwrites in place.
/// `reflect_state` (a singleton) follows the same last-write-wins rule
/// against its own `last_run_at`.
pub fn import_from_reader<R: BufRead>(conn: &Connection, reader: R, merge: bool) -> Result<ImportCounts, KbError> {
    if !merge && !store::is_db_empty(conn)? {
        return Err(KbError::Other(
            "refusing to import into a non-empty database without --merge".to_string(),
        ));
    }

    let mut counts = ImportCounts::default();
    let mut saw_header = false;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| KbError::Other(format!("malformed export line: {}", e)))?;

        if !saw_header {
            let kind = value.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind != EXPORT_KIND {
                return Err(KbError::Other("not a mach-kb export file (missing/invalid header)".to_string()));
            }
            saw_header = true;
            continue;
        }

        let table = value.get("table").and_then(|v| v.as_str()).unwrap_or("").to_string();
        match table.as_str() {
            "memories" => {
                let row: MemoryRow = serde_json::from_value(value)?;
                import_memory_row(conn, row, merge, &mut counts)?;
            }
            "insights" => {
                let row: InsightRow = serde_json::from_value(value)?;
                import_insight_row(conn, row, merge, &mut counts)?;
            }
            "reflect_state" => {
                let row: ReflectStateRow = serde_json::from_value(value)?;
                import_reflect_state_row(conn, row, merge, &mut counts)?;
            }
            other => {
                return Err(KbError::Other(format!("unknown table '{}' in export line", other)));
            }
        }
    }

    if !saw_header {
        return Err(KbError::Other("empty or missing export header".to_string()));
    }
    Ok(counts)
}

fn import_memory_row(conn: &Connection, row: MemoryRow, merge: bool, counts: &mut ImportCounts) -> Result<(), KbError> {
    let incoming = row_to_memory(row)?;
    match store::get(conn, incoming.id)? {
        None => {
            store::raw_upsert_memory(conn, &incoming)?;
            counts.memories_inserted += 1;
        }
        Some(existing) => {
            if !merge {
                // Reached only if a row appeared mid-import (concurrent
                // writer) after the up-front is_db_empty check passed —
                // never silently overwrite without --merge's explicit
                // consent.
                return Err(KbError::Other(format!(
                    "memory #{} already exists (pass --merge to upsert)",
                    incoming.id
                )));
            }
            if existing == incoming {
                counts.memories_skipped += 1;
            } else if store::memory_last_modified(&incoming) >= store::memory_last_modified(&existing) {
                store::raw_upsert_memory(conn, &incoming)?;
                counts.memories_updated += 1;
            } else {
                counts.memories_skipped += 1;
            }
        }
    }
    Ok(())
}

fn import_insight_row(conn: &Connection, row: InsightRow, merge: bool, counts: &mut ImportCounts) -> Result<(), KbError> {
    let incoming = row_to_insight(row)?;
    match store::get_insight(conn, incoming.id)? {
        None => {
            store::raw_upsert_insight(conn, &incoming)?;
            counts.insights_inserted += 1;
        }
        Some(existing) => {
            if !merge {
                return Err(KbError::Other(format!(
                    "insight #{} already exists (pass --merge to upsert)",
                    incoming.id
                )));
            }
            if existing == incoming {
                counts.insights_skipped += 1;
            } else if store::insight_last_modified(&incoming) >= store::insight_last_modified(&existing) {
                store::raw_upsert_insight(conn, &incoming)?;
                counts.insights_updated += 1;
            } else {
                counts.insights_skipped += 1;
            }
        }
    }
    Ok(())
}

fn import_reflect_state_row(
    conn: &Connection,
    row: ReflectStateRow,
    merge: bool,
    counts: &mut ImportCounts,
) -> Result<(), KbError> {
    let existing = store::get_reflect_state(conn)?;
    // Without --merge the db was just confirmed empty of memories/insights,
    // so the watermark is meaningless either way — always take the
    // incoming value. With --merge, last-write-wins on last_run_at (NULL
    // — never run — always loses to any real timestamp).
    let take_incoming = !merge
        || match (&row.last_run_at, &existing.last_run_at) {
            (Some(incoming), Some(cur)) => incoming >= cur,
            (Some(_), None) => true,
            (None, _) => false,
        };
    if take_incoming {
        store::update_reflect_state(conn, row.last_run_at.as_deref().unwrap_or(""), row.last_memory_id)?;
        counts.reflect_state_updated = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mem_conn() -> Connection {
        store::open_with_path(std::path::Path::new(":memory:")).expect("open in-memory store")
    }

    fn fake_embed(seed: u8) -> Vec<f32> {
        vec![seed as f32, (seed as f32) * 2.0, -(seed as f32)]
    }

    #[test]
    fn export_then_import_into_empty_db_roundtrips_row_counts_and_content() {
        let src = mem_conn();
        let id1 = store::insert(&src, "first memory", Some("test"), Some("proj"), true, Some(&fake_embed(3)), 7).unwrap();
        let id2 = store::insert(&src, "second memory", None, None, false, None, 2).unwrap();
        let now = store::now_rfc3339();
        store::supersede(&src, id2, id1, &now).unwrap();
        let ins_id = store::insert_insight(&src, "an insight", 0.6, &[id1.to_string()], Some(&fake_embed(9))).unwrap();
        store::update_reflect_state(&src, &now, Some(id1)).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        let counts = export_to_writer(&src, &mut buf).unwrap();
        assert_eq!(counts.memories, 2);
        assert_eq!(counts.insights, 1);

        let dst = mem_conn();
        let import_counts = import_from_reader(&dst, Cursor::new(buf), false).unwrap();
        assert_eq!(import_counts.memories_inserted, 2);
        assert_eq!(import_counts.insights_inserted, 1);
        assert!(import_counts.reflect_state_updated);

        let m1 = store::get(&dst, id1).unwrap().unwrap();
        assert_eq!(m1.content, "first memory");
        assert_eq!(m1.embedding.unwrap(), fake_embed(3), "embedding must survive the base64 round trip exactly");
        assert_eq!(m1.importance, 7);

        let m2 = store::get(&dst, id2).unwrap().unwrap();
        assert!(m2.is_superseded(), "tombstoned state must be preserved");
        assert_eq!(m2.superseded_by, Some(id1));

        let ins = store::get_insight(&dst, ins_id).unwrap().unwrap();
        assert_eq!(ins.text, "an insight");
        assert_eq!(ins.embedding.unwrap(), fake_embed(9));

        let state = store::get_reflect_state(&dst).unwrap();
        assert_eq!(state.last_memory_id, Some(id1));
    }

    #[test]
    fn export_includes_dormant_rows() {
        let src = mem_conn();
        let id = store::insert(&src, "will go dormant", None, None, true, None, 3).unwrap();
        let now = store::now_rfc3339();
        store::set_dormant(&src, id, &now).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        let counts = export_to_writer(&src, &mut buf).unwrap();
        assert_eq!(counts.memories, 1, "a dormant row must still be exported (full fidelity)");

        let dst = mem_conn();
        import_from_reader(&dst, Cursor::new(buf), false).unwrap();
        let m = store::get(&dst, id).unwrap().unwrap();
        assert!(m.is_dormant(), "dormant status must survive the round trip");
    }

    #[test]
    fn import_without_merge_refuses_on_non_empty_db() {
        let src = mem_conn();
        store::insert(&src, "a memory", None, None, true, None, 5).unwrap();
        let mut buf: Vec<u8> = Vec::new();
        export_to_writer(&src, &mut buf).unwrap();

        let dst = mem_conn();
        store::insert(&dst, "already something here", None, None, true, None, 5).unwrap();
        let err = import_from_reader(&dst, Cursor::new(buf), false).unwrap_err();
        assert!(err.to_string().contains("--merge"));
    }

    #[test]
    fn import_merge_last_write_wins_overwrites_only_when_incoming_is_not_older() {
        let dst = mem_conn();
        let id = store::insert(&dst, "old content", None, None, true, None, 5).unwrap();
        // Age the local row's created_at back so the incoming row (fresher
        // created_at) is unambiguously the newer one.
        dst.execute(
            "UPDATE memories SET created_at = '2020-01-01T00:00:00Z', valid_from = '2020-01-01T00:00:00Z' WHERE id = ?1",
            rusqlite::params![id],
        )
        .unwrap();

        let src = mem_conn();
        // Same id, fresher content, imported straight from a hand-built
        // export line rather than via a real insert (which would pick its
        // own id) — simulates "the same logical row from another machine".
        let now = store::now_rfc3339();
        let row = MemoryRow {
            table: "memories".to_string(),
            id,
            content: "new content from another machine".to_string(),
            source: None,
            project: None,
            created_at: now.clone(),
            reviewed: true,
            embedding_b64: None,
            importance: 5,
            stability: Some(35.0),
            access_count: 0,
            first_accessed_at: None,
            last_accessed_at: None,
            valid_from: now,
            invalidated_at: None,
            superseded_by: None,
            dormant_at: None,
            last_verified_at: None,
            graph_extracted_at: None,
        };
        let mut buf: Vec<u8> = Vec::new();
        writeln!(
            &mut buf,
            "{}",
            serde_json::to_string(&ExportHeader {
                kind: EXPORT_KIND.to_string(),
                version: EXPORT_VERSION,
                exported_at: store::now_rfc3339()
            })
            .unwrap()
        )
        .unwrap();
        writeln!(&mut buf, "{}", serde_json::to_string(&row).unwrap()).unwrap();
        let _ = src; // only used to keep the fixture symmetrical with other tests

        let counts = import_from_reader(&dst, Cursor::new(buf.clone()), true).unwrap();
        assert_eq!(counts.memories_updated, 1);
        let m = store::get(&dst, id).unwrap().unwrap();
        assert_eq!(m.content, "new content from another machine");

        // Re-importing the SAME (now older-relative-to-local) line again
        // must be a no-op skip, not another overwrite — the local row is
        // now the newer one (it was just written with `now`, and the
        // incoming row's created_at hasn't changed).
        let now2 = store::now_rfc3339();
        dst.execute("UPDATE memories SET created_at = ?1 WHERE id = ?2", rusqlite::params![now2, id]).unwrap();
        let counts2 = import_from_reader(&dst, Cursor::new(buf), true).unwrap();
        assert_eq!(counts2.memories_skipped, 1, "incoming row is now strictly older — must be skipped");
        let m2 = store::get(&dst, id).unwrap().unwrap();
        assert_eq!(m2.content, "new content from another machine", "the newer local content must survive");
    }

    #[test]
    fn import_merge_skips_identical_rows() {
        let dst = mem_conn();
        let id = store::insert(&dst, "unchanged", None, None, true, None, 5).unwrap();
        let m = store::get(&dst, id).unwrap().unwrap();

        let mut buf: Vec<u8> = Vec::new();
        export_to_writer(&dst, &mut buf).unwrap();
        let _ = m;

        let counts = import_from_reader(&dst, Cursor::new(buf), true).unwrap();
        assert_eq!(counts.memories_skipped, 1);
        assert_eq!(counts.memories_updated, 0);
    }

    #[test]
    fn import_rejects_a_file_without_the_export_header() {
        let dst = mem_conn();
        let bogus = b"{\"table\":\"memories\",\"id\":1}\n".to_vec();
        let err = import_from_reader(&dst, Cursor::new(bogus), false).unwrap_err();
        assert!(err.to_string().contains("header"));
    }

    #[test]
    fn import_rejects_empty_input() {
        let dst = mem_conn();
        let err = import_from_reader(&dst, Cursor::new(Vec::new()), false).unwrap_err();
        assert!(err.to_string().contains("header"));
    }
}
