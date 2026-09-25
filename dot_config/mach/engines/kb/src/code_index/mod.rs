//! Code index: the storage layer behind `mach kb index` and the code-aware
//! `mach kb ask`. Design: `docs/superpowers/specs/2026-09-23-code-index-design.md`.
//!
//! Schema and CRUD live in `store.rs` (schema v32, `migrate_v31_to_v32`),
//! right next to the transcript-chunk functions it mirrors: `code_scope`
//! (per-directory index/skip decisions), `code_files` (per-file indexing
//! state), and `code_chunks` + `code_chunks_fts` (the chunks themselves,
//! hybrid-searchable). Like transcript chunks, none of this is a
//! `memories` row -- it is derived state, parsed out of the git repos on
//! disk, droppable and rebuildable at any time.
//!
//! Phase 2 (schema v33, `migrate_v32_to_v33`) adds `code_summaries` +
//! `code_summaries_fts`: file/module/repo summary text, still not a
//! `memories` row for file summaries, but module/repo summaries are ALSO
//! mirrored into `memories` as index-owned rows via `upsert_index_memory`
//! (`store::supersession_guard`'s `"index-owned"` block keeps every
//! automatic supersession pass off them; only the indexer itself replaces
//! one, by calling `upsert_index_memory` again).
//!
//! Phase 3 (schema v35, `migrate_v34_to_v35`) adds the symbol graph:
//! `code_symbols` (one row per definition) and `code_edges` (one row per
//! call/include/import/inherits reference), both populated by
//! `chunk::extract_refs` -- never an LLM -- and written with
//! `replace_file_symbols`/`delete_file_symbols`/`move_file_symbols`, the
//! same delete-then-insert-per-file shape `replace_code_chunks` uses.
//! `resolve_edges` links `dst_symbol_id` up project-wide afterward, since a
//! callee can live in a file the current pass never touched.
//!
//! Task 3 adds `history.rs`: monthly commit-history summaries, one sonnet
//! call per calendar month of a project's git log, written to `code_history`
//! (created alongside the symbol graph above) and mirrored into `memories`
//! as index-owned, dated rows with source `code-history:<project>:<YYYY-MM>`
//! -- see `history.rs`'s own doc comment. `store::INDEX_OWNED_SOURCE_PREFIX`
//! (`code-index:`) and `store::CODE_HISTORY_SOURCE_PREFIX`
//! (`code-history:`) are both covered, everywhere an index-owned row must be
//! excluded, by the shared `store::is_index_owned`/`is_index_owned_source`/
//! `not_index_owned_sql` -- a third owned prefix, if one is ever added, only
//! needs to change there.
//!
//! This module re-exports that storage surface under one name for the
//! rest of the code-index feature: `git.rs` (repo access), `chunk.rs`
//! (tree-sitter chunking), `job.rs` (the nightly indexing job, file-level
//! headers and summaries), `summary.rs` (bottom-up module/repo summaries,
//! called from `job.rs` at the end of each project's run -- see its own
//! doc comment), `history.rs` (monthly commit-history summaries, called
//! from `job.rs` right after `summary.rs`), and the agentic `ask` tools.

pub use crate::store::{
    callees_of, callers_of, code_chunks_missing_embedding, code_file_delete, code_file_get, code_file_set_status,
    code_file_upsert, code_files_with_status, code_history_get, code_history_list, code_history_set_memory_id,
    code_history_upsert, code_scope_get, code_scope_set, code_summaries_missing_embedding, code_summary_delete,
    code_summary_get, code_summary_upsert, delete_file_symbols, invalidate_memory, mark_code_summaries_stale,
    move_file_symbols, replace_code_chunks, replace_file_symbols, resolve_edges, search_code_chunks,
    search_code_summaries, set_code_chunk_embedding, set_code_chunk_header, set_code_indexed_head,
    set_code_summary_embedding, set_code_summary_memory_id, stale_code_summaries, symbol_definitions,
    upsert_index_memory, CodeFileRow, CodeHistoryRow, CodeHit, CodeSummaryRow, EdgeRow, NewCodeChunk, NewEdge,
    NewSymbol, ScopeRow, SummaryHit, SymbolLookup, SymbolRow, CODE_HISTORY_SOURCE_PREFIX, INDEX_OWNED_SOURCE_PREFIX,
};

pub mod ask;
pub mod chunk;
pub mod git;
pub mod history;
pub mod job;
pub mod scope;
pub mod secrets;
pub mod summary;
