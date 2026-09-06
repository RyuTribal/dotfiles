
//! kb — personal vectorized knowledge-bank engine.
//!
//! Store: SQLite at ~/.local/share/mach/kb.db, one flat `memories` table
//! (see `store` for the schema). Search is brute-force cosine similarity
//! over embeddings decoded from the `embedding` BLOB column, computed in
//! Rust rather than pushed into SQLite.
//!
//! On vector search: `sqlite-vec` was tried first, as asked. It compiles
//! cleanly against rusqlite's bundled sqlite3 and works at runtime — loading
//! it via `sqlite3_auto_extension` and querying a `vec0` virtual table both
//! checked out in a throwaway probe crate. It was not carried into this
//! engine because the schema this phase specifies is a single flat table
//! with `embedding BLOB` as a column, not a separate vector index table,
//! and because brute-force cosine is trivial to unit test in pure Rust with
//! a fake embedder (no live ollama, no virtual table, no extension loading
//! in test builds). At personal scale (comfortably under 50k rows) a linear
//! scan over 768-dimensional vectors costs low single-digit milliseconds,
//! so there is no real performance argument for the extension yet — if the
//! corpus ever outgrows that, revisiting `sqlite-vec` (now verified viable)
//! is the obvious next step.
//!
//! Embeddings come from Ollama's HTTP API (`nomic-embed-text`, 768 dims).
//! `embed` wraps that call behind an `Embedder` trait so store/search logic
//! can be tested with a fake embedder that never touches the network.
pub mod cli;
pub mod embed;
pub mod store;

pub use store::{KbError, Memory};
