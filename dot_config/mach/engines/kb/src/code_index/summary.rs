//! Code index: bottom-up module and repo summaries (see
//! docs/superpowers/specs/2026-09-23-code-index-design.md, "Nightly job",
//! phase 2's module/repo stage).
//!
//! Called once per project from `job.rs::index_project`, right after that
//! project's own file-level summary pass (`job::run_summary_pass`) --
//! sharing the very same [`Budget`] those file-level calls (and the header
//! calls before them) spend against, so a module/repo call is exactly as
//! "one more `claude -p` call this run" as any other.
//!
//! A *module* is every directory at depth 1 or depth 2 (from the project
//! root) that currently has at least one `indexed`/`fallback` file
//! somewhere under it; the project root itself, `"/"`, is always a
//! candidate module (a *repo* summary) as long as the project has any
//! indexed file at all. This module set is recomputed fresh from
//! `code_files` on every run -- nothing about it is persisted -- so a
//! directory that stops containing indexed files (every file under it
//! deleted, recategorized out of scope, or secret-swept) simply drops out
//! of it next run; see [`cleanup_orphaned_modules`].
//!
//! For every module/repo whose `code_summaries` row is stale or entirely
//! missing, deepest first (depth 2, then depth 1, then `/`), this
//! regenerates the summary from its children's *current* summaries once
//! they are all fresh -- or, failing that, once the module has sat stale
//! for 7+ days, using whatever children currently have a summary at all
//! (see [`is_ready`]). "Children" differs by depth:
//!
//! - a depth-2 module's children are the file-level summaries of every
//!   file anywhere under it (recursively -- there is no depth-3 module,
//!   so a file three or more directories deep is still a direct child of
//!   its depth-2 ancestor);
//! - a depth-1 module's children are its depth-2 modules' summaries, plus
//!   the file-level summaries of files directly inside it (not under any
//!   subdirectory);
//! - the repo's children are every depth-1 module's summary plus every
//!   root-level file's summary (a mixed repo is summarised from both).
//!
//! Children with no text (placeholders, given-up files) are never gathered:
//! a module left with none makes no call and stays a cleared placeholder.
//! A `--path-prefix` run regenerates only modules under the prefix and
//! never the repo level. The mirrored memory carries only the summary's
//! lead plus a pointer; the full text stays in `code_summaries`.
//!
//! Processing depth 2 before depth 1 before `/` in the same run, against
//! live reads of `code_summaries`, is what makes the bottom-up dependency
//! work in one pass: a depth-1 module regenerated this run already sees
//! its depth-2 children's *new* text, not last run's.
//!
//! On a successful regeneration the summary is written to `code_summaries`
//! (`source_digest` = sha256 of its children's own `path:digest` lines,
//! sorted -- stable no matter what order the children were gathered in,
//! and exactly what makes a later child change mark this module stale
//! again the same way a file change does), embedded (failure leaves the
//! embedding NULL for the existing `code_summaries_missing_embedding`
//! backfill to close later -- no new backfill needed, it already scans
//! every level), and mirrored into `memories` as an index-owned row via
//! `store::upsert_index_memory`, whose id is then recorded back onto the
//! summary row (`store::set_code_summary_memory_id`).
//!
//! Why nothing here needs to re-stale a module's own parent after
//! regenerating it: `job.rs`'s `ancestor_summary_paths` already marks
//! *every* ancestor level (depth 1, depth 2, and `/`) stale together, in
//! one call, the moment a leaf file changes -- so by the time this stage
//! runs, a depth-1 module whose depth-2 child just changed is already
//! independently on the stale-or-missing candidate list, not waiting on
//! this stage to propagate anything upward.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::code_index::job::{is_tool_transcript, Budget, SUMMARY_GIVE_UP_ATTEMPTS, TIMEOUT_INDEX_SONNET};
use crate::embed::Embedder;
use crate::reflect::{LoggedLlm, ReflectLlm};
use crate::store::{self, CodeSummaryRow, KbError, INDEX_OWNED_SOURCE_PREFIX};

/// Combined child-summary section of a module/repo prompt is capped at this
/// many characters, dropping the longest child first (repeatedly) until it
/// fits -- see [`build_module_prompt`].
const MODULE_PROMPT_CHILD_CHARS: usize = 30_000;

/// A stale-or-missing module regenerates anyway once it has sat stale this
/// many days, even with some children still not fresh -- otherwise a
/// permanently-stuck child (e.g. a file whose summary call keeps failing)
/// would starve its ancestors of ever regenerating at all.
const STALE_OVERRIDE_DAYS: f64 = 7.0;

/// Outcome of one project's [`run_module_summary_pass`].
pub(crate) struct ModulePassResult {
    /// Module/repo (`sonnet`) calls actually attempted this run (success or
    /// failure -- each spends one unit of `budget`), same accounting as
    /// `job::ProjectOutcome::headers_attempted`.
    pub attempted: usize,
    /// Module/repo `code_summaries` rows deleted this run because their
    /// directory no longer has any indexed file (see
    /// [`cleanup_orphaned_modules`]). Free -- no LLM call, not counted
    /// against `budget`.
    pub pruned: usize,
}

/// One child of a module/repo being regenerated: either a file (a
/// `code_summaries` row with `level = "file"`) or a nested module (`level =
/// "module"`). `is_file` picks the right freshness check in
/// [`child_fresh`] -- a file child is fresh when its summary's
/// `source_digest` still matches the file's current blob (its own `stale`
/// bit is never meaningfully set -- see `job::run_summary_pass`, which
/// tracks a file's own freshness by digest, not by staling it); a module
/// child is fresh when its `stale` bit is clear.
#[derive(Debug, Clone)]
struct ChildRef {
    path: String,
    is_file: bool,
}

/// One child's summary, read once ready to be gathered into a prompt and a
/// digest -- see [`gather_children`].
struct ChildSummary {
    path: String,
    digest: String,
    text: String,
}

/// A file's current blob and give-up state, for the file-child freshness
/// check (`child_fresh`) -- `summary_attempts` is the same
/// per-current-blob counter `job::run_summary_pass` maintains (see
/// `job::SUMMARY_GIVE_UP_ATTEMPTS`).
struct FileMeta {
    blob: String,
    summary_attempts: i64,
}

/// The project's current module shape, derived fresh from `code_files`
/// every run (nothing here is persisted) -- see the module doc comment for
/// what counts as a module.
struct FileTree {
    depth1_dirs: BTreeSet<String>,
    depth2_dirs: BTreeSet<String>,
    /// depth-2 dir -> every indexed/fallback file anywhere under it.
    depth2_children: BTreeMap<String, Vec<ChildRef>>,
    /// depth-1 dir -> its depth-2 modules, plus files directly inside it.
    depth1_children: BTreeMap<String, Vec<ChildRef>>,
    /// Every indexed/fallback file directly at the project root (zero
    /// directory segments) -- normally not any module's child (see
    /// `build_file_tree`'s own doc comment), but the repo's own children
    /// when there are no depth-1 directories at all (fix-list item 5: a
    /// repo whose files are all at the root is summarised from them
    /// directly, rather than from an empty module list).
    root_files: Vec<ChildRef>,
    /// Current blob + give-up state of every indexed/fallback file.
    files: HashMap<String, FileMeta>,
    /// Whether the project has any indexed/fallback file at all -- when
    /// false, `"/"` is not a candidate module and an existing repo summary
    /// is orphaned.
    has_any_file: bool,
}

/// Runs the module/repo summary stage for one project: prunes orphaned
/// module/repo summaries first (free, no LLM call), then regenerates every
/// stale-or-missing module/repo whose children are ready, deepest first.
/// Stops the moment `budget` is spent -- whatever is left stale or missing
/// is picked up again next run, same as every other pass in `job.rs`.
pub(crate) fn run_module_summary_pass<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    summary_llm: &LoggedLlm<L>,
    embedder: &E,
    project_id: i64,
    project_name: &str,
    budget: &mut Budget,
    path_prefix: Option<&str>,
    now: &str,
) -> Result<ModulePassResult, KbError> {
    let tree = build_file_tree(conn, project_id)?;
    let pruned = cleanup_orphaned_modules(conn, project_id, project_name, &tree, now)?;

    let mut attempted = 0usize;
    // A `--path-prefix` run (final-review item 10) regenerates only the
    // modules under the prefix and never the repo `/` level: the rest of
    // the project wasn't looked at this run, so its summaries (and the
    // repo summary built from them) aren't this run's to rewrite.
    let in_scope = |dir: &str| path_prefix.is_none_or(|p| dir_under_prefix(dir, p));

    for dir in tree.depth2_dirs.iter().filter(|d| in_scope(d)) {
        if !budget.remaining() {
            return Ok(ModulePassResult { attempted, pruned });
        }
        let children = tree.depth2_children.get(dir).cloned().unwrap_or_default();
        if try_regenerate_one(conn, summary_llm, embedder, project_name, project_id, dir, "module", &children, &tree.files, budget, now)? {
            attempted += 1;
        }
    }

    for dir in tree.depth1_dirs.iter().filter(|d| in_scope(d)) {
        if !budget.remaining() {
            return Ok(ModulePassResult { attempted, pruned });
        }
        let children = tree.depth1_children.get(dir).cloned().unwrap_or_default();
        if try_regenerate_one(conn, summary_llm, embedder, project_name, project_id, dir, "module", &children, &tree.files, budget, now)? {
            attempted += 1;
        }
    }

    if tree.has_any_file && path_prefix.is_none() && budget.remaining() {
        // The repo's children are ALWAYS its root-level files plus its
        // depth-1 modules (final-review item 4): a mixed repo's root files
        // (build scripts, `main.rs`, a top-level README) are part of its
        // architecture too, and a change to one of them must change the
        // repo's digest. A repo with no subdirectories is just the case
        // where the module half is empty.
        let mut children: Vec<ChildRef> = tree.root_files.clone();
        children.extend(tree.depth1_dirs.iter().map(|d| ChildRef { path: d.clone(), is_file: false }));
        if try_regenerate_one(conn, summary_llm, embedder, project_name, project_id, "/", "repo", &children, &tree.files, budget, now)? {
            attempted += 1;
        }
    }

    Ok(ModulePassResult { attempted, pruned })
}

/// Whether module `dir` (`"a/"`, `"a/b/"`) lies under `prefix` (`"a"`,
/// `"a/"`, `"a/b"`) -- same trailing-slash-insensitive, whole-segment rule
/// as `job::prefix_matcher`.
fn dir_under_prefix(dir: &str, prefix: &str) -> bool {
    let d = dir.trim_end_matches('/');
    let p = prefix.trim_end_matches('/');
    p.is_empty() || d == p || (d.len() > p.len() && d.starts_with(p) && d.as_bytes()[p.len()] == b'/')
}

/// Every indexed/fallback file's path, grouped into the depth-1/depth-2
/// module shape described in the module doc comment. A file with fewer
/// than two directory segments (`"x.rs"`, root-level) contributes to
/// `has_any_file` and `root_files` only -- it has no depth-1/depth-2
/// ancestor and is not counted as any module's child; it becomes the
/// repo's own children only when there are no depth-1 directories at all
/// (see `FileTree::root_files`, fix-list item 5).
fn build_file_tree(conn: &Connection, project_id: i64) -> Result<FileTree, KbError> {
    let mut rows = store::code_files_with_status(conn, project_id, "indexed")?;
    rows.extend(store::code_files_with_status(conn, project_id, "fallback")?);

    let mut depth1_dirs = BTreeSet::new();
    let mut depth2_dirs = BTreeSet::new();
    let mut depth2_children: BTreeMap<String, Vec<ChildRef>> = BTreeMap::new();
    let mut depth1_children: BTreeMap<String, Vec<ChildRef>> = BTreeMap::new();
    let mut root_files: Vec<ChildRef> = Vec::new();
    let mut files: HashMap<String, FileMeta> = HashMap::new();

    for f in &rows {
        files.insert(f.path.clone(), FileMeta { blob: f.blob.clone(), summary_attempts: f.summary_attempts });
        let segments: Vec<&str> = f.path.split('/').collect();
        let dirs = &segments[..segments.len().saturating_sub(1)];
        if dirs.is_empty() {
            root_files.push(ChildRef { path: f.path.clone(), is_file: true });
            continue;
        }
        let d1 = format!("{}/", dirs[0]);
        depth1_dirs.insert(d1.clone());
        if dirs.len() >= 2 {
            let d2 = format!("{}/{}/", dirs[0], dirs[1]);
            depth2_dirs.insert(d2.clone());
            depth2_children.entry(d2).or_default().push(ChildRef { path: f.path.clone(), is_file: true });
        } else {
            depth1_children.entry(d1).or_default().push(ChildRef { path: f.path.clone(), is_file: true });
        }
    }
    for d2 in &depth2_dirs {
        let d1 = d2.split('/').next().map(|s| format!("{}/", s)).unwrap_or_default();
        depth1_children.entry(d1).or_default().push(ChildRef { path: d2.clone(), is_file: false });
    }

    Ok(FileTree { depth1_dirs, depth2_dirs, depth2_children, depth1_children, root_files, files, has_any_file: !rows.is_empty() })
}

/// Deletes every `level IN ('module', 'repo')` `code_summaries` row whose
/// path is no longer live in `tree` (its directory has zero indexed files
/// now, or -- for `"/"` -- the project has zero indexed files anywhere),
/// tombstoning its mirrored `memories` row first, if it has one, via
/// `store::invalidate_memory` (no successor -- the module is gone, not
/// replaced). Runs before any regeneration this run, unconditionally (not
/// budget-gated: no LLM call). Returns how many were pruned.
fn cleanup_orphaned_modules(conn: &Connection, project_id: i64, project_name: &str, tree: &FileTree, now: &str) -> Result<usize, KbError> {
    let mut pruned = 0usize;
    for (path, level, memory_id) in existing_module_and_repo_summaries(conn, project_id)? {
        let still_live = if level == "repo" { tree.has_any_file } else { tree.depth1_dirs.contains(&path) || tree.depth2_dirs.contains(&path) };
        if still_live {
            continue;
        }
        // By source, not just the recorded `memory_id`: a restored older
        // generation of the mirror must go too (final-review item 5).
        store::invalidate_index_memories_by_source(conn, &index_source(project_name, &path), now)?;
        if let Some(mid) = memory_id {
            store::invalidate_memory(conn, mid, now)?;
        }
        store::code_summary_delete(conn, project_id, &path)?;
        pruned += 1;
    }
    Ok(pruned)
}

/// `(path, level, memory_id)` of every module/repo-level `code_summaries`
/// row of a project -- not part of `store.rs`'s public surface (no
/// existing function filters by `level`), same rationale as `job.rs`'s own
/// small private SQL helpers: narrow, single-caller, crate-internal.
fn existing_module_and_repo_summaries(conn: &Connection, project_id: i64) -> Result<Vec<(String, String, Option<i64>)>, KbError> {
    let mut stmt = conn.prepare("SELECT path, level, memory_id FROM code_summaries WHERE project_id = ?1 AND level IN ('module', 'repo')")?;
    let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?)))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Whether `child` currently has a fresh summary, or -- for a file child
/// only -- has given up trying to get one and so counts as "done" instead
/// (fix-list item 4): a file child is fresh when its `code_summaries` row's
/// `source_digest` still equals the file's current blob (a file's own row
/// is never marked `stale` -- see the module doc comment), and if it has
/// no such row it still counts as ready once `summary_attempts` has
/// reached [`SUMMARY_GIVE_UP_ATTEMPTS`] for the current blob -- it will
/// never produce a summary for this blob, so it must not block its
/// module/repo ancestor from ever becoming ready. A module child is fresh
/// when its row exists and `stale` is false; a missing module-child row is
/// never fresh (modules have no give-up mechanism of their own -- see
/// `try_regenerate_one`'s placeholder seeding for how a module without a
/// row still becomes eligible for the 7-day override instead).
fn child_fresh(conn: &Connection, project_id: i64, child: &ChildRef, files: &HashMap<String, FileMeta>) -> Result<bool, KbError> {
    let row = store::code_summary_get(conn, project_id, &child.path)?;
    if child.is_file {
        if let Some(row) = &row {
            if files.get(&child.path).is_some_and(|m| m.blob == row.source_digest) {
                return Ok(true);
            }
        }
        return Ok(files.get(&child.path).is_some_and(|m| m.summary_attempts >= SUMMARY_GIVE_UP_ATTEMPTS));
    }
    Ok(row.is_some_and(|r| !r.stale))
}

/// Whether a stale-or-missing module/repo is ready to regenerate right
/// now: every child fresh (see [`child_fresh`]), or -- only when it
/// already has a row with a `stale_since` -- that timestamp is at least
/// [`STALE_OVERRIDE_DAYS`] old. A module that has never had a real row yet
/// still gets a `stale_since` to measure this against, via
/// `try_regenerate_one`'s placeholder seeding (fix-list item 4) -- see
/// there for why a genuinely missing row would otherwise never become
/// eligible for this override at all.
fn is_ready(conn: &Connection, project_id: i64, children: &[ChildRef], files: &HashMap<String, FileMeta>, existing: Option<&CodeSummaryRow>, now: &str) -> Result<bool, KbError> {
    let mut all_fresh = true;
    for child in children {
        if !child_fresh(conn, project_id, child, files)? {
            all_fresh = false;
            break;
        }
    }
    if all_fresh {
        return Ok(true);
    }
    if let Some(row) = existing {
        if let Some(since) = row.stale_since.as_deref() {
            if store::age_days(since, now) >= STALE_OVERRIDE_DAYS {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Reads whichever of `children` currently have a `code_summaries` row
/// (all of them, when readiness came from "every child fresh"; only some,
/// when it came from the 7-day override -- "use whatever children exist"),
/// path-ascending.
fn gather_children(conn: &Connection, project_id: i64, children: &[ChildRef]) -> Result<Vec<ChildSummary>, KbError> {
    let mut out = Vec::new();
    for child in children {
        if let Some(row) = store::code_summary_get(conn, project_id, &child.path)? {
            // A placeholder (`text = ''`) contributes nothing -- to the
            // prompt or to the digest (final-review items 2/3).
            if row.text.trim().is_empty() {
                continue;
            }
            out.push(ChildSummary { path: child.path.clone(), digest: row.source_digest, text: row.text });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// sha256 (lowercase hex) of `children`'s own `path:digest` lines, sorted
/// path-ascending before hashing -- so this is the same digest no matter
/// what order `children` was built in, and changes whenever any child's
/// own digest changes (a file's blob, or a nested module's own digest).
fn children_digest(children: &[ChildSummary]) -> String {
    let mut lines: Vec<String> = children.iter().map(|c| format!("{}:{}", c.path, c.digest)).collect();
    lines.sort();
    let mut hasher = Sha256::new();
    hasher.update(lines.join("\n").as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

/// "Project: <project>\nModule: <dir>\n\n<child blocks>\n<instruction>".
/// Each child is rendered as `"<path>:\n<text>\n"`, path-ascending; if the
/// combined child section would exceed [`MODULE_PROMPT_CHILD_CHARS`], the
/// single longest child block is dropped and the total re-measured,
/// repeating until it fits (or nothing is left) -- so when something has
/// to give, it's always the longest child that goes first, not whichever
/// happened to sort last. `is_repo` picks the instruction's word cap and
/// subject (250 words / module purpose+components+flows+extension
/// points+gotchas, vs. 350 words / whole-project architecture overview).
fn build_module_prompt(project: &str, dir: &str, is_repo: bool, children: &[ChildSummary]) -> String {
    let mut entries: Vec<(String, String)> = children.iter().map(|c| (c.path.clone(), format!("{}:\n{}\n", c.path, c.text))).collect();
    loop {
        let total: usize = entries.iter().map(|(_, block)| block.chars().count()).sum();
        if total <= MODULE_PROMPT_CHILD_CHARS || entries.is_empty() {
            break;
        }
        let longest = entries.iter().enumerate().max_by_key(|(_, (_, block))| block.chars().count()).map(|(i, _)| i).expect("entries is non-empty");
        entries.remove(longest);
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut s = String::new();
    s.push_str(&format!("Project: {}\nModule: {}\n\n", project, dir));
    for (_, block) in &entries {
        s.push_str(block);
        s.push('\n');
    }
    // Spelled out because the model otherwise tries to "verify" a short
    // child summary by narrating file-reading tool calls it doesn't have.
    s.push_str("You have no tools and cannot read files. Answer only from the summaries above.\n");
    if is_repo {
        s.push_str("Reply with at most 350 words: an architecture overview of the whole project.\n");
    } else {
        s.push_str(
            "Reply with at most 250 words: the purpose of this module, its main components and how they interact, key flows, extension points, and gotchas.\n",
        );
    }
    s
}

/// One module/repo's regeneration attempt. Returns `false` with no side
/// effect at all when `dir` isn't currently a stale-or-missing candidate,
/// or is but isn't ready yet (per [`is_ready`]) -- no budget spent either
/// way, since nothing was attempted. Once ready, spends one unit of
/// `budget` right before the `sonnet` call (same accounting as
/// `job::run_summary_pass`: attempted, and budget spent, regardless of
/// whether the call itself succeeds). A call failure, or a reply that
/// trims to nothing, leaves the row exactly as it was (stale or missing),
/// so it's retried next run -- same "no half-finished write" contract as
/// every other LLM pass in this feature. On success: writes the summary
/// (`source_digest` from [`children_digest`], computed over the FULL
/// child set gathered, before any prompt truncation -- truncation only
/// affects what the model sees, never what the digest tracks), embeds it
/// (NULL on embedder failure, closed later by the existing
/// `code_summaries_missing_embedding` backfill -- it already scans every
/// summary level, not just `"file"`), mirrors it into `memories` via
/// `store::upsert_index_memory`, and records that memory's id back onto
/// the summary row.
#[allow(clippy::too_many_arguments)]
fn try_regenerate_one<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    summary_llm: &LoggedLlm<L>,
    embedder: &E,
    project_name: &str,
    project_id: i64,
    dir: &str,
    level: &str,
    children_refs: &[ChildRef],
    files: &HashMap<String, FileMeta>,
    budget: &mut Budget,
    now: &str,
) -> Result<bool, KbError> {
    let existing = store::code_summary_get(conn, project_id, dir)?;
    let is_candidate = existing.as_ref().map(|row| row.stale).unwrap_or(true);
    if !is_candidate {
        return Ok(false);
    }
    if !is_ready(conn, project_id, children_refs, files, existing.as_ref(), now)? {
        if existing.is_none() {
            // First time this module/repo has ever been a candidate: seed
            // a placeholder row recording `now` as its `stale_since`, so a
            // module that never gets a real row (e.g. every child gave up
            // on its own summary) still becomes eligible for the 7-day
            // stale override on a later run, instead of being stuck
            // forever with nothing to measure that override against
            // (fix-list item 4).
            store::code_summary_seed_placeholder(conn, project_id, dir, level, now)?;
        }
        return Ok(false);
    }

    let children = gather_children(conn, project_id, children_refs)?;
    let digest = children_digest(&children);
    if children.is_empty() {
        // Every child is done but none has any text (all gave up, or are
        // placeholders themselves): nothing to summarise from, so no call
        // (final-review item 2). Leave/turn the row into a cleared
        // placeholder -- not stale, so it isn't re-checked every run; any
        // later child change re-stales it via `ancestor_summary_paths`.
        // A real row whose children all emptied out no longer describes
        // anything current, so it is cleared and its mirror invalidated.
        let had_text = existing.as_ref().is_some_and(|r| !r.text.is_empty());
        if had_text {
            if let Some(mid) = store::code_summary_placeholderize(conn, project_id, dir, now)? {
                store::invalidate_memory(conn, mid, now)?;
            }
            store::invalidate_index_memories_by_source(conn, &index_source(project_name, dir), now)?;
        }
        if existing.is_none() {
            store::code_summary_seed_placeholder(conn, project_id, dir, level, now)?;
        }
        store::code_summary_clear_stale(conn, project_id, dir, &digest)?;
        return Ok(false);
    }
    if let Some(row) = &existing {
        if row.source_digest == digest && !row.text.is_empty() {
            // Staled, but nothing it was built from actually changed:
            // clear stale, no call, no new mirror generation.
            store::code_summary_clear_stale(conn, project_id, dir, &digest)?;
            return Ok(false);
        }
    }
    let prompt = build_module_prompt(project_name, dir, level == "repo", &children);

    let result = summary_llm.call("sonnet", &prompt, TIMEOUT_INDEX_SONNET);
    budget.record(result.as_ref().err().map(String::as_str));
    let Ok(reply) = result else {
        return Ok(true);
    };
    let text = reply.trim();
    if text.is_empty() || is_tool_transcript(text) {
        return Ok(true);
    }

    store::code_summary_upsert(conn, project_id, dir, level, text, &digest, now)?;
    let embedding = embedder.embed(text).ok();
    if let Some(v) = &embedding {
        store::set_code_summary_embedding(conn, project_id, dir, v)?;
    }

    let source = index_source(project_name, dir);
    let content = mirror_content(project_name, dir, level, text);
    // The mirror is embedded from its own (short) content, not the full
    // summary text, so recall ranks what it will actually show.
    let mirror_embedding = embedder.embed(&content).ok();
    store::upsert_index_memory_for_summary(conn, project_id, dir, project_name, &source, &content, mirror_embedding.as_deref(), now)?;
    Ok(true)
}

/// `code-index:<project>:<dir>` -- the exact source of a module/repo mirror.
fn index_source(project_name: &str, dir: &str) -> String {
    format!("{}{}:{}", INDEX_OWNED_SOURCE_PREFIX, project_name, dir)
}

/// Maximum characters of a summary's lead mirrored into `memories`.
const MIRROR_LEAD_CHARS: usize = 600;

/// The memory mirror's content (final-review item 8): only the summary's
/// lead ([`lead_of`]) plus a pointer to the full text, which stays in
/// `code_summaries` (reached via `mach kb ask`'s `overview` tool). Keeps a
/// 250-350-word summary from flooding plain recall.
fn mirror_content(project_name: &str, dir: &str, level: &str, text: &str) -> String {
    let lead = lead_of(text);
    let head = if level == "repo" { format!("{} architecture: ", project_name) } else { format!("{} {}: ", project_name, dir) };
    format!("{}{} (full: mach kb ask --project {} … overview {})", head, lead, project_name, dir)
}

/// First paragraph of `text` (up to the first blank line), capped at
/// [`MIRROR_LEAD_CHARS`]: when longer, cut after the last sentence end
/// (`.`, `!`, `?` followed by whitespace or the end) inside the cap; with
/// no sentence end there, cut at the last whitespace and append `…`.
///
/// `pub(crate)`, not private, so `code_index::history`'s own memory mirror
/// (a different content shape -- `"<project> <YYYY-MM>: "` instead of this
/// module's `mirror_content` header) reuses this exact truncation instead
/// of duplicating it.
pub(crate) fn lead_of(text: &str) -> String {
    let trimmed = text.trim();
    let para = trimmed.split("\n\n").next().unwrap_or("").trim();
    lead_capped(para)
}

/// Like [`lead_of`] but across paragraphs: the whole text, whitespace
/// collapsed, cut at the last sentence end within [`MIRROR_LEAD_CHARS`].
/// The history mirror uses this -- a month summary's first paragraph can be
/// a one-line heading, which made the mirrored memory near-empty.
pub(crate) fn lead_across_paragraphs(text: &str) -> String {
    lead_capped(text.trim())
}

fn lead_capped(para: &str) -> String {
    let para: String = para.split_whitespace().collect::<Vec<_>>().join(" ");
    if para.chars().count() <= MIRROR_LEAD_CHARS {
        return para;
    }
    let capped: String = para.chars().take(MIRROR_LEAD_CHARS).collect();
    let bytes = capped.as_bytes();
    let mut cut: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'!' | b'?') && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            cut = Some(i + 1);
        }
    }
    if let Some(c) = cut {
        return capped[..c].to_string();
    }
    let budget: String = para.chars().take(MIRROR_LEAD_CHARS - 1).collect();
    let end = budget.rfind(' ').unwrap_or(budget.len());
    format!("{}…", budget[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet as StdHashSet;
    use std::time::Duration;

    use crate::store::ProjectRow;

    fn mem_conn() -> Connection {
        store::open_with_path(std::path::Path::new(":memory:")).expect("open in-memory store")
    }

    fn new_project(conn: &Connection, name: &str) -> ProjectRow {
        store::upsert_project(conn, &format!("fp-{name}"), name, "/nonexistent", "2026-09-24T00:00:00Z").unwrap();
        store::get_project_by_name(conn, name).unwrap().unwrap()
    }

    /// Seeds a `code_files` row directly -- this stage never reads git or
    /// chunks, only `code_files`/`code_summaries`, so tests skip the git
    /// fixture machinery `job.rs`'s own tests need.
    fn seed_file(conn: &Connection, project_id: i64, path: &str, blob: &str, status: &str) {
        store::code_file_upsert(conn, project_id, path, blob, Some("Rust"), 1, status, "2026-09-24T00:00:00Z").unwrap();
    }

    fn seed_file_summary(conn: &Connection, project_id: i64, path: &str, blob: &str, text: &str) {
        store::code_summary_upsert(conn, project_id, path, "file", text, blob, "2026-09-24T00:00:00Z").unwrap();
    }

    /// Records every `Module:` path it was called for, in call order, so
    /// tests can assert both call count and (for the deepest-first test)
    /// order. Always succeeds with a distinct, non-empty reply unless the
    /// dir is listed in `fail`.
    #[derive(Default)]
    struct FakeLlm {
        calls: RefCell<Vec<String>>,
        prompts: RefCell<Vec<String>>,
        fail: RefCell<StdHashSet<String>>,
        /// When set, every reply is this text instead of "summary for <dir>".
        reply: RefCell<Option<String>>,
    }

    impl ReflectLlm for FakeLlm {
        fn call(&self, _model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            let dir = prompt.lines().find_map(|l| l.strip_prefix("Module: ")).unwrap_or("").to_string();
            self.calls.borrow_mut().push(dir.clone());
            self.prompts.borrow_mut().push(prompt.to_string());
            if self.fail.borrow().contains(&dir) {
                return Err("simulated failure".to_string());
            }
            if let Some(r) = self.reply.borrow().as_ref() {
                return Ok(r.clone());
            }
            Ok(format!("summary for {}", dir))
        }
    }

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().take(4).map(|b| b as f32).collect())
        }
    }

    // -- pure helpers ------------------------------------------------------

    #[test]
    fn children_digest_is_stable_under_reordering_but_changes_with_content() {
        let a = vec![
            ChildSummary { path: "a/".to_string(), digest: "d1".to_string(), text: "t1".to_string() },
            ChildSummary { path: "b/".to_string(), digest: "d2".to_string(), text: "t2".to_string() },
        ];
        let b = vec![
            ChildSummary { path: "b/".to_string(), digest: "d2".to_string(), text: "t2".to_string() },
            ChildSummary { path: "a/".to_string(), digest: "d1".to_string(), text: "t1".to_string() },
        ];
        assert_eq!(children_digest(&a), children_digest(&b), "sorted before hashing, so order never matters");

        let c = vec![
            ChildSummary { path: "a/".to_string(), digest: "different".to_string(), text: "t1".to_string() },
            ChildSummary { path: "b/".to_string(), digest: "d2".to_string(), text: "t2".to_string() },
        ];
        assert_ne!(children_digest(&a), children_digest(&c), "a changed child digest must change the module's own digest");
    }

    #[test]
    fn build_module_prompt_drops_the_longest_child_first_when_over_the_char_cap() {
        // "zdir/" (12,000), "mdir/" (11,000), "adir/" (10,000) -- combined
        // ~33,000 exceeds the 30,000 cap by dropping just the longest one
        // (zdir/, ~12,000), leaving ~21,000: it must be the one dropped,
        // not whichever sorts last alphabetically or was inserted last.
        let children = vec![
            ChildSummary { path: "zdir/".to_string(), digest: "d".to_string(), text: "z".repeat(12_000) },
            ChildSummary { path: "mdir/".to_string(), digest: "d".to_string(), text: "m".repeat(11_000) },
            ChildSummary { path: "adir/".to_string(), digest: "d".to_string(), text: "a".repeat(10_000) },
        ];
        let prompt = build_module_prompt("proj", "/", true, &children);
        assert!(!prompt.contains("zdir/:\n"), "the longest child must be dropped first");
        assert!(prompt.contains("mdir/:\n"));
        assert!(prompt.contains("adir/:\n"));
    }

    #[test]
    fn build_module_prompt_keeps_everything_under_the_cap() {
        let children = vec![ChildSummary { path: "a/".to_string(), digest: "d".to_string(), text: "short summary".to_string() }];
        let prompt = build_module_prompt("proj", "a/", false, &children);
        assert!(prompt.contains("Project: proj\nModule: a/\n\n"));
        assert!(prompt.contains("a/:\nshort summary\n"));
        assert!(prompt.contains("250 words"), "a module prompt asks for the 250-word instruction");
    }

    #[test]
    fn build_module_prompt_for_the_repo_asks_for_the_350_word_architecture_overview() {
        let children = vec![ChildSummary { path: "a/".to_string(), digest: "d".to_string(), text: "module a summary".to_string() }];
        let prompt = build_module_prompt("proj", "/", true, &children);
        assert!(prompt.contains("Module: /"));
        assert!(prompt.contains("350 words"));
        assert!(prompt.contains("architecture overview"));
    }

    // -- module set derivation ----------------------------------------------

    #[test]
    fn the_module_set_is_depth_1_and_2_dirs_with_an_indexed_file_recursively_plus_the_repo_root() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";

        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");
        // Deep nesting: still a depth-2 "a/b/" child, no depth-3 module.
        seed_file(&conn, project.id, "a/b/c/z.rs", "b2", "indexed");
        seed_file_summary(&conn, project.id, "a/b/c/z.rs", "b2", "z summary");
        // A skipped file must not make "skip/" a module.
        seed_file(&conn, project.id, "skip/y.rs", "b3", "skipped");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        assert!(store::code_summary_get(&conn, project.id, "a/").unwrap().is_some(), "a/ has an indexed file directly in it");
        assert!(store::code_summary_get(&conn, project.id, "a/b/").unwrap().is_some(), "a/b/ has an indexed file recursively under it");
        assert!(store::code_summary_get(&conn, project.id, "a/b/c/").unwrap().is_none(), "depth-3 is never a module");
        assert!(store::code_summary_get(&conn, project.id, "skip/").unwrap().is_none(), "a skipped file's directory is never a module");
        assert!(store::code_summary_get(&conn, project.id, "/").unwrap().is_some(), "the repo root is always a candidate once anything is indexed");
    }

    #[test]
    fn processes_modules_deepest_first_depth_2_then_depth_1_then_the_repo() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        for (path, blob) in [("a/b/x.rs", "b1"), ("a/y.rs", "b2")] {
            seed_file(&conn, project.id, path, blob, "indexed");
            seed_file_summary(&conn, project.id, path, blob, &format!("summary for {}", path));
        }

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        assert_eq!(llm.calls.borrow().as_slice(), &["a/b/".to_string(), "a/".to_string(), "/".to_string()], "depth 2, then depth 1, then the repo root, in that order");
    }

    // -- readiness rule -------------------------------------------------------

    #[test]
    fn a_module_with_an_unready_child_and_no_existing_row_seeds_a_placeholder_recording_first_seen() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";

        seed_file(&conn, project.id, "a/x.rs", "blob-x", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob-x", "x summary");
        // "a/y.rs" is indexed but its blob moved on since its last summary
        // -- not fresh.
        seed_file(&conn, project.id, "a/y.rs", "blob-y-new", "indexed");
        seed_file_summary(&conn, project.id, "a/y.rs", "blob-y-old", "y summary (stale)");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        // Fix-list item 4: a module that has never had a real row still
        // gets a placeholder recording `now` as `stale_since`, so a later
        // run can check the 7-day override against it -- it is no longer
        // left with no row at all.
        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(row.stale, "not every child is fresh yet");
        assert_eq!(row.text, "", "a placeholder, not a real summary");
        assert_eq!(row.stale_since.as_deref(), Some(now));
        assert!(llm.calls.borrow().is_empty(), "an unready, never-generated module makes no LLM call");
    }

    #[test]
    fn a_stale_module_regenerates_once_it_has_sat_stale_seven_or_more_days_even_with_an_unready_child() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";

        seed_file(&conn, project.id, "a/x.rs", "blob-x", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob-x", "x summary");
        seed_file(&conn, project.id, "a/y.rs", "blob-y-new", "indexed");
        seed_file_summary(&conn, project.id, "a/y.rs", "blob-y-old", "y summary (stale)");

        // An existing "a/" row, stale since 23 days ago -- well past the
        // 7-day override -- even though "a/y.rs" is STILL not fresh.
        store::code_summary_upsert(&conn, project.id, "a/", "module", "old a/ summary", "old-digest", "2026-09-01T00:00:00Z").unwrap();
        store::mark_code_summaries_stale(&conn, project.id, &["a/".to_string()], "2026-09-01T00:00:00Z").unwrap();

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(!row.stale, "the 7-day override regenerates using whatever children currently exist");
        assert_ne!(row.text, "old a/ summary");
    }

    #[test]
    fn a_new_module_with_one_given_up_child_becomes_ready() {
        // Fix-list item 4 (second half): a file that gave up on ever
        // getting its own summary (job::SUMMARY_GIVE_UP_ATTEMPTS reached
        // for its current blob) counts as "done" for its module's
        // readiness -- it must not block a brand-new module (no existing
        // row at all) from ever becoming ready.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";

        seed_file(&conn, project.id, "a/x.rs", "blob-x", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob-x", "x summary");
        // "a/y.rs" has no summary at all, and has exhausted its attempts
        // for the current blob -- it will never produce one.
        seed_file(&conn, project.id, "a/y.rs", "blob-y", "indexed");
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(!row.stale, "the given-up child no longer blocks readiness -- a/ regenerates from x.rs alone");
        assert!(llm.calls.borrow().contains(&"a/".to_string()));
    }

    #[test]
    fn a_never_summarised_module_regenerates_via_the_seven_day_override_once_its_placeholder_is_old_enough() {
        // Fix-list item 4 (first half): a module that has NEVER had a real
        // row still becomes eligible for the 7-day stale override, via the
        // placeholder `try_regenerate_one` seeds the first time it's an
        // unready candidate (`store::code_summary_seed_placeholder`).
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let day1 = "2026-09-01T00:00:00Z";

        seed_file(&conn, project.id, "a/x.rs", "blob-x", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob-x", "x summary");
        // "a/y.rs" is indexed but never got a summary, and hasn't given up
        // either (still below the give-up cap) -- genuinely not fresh,
        // not "done".
        seed_file(&conn, project.id, "a/y.rs", "blob-y", "indexed");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, day1).unwrap();

        let placeholder = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(placeholder.stale);
        assert_eq!(placeholder.text, "", "no real row yet, just the first-seen placeholder");
        assert_eq!(placeholder.stale_since.as_deref(), Some(day1));
        assert!(llm.calls.borrow().is_empty(), "not ready yet, so no call this run");

        // 8 days later, a/y.rs STILL has no summary -- but the
        // placeholder's stale_since is old enough for the 7-day override.
        let day9 = "2026-09-09T00:00:00Z";
        let mut budget2 = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget2, None, day9).unwrap();

        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(!row.stale, "the 7-day override regenerates even though a/y.rs never became fresh");
        assert_ne!(row.text, "", "a real summary now, not the placeholder");
        assert!(llm.calls.borrow().contains(&"a/".to_string()));
    }

    #[test]
    fn a_failed_module_call_leaves_no_row_so_it_is_retried_next_run() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");

        let llm = FakeLlm { fail: RefCell::new(["a/".to_string()].into_iter().collect()), ..Default::default() };
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        let result = run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        assert_eq!(result.attempted, 1, "the call was attempted, and spent budget, even though it failed");
        assert!(store::code_summary_get(&conn, project.id, "a/").unwrap().is_none(), "a failed call leaves no row at all");
    }

    // -- memory mirroring -----------------------------------------------------

    #[test]
    fn regenerating_a_module_mirrors_into_memories_and_supersedes_the_previous_mirror() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/x.rs", "blob1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob1", "file summary v1");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        let memory_id = row.memory_id.expect("a module summary must have a mirrored memory");
        let mem = store::get(&conn, memory_id).unwrap().unwrap();
        assert_eq!(mem.source.as_deref(), Some("code-index:proj:a/"));
        assert!(mem.content.starts_with("proj a/: "), "{}", mem.content);
        assert_eq!(mem.basis.as_deref(), Some(store::BASIS_DERIVED));
        assert!(mem.invalidated_at.is_none());

        // The file changes and is resummarised, staling "a/" again.
        seed_file(&conn, project.id, "a/x.rs", "blob2", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "blob2", "file summary v2");
        store::mark_code_summaries_stale(&conn, project.id, &["a/".to_string()], now).unwrap();

        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();
        let row2 = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        let memory_id2 = row2.memory_id.expect("still has a mirrored memory id");
        assert_ne!(memory_id, memory_id2, "regeneration mirrors into a NEW memory row");

        let old_mem = store::get(&conn, memory_id).unwrap().unwrap();
        assert!(old_mem.invalidated_at.is_some(), "the old mirror must be superseded");
        assert_eq!(old_mem.superseded_by, Some(memory_id2));

        let active_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE source = ?1 AND invalidated_at IS NULL", params!["code-index:proj:a/"], |r| r.get(0))
            .unwrap();
        assert_eq!(active_count, 1, "exactly one active mirror per source");
    }

    #[test]
    fn the_repo_summary_mirrors_with_an_architecture_content_prefix_and_the_root_source() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        let row = store::code_summary_get(&conn, project.id, "/").unwrap().unwrap();
        assert_eq!(row.level, "repo");
        let memory_id = row.memory_id.expect("the repo summary must have a mirrored memory");
        let mem = store::get(&conn, memory_id).unwrap().unwrap();
        assert_eq!(mem.source.as_deref(), Some("code-index:proj:/"));
        assert!(mem.content.starts_with("proj architecture: "), "{}", mem.content);
    }

    #[test]
    fn a_repo_with_no_depth_1_directories_is_summarised_from_its_root_files() {
        // Fix-list item 5: every file sits directly at the repo root, so
        // there are no depth-1 module summaries to build the repo from --
        // it must fall back to the root files' own summaries instead of
        // running with zero children.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "main.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "main.rs", "b1", "main summary");
        seed_file(&conn, project.id, "lib.rs", "b2", "indexed");
        seed_file_summary(&conn, project.id, "lib.rs", "b2", "lib summary");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        let row = store::code_summary_get(&conn, project.id, "/").unwrap();
        assert!(row.is_some(), "a repo with no depth-1 dirs must still be summarised, from its root files");
        assert!(!row.unwrap().stale);
        assert!(llm.calls.borrow().contains(&"/".to_string()));
    }

    #[test]
    fn a_repo_with_no_depth_1_dirs_waits_on_an_unready_root_file_instead_of_summarising_from_nothing() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "main.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "main.rs", "b1", "main summary");
        // "lib.rs" is indexed but its blob moved on since its last summary
        // -- not fresh.
        seed_file(&conn, project.id, "lib.rs", "b2-new", "indexed");
        seed_file_summary(&conn, project.id, "lib.rs", "b2-old", "lib summary (stale)");

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        // If the repo's children were (incorrectly) still the empty
        // depth-1 module list, this would be vacuously "ready" and
        // regenerate from nothing; with the root files as its real
        // children, it correctly waits on lib.rs instead.
        assert!(llm.calls.borrow().is_empty(), "an unready root file must block the repo summary, not be silently ignored");
        let row = store::code_summary_get(&conn, project.id, "/").unwrap().unwrap();
        assert!(row.stale, "seeded as a first-seen placeholder (fix-list item 4), not summarised from nothing");
    }

    // -- orphaned module cleanup ------------------------------------------------

    #[test]
    fn cleanup_prunes_a_module_and_the_repo_once_their_directories_have_no_indexed_files_left_and_tombstones_their_memories() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";

        // Leftover "a/" module summary + its mirrored memory, as if "a/"
        // once had indexed files -- and a leftover repo summary with no
        // mirror at all (exercises the "no memory_id to tombstone" path).
        store::code_summary_upsert(&conn, project.id, "a/", "module", "old a/ summary", "old-digest", now).unwrap();
        let memory_id = store::upsert_index_memory(&conn, &project.name, "code-index:proj:a/", "proj a/: old a/ summary", None, now).unwrap();
        store::set_code_summary_memory_id(&conn, project.id, "a/", memory_id).unwrap();
        store::code_summary_upsert(&conn, project.id, "/", "repo", "old repo summary", "old-digest", now).unwrap();

        // No code_files rows at all: the whole project has zero indexed
        // files, so both "a/" and "/" are orphaned.
        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(10);
        let result = run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();

        assert_eq!(result.pruned, 2);
        assert_eq!(result.attempted, 0, "pruning costs zero LLM calls");
        assert!(llm.calls.borrow().is_empty());
        assert!(store::code_summary_get(&conn, project.id, "a/").unwrap().is_none());
        assert!(store::code_summary_get(&conn, project.id, "/").unwrap().is_none());

        let mem = store::get(&conn, memory_id).unwrap().unwrap();
        assert!(mem.invalidated_at.is_some(), "its mirrored memory is tombstoned");
        assert!(mem.superseded_by.is_none(), "tombstoned WITHOUT a successor");
    }

    // -- budget ----------------------------------------------------------------

    #[test]
    fn budget_stops_cleanly_and_the_rest_resumes_on_the_next_call() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        for (path, blob) in [("a/x.rs", "b1"), ("b/y.rs", "b2")] {
            seed_file(&conn, project.id, path, blob, "indexed");
            seed_file_summary(&conn, project.id, path, blob, &format!("summary for {}", path));
        }

        let llm = FakeLlm::default();
        let summary_llm = LoggedLlm::new(&conn, "index_summary", &llm);
        let mut budget = Budget::new(1);
        let result = run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, None, now).unwrap();
        assert_eq!(result.attempted, 1, "budget of 1 allows exactly one module call this run");
        assert!(!budget.remaining());

        let mut budget2 = Budget::new(10);
        let result2 = run_module_summary_pass(&conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget2, None, now).unwrap();
        assert_eq!(result2.attempted, 2, "the other depth-1 module, then the repo, once both are fresh");
        assert!(store::code_summary_get(&conn, project.id, "/").unwrap().is_some());
    }

    // -- final-review fixes -----------------------------------------------

    fn run_pass(conn: &Connection, llm: &FakeLlm, project: &ProjectRow, prefix: Option<&str>, now: &str) -> ModulePassResult {
        let summary_llm = LoggedLlm::new(conn, "index_summary", llm);
        let mut budget = Budget::new(10);
        run_module_summary_pass(conn, &summary_llm, &FakeEmbedder, project.id, &project.name, &mut budget, prefix, now).unwrap()
    }

    #[test]
    fn a_module_whose_children_all_gave_up_makes_no_call_and_stays_a_cleared_placeholder() {
        // Final-review item 2: pre-fix, a module whose only child gave up
        // was "ready" and got a sonnet call built from zero children.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/y.rs", "blob-y", "indexed");
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();

        let llm = FakeLlm::default();
        let result = run_pass(&conn, &llm, &project, None, now);
        assert!(llm.calls.borrow().is_empty(), "no children with text -> no call at any level: {:?}", llm.calls.borrow());
        assert_eq!(result.attempted, 0);
        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert_eq!(row.text, "");
        assert!(!row.stale, "stale cleared so it is not re-checked every run");
        assert!(row.memory_id.is_none(), "a placeholder is never mirrored");
        let repo = store::code_summary_get(&conn, project.id, "/").unwrap().unwrap();
        assert_eq!(repo.text, "", "the repo's only child is a placeholder too");
        let mirrors: i64 = conn.query_row("SELECT COUNT(*) FROM memories WHERE source LIKE 'code-index:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(mirrors, 0);
    }

    #[test]
    fn an_unchanged_children_digest_clears_stale_without_a_call() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");
        let llm = FakeLlm::default();
        run_pass(&conn, &llm, &project, None, now);
        let before = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        let calls_before = llm.calls.borrow().len();

        // Staled with nothing actually changed underneath (e.g. a file
        // re-summarised to the same blob).
        store::mark_code_summaries_stale(&conn, project.id, &["a/".to_string(), "/".to_string()], now).unwrap();
        run_pass(&conn, &llm, &project, None, now);
        assert_eq!(llm.calls.borrow().len(), calls_before, "digest unchanged -> no call");
        let after = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert!(!after.stale);
        assert_eq!(after.text, before.text);
        assert_eq!(after.memory_id, before.memory_id, "no new mirror generation either");
        assert!(!store::code_summary_get(&conn, project.id, "/").unwrap().unwrap().stale);
    }

    #[test]
    fn placeholder_children_are_never_gathered_into_a_prompt() {
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        // a/ is a placeholder (its only file gave up); b/ is real.
        seed_file(&conn, project.id, "a/y.rs", "blob-y", "indexed");
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();
        store::code_file_increment_summary_attempts(&conn, project.id, "a/y.rs").unwrap();
        seed_file(&conn, project.id, "b/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "b/x.rs", "b1", "x summary");
        let llm = FakeLlm::default();
        run_pass(&conn, &llm, &project, None, now);
        let repo_prompt = llm.prompts.borrow().iter().find(|p| p.contains("Module: /\n")).cloned().expect("repo was summarised");
        assert!(repo_prompt.contains("b/:\n"));
        assert!(!repo_prompt.contains("a/:\n"), "an empty placeholder child adds nothing to the prompt");
    }

    #[test]
    fn a_mixed_repo_summarises_from_root_files_and_depth_1_modules_together() {
        // Final-review item 4.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "main.rs", "m1", "indexed");
        seed_file_summary(&conn, project.id, "main.rs", "m1", "main summary");
        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");
        let llm = FakeLlm::default();
        run_pass(&conn, &llm, &project, None, now);
        let repo_prompt = llm.prompts.borrow().iter().find(|p| p.contains("Module: /\n")).cloned().expect("repo was summarised");
        assert!(repo_prompt.contains("main.rs:\nmain summary"), "{}", repo_prompt);
        assert!(repo_prompt.contains("a/:\n"), "{}", repo_prompt);

        // A root-file change alone changes the repo's digest.
        let d1 = store::code_summary_get(&conn, project.id, "/").unwrap().unwrap().source_digest;
        seed_file(&conn, project.id, "main.rs", "m2", "indexed");
        seed_file_summary(&conn, project.id, "main.rs", "m2", "main summary v2");
        store::mark_code_summaries_stale(&conn, project.id, &["/".to_string()], now).unwrap();
        run_pass(&conn, &llm, &project, None, now);
        let d2 = store::code_summary_get(&conn, project.id, "/").unwrap().unwrap().source_digest;
        assert_ne!(d1, d2);
    }

    #[test]
    fn orphan_cleanup_invalidates_every_active_mirror_by_source_including_a_restored_one() {
        // Final-review item 5.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        store::code_summary_upsert(&conn, project.id, "a/", "module", "old a/", "d", now).unwrap();
        let old = store::upsert_index_memory(&conn, "proj", "code-index:proj:a/", "gen1", None, now).unwrap();
        let cur = store::upsert_index_memory_for_summary(&conn, project.id, "a/", "proj", "code-index:proj:a/", "gen2", None, now).unwrap();
        store::restore(&conn, old, now).unwrap();
        run_pass(&conn, &FakeLlm::default(), &project, None, now);
        assert!(store::get(&conn, old).unwrap().unwrap().invalidated_at.is_some(), "restored older generation invalidated too");
        assert!(store::get(&conn, cur).unwrap().unwrap().invalidated_at.is_some());
    }

    #[test]
    fn the_memory_mirror_is_a_short_lead_with_a_pointer_while_code_summaries_keeps_the_full_text() {
        // Final-review item 8.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        seed_file(&conn, project.id, "a/x.rs", "b1", "indexed");
        seed_file_summary(&conn, project.id, "a/x.rs", "b1", "x summary");
        let sentence = "The picking module resolves pointer rays against scene geometry. ";
        let long_para = sentence.repeat(15); // ~990 chars, one paragraph
        let reply = format!("{}\n\nSecond paragraph with more detail.", long_para.trim());
        let llm = FakeLlm { reply: RefCell::new(Some(reply.clone())), ..Default::default() };
        run_pass(&conn, &llm, &project, None, now);

        let row = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        assert_eq!(row.text, reply.trim(), "the full text stays in code_summaries");
        let mem = store::get(&conn, row.memory_id.unwrap()).unwrap().unwrap();
        let suffix = " (full: mach kb ask --project proj … overview a/)";
        assert!(mem.content.starts_with("proj a/: "), "{}", mem.content);
        assert!(mem.content.ends_with(suffix), "{}", mem.content);
        let lead = &mem.content["proj a/: ".len()..mem.content.len() - suffix.len()];
        assert!(lead.chars().count() <= 600, "{} chars", lead.chars().count());
        assert!(lead.ends_with('.'), "cut at a sentence boundary: {:?}", lead);
        assert!(!lead.contains("Second paragraph"));
    }

    #[test]
    fn lead_of_keeps_a_short_first_paragraph_whole_and_hard_cuts_one_with_no_sentence_end() {
        assert_eq!(lead_of("Short one. Two.\n\nMore."), "Short one. Two.");
        let no_stop = "word ".repeat(200);
        let lead = lead_of(&no_stop);
        assert!(lead.chars().count() <= 600);
        assert!(lead.ends_with('…'));
    }

    #[test]
    fn a_path_prefix_run_only_touches_modules_under_the_prefix_and_never_the_repo() {
        // Final-review item 10.
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let now = "2026-09-24T00:00:00Z";
        for (path, blob) in [("a/b/x.rs", "b1"), ("a/y.rs", "b2"), ("c/z.rs", "b3")] {
            seed_file(&conn, project.id, path, blob, "indexed");
            seed_file_summary(&conn, project.id, path, blob, &format!("summary for {}", path));
        }
        let llm = FakeLlm::default();
        run_pass(&conn, &llm, &project, Some("a"), now);
        assert_eq!(llm.calls.borrow().as_slice(), &["a/b/".to_string(), "a/".to_string()]);
        assert!(store::code_summary_get(&conn, project.id, "/").unwrap().is_none(), "no repo row at all on a prefix run");
        assert!(store::code_summary_get(&conn, project.id, "c/").unwrap().is_none());
        let repo_mirrors: i64 =
            conn.query_row("SELECT COUNT(*) FROM memories WHERE source = 'code-index:proj:/'", [], |r| r.get(0)).unwrap();
        assert_eq!(repo_mirrors, 0);
    }
}
