//! Code index: the incremental nightly job (see
//! docs/superpowers/specs/2026-09-23-code-index-design.md, "Nightly job",
//! phase 1 steps only: scope, chunks, context headers, embeddings).
//!
//! Per registered project whose root is a git repo (others are reported as
//! skipped, never touched): list the committed tree at `HEAD`, run the
//! scope pass if a new top-level directory appeared, then compare every
//! tracked file's blob against its `code_files` row -- a file whose blob
//! differs from its row (or that has no row) is (re)chunked, one whose blob
//! matches is never touched again. `git diff code_indexed_head HEAD` is
//! only consulted for rename detection. Files matching secret patterns
//! (path or content, see `secrets.rs`) are recorded `skipped` and never
//! chunked or sent anywhere. Every file currently `dirty` (this run's or a
//! prior run's) then gets one combined header+summary call (tag
//! `index_header`) and embeddings, and `code_indexed_head` advances only
//! when no file is left `dirty`. That combined call's reply may or may not
//! carry a `SUMMARY:` line -- headers are the phase-1 contract and always
//! finalize the file regardless -- so after every project's dirty files are
//! done, any `indexed`/`fallback` file still missing a matching-digest
//! `code_summaries` row (never produced one, predates this feature, or its
//! blob moved on since) gets one dedicated summary-only call (tag
//! `index_summary`). Either call, on success, writes the file's summary and
//! marks its ancestor module paths (and the repo root) stale, which is what
//! feeds the last stage: `code_index::summary::run_module_summary_pass`,
//! called once per project right after its own file-level summary pass,
//! regenerates every stale-or-missing module/repo summary bottom-up (depth
//! 2, then 1, then `/`) from its children's summaries, mirrors module/repo
//! summaries into `memories` as index-owned rows, and prunes a module whose
//! directory no longer has any indexed file. A shared [`Budget`] counts
//! `claude -p` calls (scope + header + file-summary + module-summary)
//! across the whole invocation and stops the run cleanly when spent --
//! whatever is left `dirty`, unsummarized, or stale is picked up again next
//! run. An error in one project is recorded on its [`ProjectOutcome`] and
//! the run moves on to the next project.
//!
//! Generic over [`ReflectLlm`] and [`Embedder`] so the tests below drive it
//! with fakes; [`plan_index`] is the read-only half (no LLM, no writes)
//! behind `mach kb index --dry-run`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection};

use crate::code_index::chunk;
use crate::code_index::git::{Change, Repo, TrackedFile};
use crate::code_index::scope::{self, Category};
use crate::code_index::secrets;
use crate::code_index::summary;
use crate::embed::Embedder;
use crate::reflect::{LoggedLlm, ReflectLlm};
use crate::store::{self, KbError, ProjectRow, ScopeRow};

/// Default `claude -p` call budget for one `mach kb index` invocation —
/// the spec's "default 300".
pub const DEFAULT_BUDGET: usize = 300;

/// After this many attempts at the CURRENT blob (a combined or
/// summary-only call that succeeded but produced no parseable `SUMMARY:`
/// line -- see `store::code_file_increment_summary_attempts`), a file stops
/// being queued for further summary-only calls in `run_summary_pass`: its
/// status stays `indexed`/`fallback` (headers already finalized it), it
/// simply never gets a `code_summaries` row for this blob. A later blob
/// change resets the count to 0 (`code_file_upsert`) and gives it a fresh
/// two tries. Also consulted by `code_index::summary::child_fresh`, which
/// treats a given-up file as "done" (contributes no summary, but no longer
/// blocks its module/repo ancestor's readiness) -- fix-list items 1 and 4.
pub(crate) const SUMMARY_GIVE_UP_ATTEMPTS: i64 = 2;

/// A changed file's text is truncated to this many characters before it
/// goes into the header prompt (spec: "truncate to 24,000 chars, noting
/// truncation").
const HEADER_TRUNCATE_CHARS: usize = 24_000;

/// Chunks with a NULL embedding (on finalized files) embedded per run by the
/// backfill step -- free (no LLM budget), bounded so an embedder outage that
/// left a big backlog can't make one run take hours.
pub const EMBED_BACKFILL_PER_RUN: usize = 1_000;

/// `indexed` files with `code_chunks` but no `code_symbols` row -- rows
/// written before the symbol graph (this task) shipped -- given one free
/// `chunk::extract_refs` pass per run (no LLM call), up to this many
/// files, shared across every project processed this run (mirrors
/// [`EMBED_BACKFILL_PER_RUN`]'s run-wide cap, threaded through
/// [`index_project`] as a `&mut usize` so one huge backlog project can't
/// starve every other project's own backfill this run).
pub const SYMBOL_BACKFILL_PER_RUN: usize = 500;

/// Text of the once-per-run embedder reachability check.
const EMBEDDER_PING: &str = "mach kb index: embedder reachability check";

/// Options for one `mach kb index` invocation. `budget` defaults to
/// [`DEFAULT_BUDGET`] via [`IndexOptions::new`] — `Default::default()`
/// would silently give `0` (no calls ever made), which is never what a
/// caller wants, so `Default` is implemented by hand.
#[derive(Debug, Clone)]
pub struct IndexOptions {
    /// Limit to exactly this registered project name; `None` runs every
    /// registered project. An unknown name is an error.
    pub project: Option<String>,
    /// `claude -p` call budget for the whole invocation (scope + header
    /// calls; embeddings are free).
    pub budget: usize,
    /// Limit changed-file detection to paths under this prefix
    /// (`path == prefix || path.starts_with(prefix + "/")`). Requires
    /// `project`. When set, `code_indexed_head` is never advanced, however
    /// clean the run — this is a partial view of the repo.
    pub path_prefix: Option<String>,
    /// Print the plan and change nothing (no LLM call, no write).
    pub dry_run: bool,
    /// Skip the scope/chunk/header/file-summary/module-summary stages
    /// entirely and run only the monthly commit-history stage
    /// (`code_index::history::run_history_pass`), still budgeted and
    /// resumable exactly like a normal run's history stage. Requires
    /// `project` (a history-only run always targets one repo) and is
    /// rejected together with `path_prefix` (a `--path-prefix` run already
    /// skips history for the opposite reason -- a partial file view has no
    /// meaningful month to report on -- so combining the two is
    /// contradictory, not just redundant). Lets a user (or a scheduled job)
    /// backfill every month's history for a repo without paying for a full
    /// scope/chunk/header pass first.
    pub history_only: bool,
}

impl IndexOptions {
    pub fn new() -> Self {
        IndexOptions { project: None, budget: DEFAULT_BUDGET, path_prefix: None, dry_run: false, history_only: false }
    }
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A per-`claude -p`-call counter shared across every project processed in
/// one [`run_index`] invocation. `pub(crate)` (plus `new`/`remaining`/
/// `record`, below) so `code_index::summary`'s module/repo summary stage --
/// called from [`index_project`], right after this project's file-level
/// summary pass -- can spend against the exact same counter rather than
/// getting a second, unlinked one; its own tests construct one directly
/// too, the same way `run_index` does.
///
/// Also the run's failure ledger: every call is recorded with its outcome,
/// and after [`MAX_CONSECUTIVE_FAILURES`] failed calls in a row the budget
/// reports nothing remaining, so every stage stops the same way it would
/// on a spent budget. An outage (claude down, throttled, timing out) then
/// costs a handful of calls instead of the whole budget, each one leaving
/// its file `dirty` with nothing written.
pub(crate) struct Budget {
    limit: usize,
    used: usize,
    failed: usize,
    consecutive_failures: usize,
    last_error: Option<String>,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Self {
        Budget { limit, used: 0, failed: 0, consecutive_failures: 0, last_error: None }
    }
    pub(crate) fn remaining(&self) -> bool {
        self.used < self.limit && !self.tripped()
    }
    /// Charges one call. `error` is `Some` when the call itself failed
    /// (spawn error, non-zero exit, timeout) -- not when it returned a
    /// reply that didn't parse, which the callers track on their own.
    pub(crate) fn record(&mut self, error: Option<&str>) {
        self.used += 1;
        match error {
            None => self.consecutive_failures = 0,
            Some(e) => {
                self.failed += 1;
                self.consecutive_failures += 1;
                self.last_error = Some(e.to_string());
            }
        }
    }
    pub(crate) fn failed(&self) -> usize {
        self.failed
    }
    /// True once [`MAX_CONSECUTIVE_FAILURES`] calls in a row have failed.
    pub(crate) fn tripped(&self) -> bool {
        self.consecutive_failures >= MAX_CONSECUTIVE_FAILURES
    }
}

/// True when a free-text reply is the model narrating tool calls it cannot
/// make (every index call runs with no tools) instead of answering, e.g.
/// `**Tool: bash**` followed by a JSON parameter block. Such a reply is
/// never stored: it would become a module summary or history entry, and
/// from there a mirrored memory.
pub(crate) fn is_tool_transcript(reply: &str) -> bool {
    reply.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("**Tool:") || l.starts_with("<function_calls>") || l.starts_with("<invoke ")
    })
}

/// Failed calls in a row after which a run stops. Low on purpose: failures
/// in a row mean the LLM side is down or throttled, not that one file is
/// bad, and every further call would just burn budget for nothing.
pub(crate) const MAX_CONSECUTIVE_FAILURES: usize = 5;

/// Timeouts for index calls. Longer than reflect's: a header call embeds a
/// whole file and answers with one header per chunk, and such calls were
/// measured at 40s+ during a slow period -- at 60s a throttled hour turns
/// every call into a timeout.
pub(crate) const TIMEOUT_INDEX_HAIKU: Duration = Duration::from_secs(120);
pub(crate) const TIMEOUT_INDEX_SONNET: Duration = Duration::from_secs(180);

/// The read-only plan for one project — what `mach kb index --dry-run`
/// prints. Never touches the LLM or writes to the database.
#[derive(Debug, Clone, Default)]
pub struct ProjectPlan {
    pub project: String,
    /// `Some(reason)` when the project's root isn't a git repo — nothing
    /// else on this row is meaningful in that case.
    pub skipped_reason: Option<String>,
    /// In-scope files that would be (re)chunked this run (secret-pattern
    /// paths excluded; secret *content* is only detected on a real run).
    pub files_to_index: usize,
    pub files_to_delete: usize,
    /// Renames whose blob is unchanged (rows moved, no re-chunk, no
    /// header call needed).
    pub files_moved: usize,
    /// Whether the scope pass would run (no scope decisions yet, or a new
    /// top-level directory appeared).
    pub needs_scope: bool,
    /// Commit-history months that would be (re)summarized -- new, changed
    /// digest, or missing their mirror (0 for a `--path-prefix` plan, which
    /// skips history).
    pub months_to_summarize: usize,
    /// Module/repo summaries currently stale (one `index_summary` call
    /// each once their children are ready; under the prefix only, never
    /// the repo, for a `--path-prefix` plan).
    pub modules_stale: usize,
    /// `claude -p` calls this project would need to finish (scope, if
    /// needed, one header call per file left `dirty`, plus the stale
    /// modules and changed months above). With `--history-only`, just the
    /// months.
    pub calls_needed: usize,
}

/// Per-project outcome of an actual (non-dry-run) [`run_index`] pass.
#[derive(Debug, Clone, Default)]
pub struct ProjectOutcome {
    pub project: String,
    /// Not a git repo, or the scope pass failed / couldn't run this time
    /// (the project is left untouched until next run).
    pub skipped_reason: Option<String>,
    /// This project errored (git/db failure); the run carried on with the
    /// next project. Nothing about the error is fatal to other projects.
    pub error: Option<String>,
    pub files_chunked: usize,
    pub files_deleted: usize,
    pub files_moved: usize,
    /// Files recorded `skipped` this run because their path or content
    /// matched a secret pattern (new files, plus already-stored rows swept).
    pub files_skipped_secret: usize,
    /// Header calls actually attempted this run (success or failure —
    /// each one spent one unit of budget).
    pub headers_attempted: usize,
    /// Of `headers_attempted`, calls that failed (timeout, spawn error,
    /// non-zero exit) -- their files stay `dirty`, nothing written.
    pub headers_failed: usize,
    /// Summary-only (`index_summary`) calls actually attempted this run,
    /// after all this project's dirty files were processed — same
    /// success-or-failure accounting as `headers_attempted`.
    pub summaries_attempted: usize,
    /// Of `summaries_attempted`, calls that failed.
    pub summaries_failed: usize,
    /// Module/repo summary (`index_summary`, model `sonnet`) calls actually
    /// attempted this run, after the file-level summary pass above —
    /// same success-or-failure accounting as `headers_attempted`. See
    /// `code_index::summary::run_module_summary_pass`.
    pub modules_attempted: usize,
    /// Of `modules_attempted`, calls that failed.
    pub modules_failed: usize,
    /// Module/repo `code_summaries` rows deleted this run because their
    /// directory no longer has any indexed file (their mirrored `memories`
    /// row, if any, was tombstoned via `store::invalidate_memory` in the
    /// same pass) — free, no LLM call, so not counted against budget.
    pub modules_pruned: usize,
    pub scope_ran: bool,
    /// True iff `code_indexed_head` was advanced to `HEAD` this run (no
    /// file left `dirty`, and no `--path-prefix` was given).
    pub head_advanced: bool,
    /// `indexed` files that had `code_chunks` but no `code_symbols` row
    /// (pre-symbol-graph rows) given a free `extract_refs` pass this run --
    /// see [`SYMBOL_BACKFILL_PER_RUN`].
    pub symbols_backfilled: usize,
    /// Monthly commit-history (`index_history`, model `sonnet`) calls
    /// actually attempted this run -- same success-or-failure accounting as
    /// `headers_attempted`. Always `0` for a `--path-prefix` run (this
    /// stage is skipped entirely then). See
    /// `code_index::history::run_history_pass`.
    pub months_summarized: usize,
    /// Of `months_summarized`, calls that failed (the month is left as it
    /// was and retried next run).
    pub months_failed: usize,
    /// `code_symbols` row count for this project, as of the end of this
    /// run -- same query `index status` uses
    /// ([`count_symbols_and_edges`]), read once more here purely for
    /// display so a run's own output doesn't require a separate `index
    /// status` call to see whether the symbol graph grew. Present (and
    /// possibly `0`) even for a `--history-only` run, which doesn't touch
    /// the symbol graph itself.
    pub symbols_total: usize,
    /// `code_edges` row count for this project, as of the end of this run.
    /// See `symbols_total`.
    pub edges_total: usize,
    /// Edges the resolution pass examined this run: every unresolved edge
    /// on a full pass (first run / backfill), only the queued ones on an
    /// incremental pass -- `0` for a run with no changes.
    pub edges_examined: usize,
}

/// The result of one [`run_index`] invocation.
#[derive(Debug, Clone, Default)]
pub struct IndexReport {
    pub projects: Vec<ProjectOutcome>,
    pub calls_used: usize,
    pub budget: usize,
    /// True iff the budget was fully spent this run.
    pub budget_reached: bool,
    /// Calls that failed this run (timeout, spawn error, non-zero exit),
    /// across every project and stage.
    pub calls_failed: usize,
    /// True iff the run stopped early after [`MAX_CONSECUTIVE_FAILURES`]
    /// failed calls in a row.
    pub stopped_on_failures: bool,
    /// The most recent call failure's message, if any call failed.
    pub last_error: Option<String>,
    /// Result of the once-per-run embedder reachability check. When false,
    /// no header call was made this run (a header without its embedding
    /// would be a wasted call) and no backfill ran.
    pub embedder_up: bool,
    /// Chunks embedded by the end-of-run backfill (NULL embeddings on
    /// already-finalized files, up to [`EMBED_BACKFILL_PER_RUN`]).
    pub backfill_embedded: usize,
    /// `code_summaries` rows embedded by the end-of-run backfill (NULL
    /// embeddings left by an embedder outage during a combined or
    /// summary-only call), up to [`EMBED_BACKFILL_PER_RUN`] — the summary
    /// twin of `backfill_embedded`.
    pub backfill_summaries_embedded: usize,
}

/// `mach kb index status`'s per-project view.
#[derive(Debug, Clone, Default)]
pub struct ProjectStatus {
    pub project: String,
    pub skipped_reason: Option<String>,
    pub head: Option<String>,
    pub code_indexed_head: Option<String>,
    pub indexed: usize,
    pub fallback: usize,
    pub dirty: usize,
    /// Files recorded `skipped` (secret path/content).
    pub skipped: usize,
    /// `(reason, count)` for the skipped files, e.g.
    /// `("secret-path(.env)", 2)`. Names the pattern, never the value.
    pub skip_reasons: Vec<(String, usize)>,
    pub chunks: usize,
    pub embedded_pct: f64,
    /// `indexed`/`fallback` files that gave up on ever getting a file
    /// summary for their current blob (`summary_attempts >=
    /// SUMMARY_GIVE_UP_ATTEMPTS`, still no matching-digest `code_summaries`
    /// row) -- fix-list item 1.
    pub summaries_given_up: usize,
    /// `code_symbols` row count for this project.
    pub symbols: usize,
    /// `code_edges` row count for this project.
    pub edges: usize,
    /// Percentage of `edges` considered resolved: for `calls`/`inherits`,
    /// `dst_symbol_id IS NOT NULL`; for `includes`/`imports` (which never
    /// get a `dst_symbol_id` -- they resolve to a file), `dst_name` matches
    /// a tracked `code_files.path` exactly (what `resolve_edges` rewrites
    /// it to on success). `0.0` when there are no edges at all.
    pub edges_resolved_pct: f64,
}

// ---------------------------------------------------------------------
// Argument validation shared by plan/run/status.
// ---------------------------------------------------------------------

fn selected_projects(conn: &Connection, opts: &IndexOptions) -> Result<Vec<ProjectRow>, KbError> {
    if opts.path_prefix.is_some() && opts.project.is_none() {
        return Err(KbError::Other("--path-prefix requires --project".to_string()));
    }
    if opts.history_only && opts.project.is_none() {
        return Err(KbError::Other("--history-only requires --project".to_string()));
    }
    if opts.history_only && opts.path_prefix.is_some() {
        return Err(KbError::Other("--history-only cannot be combined with --path-prefix".to_string()));
    }
    filter_projects(conn, opts.project.as_deref())
}

fn filter_projects(conn: &Connection, filter: Option<&str>) -> Result<Vec<ProjectRow>, KbError> {
    let all = store::list_projects(conn)?;
    match filter {
        None => Ok(all),
        Some(name) => {
            let picked: Vec<ProjectRow> = all.into_iter().filter(|p| p.name == name).collect();
            if picked.is_empty() {
                return Err(KbError::Other(format!("unknown project '{}' (see `mach kb projects`)", name)));
            }
            Ok(picked)
        }
    }
}

// ---------------------------------------------------------------------
// Planning (read-only: no LLM, no writes) — `--dry-run`.
// ---------------------------------------------------------------------

/// The read-only plan for every project `opts.project` selects (or every
/// registered project when unset), in registry order. Never calls the LLM
/// or an embedder, never writes to the database.
pub fn plan_index(conn: &Connection, opts: &IndexOptions) -> Result<Vec<ProjectPlan>, KbError> {
    let mut out = Vec::new();
    for p in selected_projects(conn, opts)? {
        out.push(plan_project(conn, &p, opts.path_prefix.as_deref(), opts.history_only)?);
    }
    Ok(out)
}

fn plan_project(conn: &Connection, project: &ProjectRow, path_prefix: Option<&str>, history_only: bool) -> Result<ProjectPlan, KbError> {
    let root = Path::new(&project.root_path);
    if !Repo::is_repo(root) {
        return Ok(ProjectPlan {
            project: project.name.clone(),
            skipped_reason: Some("not a git repo".to_string()),
            ..Default::default()
        });
    }
    let repo = Repo::new(root);
    // `--history-only` runs nothing but the history stage, so that is all
    // its plan counts.
    if history_only {
        let months = crate::code_index::history::months_needing_work(conn, &repo, project.id)?;
        return Ok(ProjectPlan { project: project.name.clone(), months_to_summarize: months, calls_needed: months, ..Default::default() });
    }
    let head = repo.head()?;
    let prev_head = code_indexed_head_of(conn, project.id)?;
    let tracked = repo.tracked_files_at(&head)?;
    let changes = compute_changes(conn, &repo, project.id, prev_head.as_deref(), &head, &tracked, path_prefix)?;

    let scope_rows = store::code_scope_get(conn, project.id)?;
    let in_scope = scope::guard_files(&repo, filter_in_scope(&scope_rows, changes.add_or_modify))?;
    let in_scope: Vec<TrackedFile> = in_scope.into_iter().filter(|f| secrets::path_reason(&f.path).is_none()).collect();

    let needs_scope_flag = scope::needs_scope(conn, project.id, &top_level_dirs(&tracked))?;

    let mut dirty_paths: HashSet<String> =
        store::code_files_with_status(conn, project.id, "dirty")?.into_iter().map(|r| r.path).collect();
    for f in &in_scope {
        dirty_paths.insert(f.path.clone());
    }
    let months_to_summarize =
        if path_prefix.is_none() { crate::code_index::history::months_needing_work(conn, &repo, project.id)? } else { 0 };
    let modules_stale = stale_module_count(conn, project.id, path_prefix)?;
    let calls_needed = usize::from(needs_scope_flag) + dirty_paths.len() + modules_stale + months_to_summarize;

    Ok(ProjectPlan {
        project: project.name.clone(),
        skipped_reason: None,
        files_to_index: in_scope.len(),
        files_to_delete: changes.delete.len(),
        files_moved: changes.moved.len(),
        needs_scope: needs_scope_flag,
        months_to_summarize,
        modules_stale,
        calls_needed,
    })
}

/// Stale module/repo `code_summaries` rows (paths ending `/`); under
/// `path_prefix` only, and never the repo row, for a prefix plan -- the
/// same set `summary::run_module_summary_pass` would consider.
fn stale_module_count(conn: &Connection, project_id: i64, path_prefix: Option<&str>) -> Result<usize, KbError> {
    let paths: Vec<String> = store::stale_code_summaries(conn, project_id)?.into_iter().map(|r| r.path).filter(|p| p.ends_with('/')).collect();
    Ok(match path_prefix {
        None => paths.len(),
        Some(prefix) => {
            let prefix = prefix.trim_end_matches('/');
            paths.iter().filter(|p| p.as_str() != "/" && (p.trim_end_matches('/') == prefix || p.starts_with(&format!("{prefix}/")))).count()
        }
    })
}

// ---------------------------------------------------------------------
// The real (mutating) run.
// ---------------------------------------------------------------------

/// Runs the incremental index for every project `opts.project` selects (or
/// every registered project when unset), smallest-leftover-`dirty`-backlog
/// first. Stops the whole run the moment the budget is spent.
///
/// `opts.history_only` (requires `opts.project`, rejected together with
/// `opts.path_prefix`) skips straight to the monthly commit-history stage
/// for that one project -- see the `history_only` branch at the top of
/// [`index_project`].
///
/// Per-project isolation: an error inside one project (git failure, a
/// corrupt row, ...) is recorded as that project's
/// [`ProjectOutcome::error`] and the run continues with the next project.
/// The one exception is a `--path-prefix` run, which is by construction a
/// manual, single-project run: its error (e.g. a prefix matching no tracked
/// file) is returned as-is so the caller sees it loudly.
///
/// Before any project, the embedder is pinged once; if it's down, no
/// header call is made this run (chunking still happens -- it's free), so
/// a header is never paid for without its embedding. After the projects,
/// up to [`EMBED_BACKFILL_PER_RUN`] chunks with a NULL embedding on
/// already-finalized files are embedded (no LLM budget).
///
/// `llm` is the raw client — every call is wrapped in `reflect::LoggedLlm`
/// internally (tags `index_scope` / `index_header`).
pub fn run_index<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    llm: &L,
    embedder: &E,
    opts: &IndexOptions,
) -> Result<IndexReport, KbError> {
    let now = store::now_rfc3339();
    let mut budget = Budget::new(opts.budget);
    let selected = selected_projects(conn, opts)?;
    let embedder_up = embedder.embed(EMBEDDER_PING).is_ok();

    let mut runnable: Vec<(usize, ProjectRow)> = Vec::new();
    let mut outcomes: Vec<ProjectOutcome> = Vec::new();
    for p in selected {
        if !Repo::is_repo(Path::new(&p.root_path)) {
            outcomes.push(ProjectOutcome {
                project: p.name.clone(),
                skipped_reason: Some("not a git repo".to_string()),
                ..Default::default()
            });
            continue;
        }
        let backlog = store::code_files_with_status(conn, p.id, "dirty")?.len();
        runnable.push((backlog, p));
    }
    runnable.sort_by_key(|(backlog, _)| *backlog);

    let project_ids: Vec<i64> = runnable.iter().map(|(_, p)| p.id).collect();
    let mut symbol_backfill_budget = SYMBOL_BACKFILL_PER_RUN;
    for (_, project) in runnable {
        if !budget.remaining() {
            break;
        }
        match index_project(
            conn,
            llm,
            embedder,
            embedder_up,
            &project,
            &mut budget,
            opts.path_prefix.as_deref(),
            opts.history_only,
            &now,
            &mut symbol_backfill_budget,
        ) {
            Ok(outcome) => outcomes.push(outcome),
            Err(e) if opts.path_prefix.is_some() => return Err(e),
            Err(e) => outcomes.push(ProjectOutcome {
                project: project.name.clone(),
                error: Some(e.to_string()),
                ..Default::default()
            }),
        }
    }

    let backfill_embedded = if embedder_up {
        let scope_ids = if opts.project.is_some() { Some(project_ids.as_slice()) } else { None };
        backfill_embeddings(conn, embedder, scope_ids, EMBED_BACKFILL_PER_RUN)?
    } else {
        0
    };
    let backfill_summaries_embedded = if embedder_up {
        let scope_id = if opts.project.is_some() { project_ids.first().copied() } else { None };
        backfill_summary_embeddings(conn, embedder, scope_id, EMBED_BACKFILL_PER_RUN)?
    } else {
        0
    };

    Ok(IndexReport {
        projects: outcomes,
        calls_used: budget.used,
        budget: opts.budget,
        budget_reached: budget.used >= opts.budget,
        calls_failed: budget.failed,
        stopped_on_failures: budget.tripped(),
        last_error: budget.last_error.clone(),
        embedder_up,
        backfill_embedded,
        backfill_summaries_embedded,
    })
}

#[allow(clippy::too_many_arguments)]
fn index_project<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    llm: &L,
    embedder: &E,
    embedder_up: bool,
    project: &ProjectRow,
    budget: &mut Budget,
    path_prefix: Option<&str>,
    history_only: bool,
    now: &str,
    symbol_backfill_budget: &mut usize,
) -> Result<ProjectOutcome, KbError> {
    let root = Path::new(&project.root_path);
    let repo = Repo::new(root);

    // `--history-only`: skip scope/chunk/header/file-summary/module-summary
    // entirely and run just the monthly commit-history stage, sharing the
    // same budget-gated, embedder-up-gated shape the normal run's own
    // history stage uses. `selected_projects` has already rejected this
    // combined with `path_prefix`, and rejected it with no `--project` at
    // all, so `path_prefix` is always `None` here.
    if history_only {
        let mut months_summarized = 0usize;
        let mut months_failed = 0usize;
        if embedder_up {
            let history_llm = LoggedLlm::new(conn, "index_history", llm);
            let failed_before = budget.failed();
            let history_result =
                crate::code_index::history::run_history_pass(conn, &history_llm, embedder, &repo, project.id, &project.name, budget, now)?;
            months_summarized = history_result.attempted;
            months_failed = budget.failed() - failed_before;
        }
        let (symbols_total, edges_total, _) = count_symbols_and_edges(conn, project.id)?;
        return Ok(ProjectOutcome {
            project: project.name.clone(),
            months_summarized,
            months_failed,
            symbols_total,
            edges_total,
            ..Default::default()
        });
    }

    let head = repo.head()?;
    let prev_head = code_indexed_head_of(conn, project.id)?;
    let tracked = repo.tracked_files_at(&head)?;

    // Scope. Stale llm rows (directories no longer in the tree) are dropped
    // first; only a NEW top-level directory triggers a (paid) re-scope. A
    // project that needs a scope pass and can't get one this run (budget
    // spent, or the LLM call failed) is skipped whole: indexing it under a
    // default-`product` guess would chunk vendored/generated code.
    scope::prune_stale_scope_rows(conn, project.id, &tracked)?;
    let mut scope_ran = false;
    if scope::needs_scope(conn, project.id, &top_level_dirs(&tracked))? {
        if !budget.remaining() {
            return Ok(ProjectOutcome {
                project: project.name.clone(),
                skipped_reason: Some("scope pass needed but budget spent; resumes next run".to_string()),
                ..Default::default()
            });
        }
        let scope_llm = LoggedLlm::new(conn, "index_scope", llm);
        let (_, failed) = scope::run_scope_pass(conn, &scope_llm, &repo, project.id, now)?;
        budget.record(failed.then_some("scope pass call failed"));
        scope_ran = true;
        if failed {
            return Ok(ProjectOutcome {
                project: project.name.clone(),
                skipped_reason: Some("scope pass LLM call failed; retried next run".to_string()),
                scope_ran,
                ..Default::default()
            });
        }
    }

    let changes = compute_changes(conn, &repo, project.id, prev_head.as_deref(), &head, &tracked, path_prefix)?;
    for p in &changes.delete {
        store::code_file_delete(conn, project.id, p)?;
        // The symbol graph mirrors the chunk delete right above it -- same
        // "row is gone, so is everything derived from it" rule, and (unlike
        // `code_chunks`) `code_symbols`/`code_edges` have no FK cascade off
        // `code_files`, so this is the only thing that ever removes them.
        store::delete_file_symbols(conn, project.id, p)?;
        // The file is gone: its own summary (if any) goes with it, and its
        // ancestor module paths + the repo root need regenerating.
        store::code_summary_delete(conn, project.id, p)?;
        store::mark_code_summaries_stale(conn, project.id, &ancestor_summary_paths(p), now)?;
    }
    for (from, to) in &changes.moved {
        move_code_rows(conn, project.id, from, to)?;
        store::move_file_symbols(conn, project.id, from, to)?;
        // A rename that crosses a directory boundary changes BOTH the old
        // and new parent's child set (the moved file leaves one module's
        // children and joins another's), so both sides' ancestor module
        // paths need regenerating -- a same-directory rename changes
        // nothing about any module's membership, so it's left alone (fix-
        // list item 2).
        if parent_dir(from) != parent_dir(to) {
            let mut paths = ancestor_summary_paths(from);
            paths.extend(ancestor_summary_paths(to));
            paths.sort();
            paths.dedup();
            store::mark_code_summaries_stale(conn, project.id, &paths, now)?;
        }
    }

    let scope_rows = store::code_scope_get(conn, project.id)?;
    cleanup_recategorized_files(conn, project.id, &project.name, &scope_rows, path_prefix, now)?;
    let mut files_skipped_secret = sweep_secret_rows(conn, project.id, &project.name, path_prefix, now)?;

    let in_scope = scope::guard_files(&repo, filter_in_scope(&scope_rows, changes.add_or_modify))?;
    let mut files_chunked = 0usize;
    for f in &in_scope {
        // Secret check BEFORE anything is chunked, stored, or sent: a
        // secret-pattern path isn't even read; a file whose content carries
        // a secret marker is recorded `skipped` with the pattern name only.
        if let Some(reason) = secrets::path_reason(&f.path) {
            mark_skipped(conn, project.id, &project.name, &f.path, &f.blob, reason, now)?;
            files_skipped_secret += 1;
            continue;
        }
        let text = repo.show(&head, &f.path)?;
        if let Some(reason) = secrets::content_reason(&text) {
            mark_skipped(conn, project.id, &project.name, &f.path, &f.blob, reason, now)?;
            files_skipped_secret += 1;
            continue;
        }
        let result = chunk::chunk_file(&f.path, &text);
        store::replace_code_chunks(conn, project.id, &f.path, &result.chunks)?;
        let lang = result.lang.map(|l| format!("{:?}", l));
        let lines = text.lines().count() as i64;
        store::code_file_upsert(conn, project.id, &f.path, &f.blob, lang.as_deref(), lines, "dirty", now)?;
        // After the upsert, so the row exists for `refs_blob` to land on.
        let refs = chunk::extract_refs(&f.path, &text);
        store::replace_file_symbols_for_blob(conn, project.id, &f.path, &f.blob, &refs.symbols, &refs.edges)?;
        files_chunked += 1;
    }

    // Symbol-graph backfill (free, no LLM): files whose current blob was
    // never run through `extract_refs` (`refs_blob` NULL or stale). Then
    // resolve edges exactly once for this project, now that this run's own
    // (re)chunked files AND any backfilled ones are both in place: a full
    // project-wide pass on the project's first run or whenever the backfill
    // did anything, otherwise an incremental pass over just what this run
    // (or an interrupted earlier one) queued in `code_graph_pending` -- a
    // run with no changes examines zero edges.
    let symbols_backfilled = backfill_symbols(conn, &repo, &head, project.id, path_prefix, symbol_backfill_budget)?;
    let edges_examined = if prev_head.is_none() || symbols_backfilled > 0 {
        store::resolve_edges_all(conn, project.id)?.examined
    } else {
        store::resolve_edges_incremental(conn, project.id)?.examined
    };

    let mut headers_attempted = 0usize;
    let mut summaries_attempted = 0usize;
    let mut modules_attempted = 0usize;
    let mut modules_pruned = 0usize;
    let mut months_summarized = 0usize;
    let (mut headers_failed, mut summaries_failed, mut modules_failed, mut months_failed) = (0usize, 0usize, 0usize, 0usize);
    if embedder_up {
        // A `--path-prefix` run finishes only that subtree's backlog; dirty
        // files elsewhere wait for a full run.
        let in_prefix = prefix_matcher(path_prefix);
        let dirty_files: Vec<_> =
            store::code_files_with_status(conn, project.id, "dirty")?.into_iter().filter(|f| in_prefix(&f.path)).collect();
        let header_llm = LoggedLlm::new(conn, "index_header", llm);
        let failed_before = budget.failed();
        for file in &dirty_files {
            if !budget.remaining() {
                break;
            }
            let attempted = finalize_dirty_file(
                conn,
                &header_llm,
                budget,
                embedder,
                &repo,
                &head,
                &project.name,
                project.id,
                &file.path,
                &file.blob,
                now,
            )?;
            if attempted {
                headers_attempted += 1;
            }
        }
        headers_failed = budget.failed() - failed_before;

        // After every dirty file this run is done: any indexed/fallback
        // file still missing a matching-digest summary (the combined call
        // above didn't produce one, or the file predates this feature)
        // gets one dedicated summary-only call.
        let summary_llm = LoggedLlm::new(conn, "index_summary", llm);
        let failed_before = budget.failed();
        summaries_attempted =
            run_summary_pass(conn, &summary_llm, embedder, &repo, &head, project.id, &project.name, budget, path_prefix, now)?;
        summaries_failed = budget.failed() - failed_before;

        // Finally, bottom-up module/repo summaries over whatever module
        // paths are stale or missing right now -- which includes every
        // ancestor path either summary pass above just staled by finishing
        // a file. Shares `budget` with everything above it (same counter,
        // same run).
        let failed_before = budget.failed();
        let module_result =
            summary::run_module_summary_pass(conn, &summary_llm, embedder, project.id, &project.name, budget, path_prefix, now)?;
        modules_attempted = module_result.attempted;
        modules_failed = budget.failed() - failed_before;
        modules_pruned = module_result.pruned;

        // Monthly commit-history summaries, last: a `--path-prefix` run
        // skips this stage entirely -- a partial view of the repo has no
        // meaningful "this month" history to report on (Task 3 brief).
        if path_prefix.is_none() {
            let history_llm = LoggedLlm::new(conn, "index_history", llm);
            let failed_before = budget.failed();
            let history_result =
                crate::code_index::history::run_history_pass(conn, &history_llm, embedder, &repo, project.id, &project.name, budget, now)?;
            months_summarized = history_result.attempted;
            months_failed = budget.failed() - failed_before;
        }
    }

    let still_dirty = store::code_files_with_status(conn, project.id, "dirty")?.len();
    let head_advanced = still_dirty == 0 && path_prefix.is_none();
    if head_advanced {
        store::set_code_indexed_head(conn, project.id, &head, now)?;
    }

    let (symbols_total, edges_total, _) = count_symbols_and_edges(conn, project.id)?;

    Ok(ProjectOutcome {
        project: project.name.clone(),
        skipped_reason: None,
        error: None,
        files_chunked,
        files_deleted: changes.delete.len(),
        files_moved: changes.moved.len(),
        files_skipped_secret,
        headers_attempted,
        headers_failed,
        summaries_attempted,
        summaries_failed,
        modules_attempted,
        modules_failed,
        modules_pruned,
        scope_ran,
        head_advanced,
        symbols_backfilled,
        months_summarized,
        months_failed,
        symbols_total,
        edges_total,
        edges_examined,
    })
}

/// Symbol-graph backfill: every `indexed` file (never `fallback` -- an
/// unrecognised extension or a severely-broken parse always yields
/// `Refs::default()`, see `chunk::extract_refs`'s own doc comment, so a
/// `fallback`-status file would only occupy a slot forever for nothing) NOT
/// a Markdown file (`extract_refs` is a deliberate no-op for prose, same
/// reason) whose `refs_blob` is NULL or differs from its current `blob` --
/// i.e. one whose current content was never run through `extract_refs`
/// (chunked before the symbol graph shipped). Keyed on `refs_blob`, not on
/// "has zero `code_symbols` rows": a file that legitimately defines nothing
/// used to be re-selected every run forever. One free `extract_refs` pass per file, reading its committed
/// text at `head` (a file only reaches `code_files` in `indexed` status
/// with its current blob matching what's tracked at `head` -- `compute_changes`
/// would have re-chunked it above otherwise, in the same run).
///
/// `remaining` is `&mut` so a run processing several projects spends one
/// shared cap across all of them (mirrors `EMBED_BACKFILL_PER_RUN`'s
/// run-wide accounting) while still letting each project resolve its own
/// edges exactly once, right after its own backfill, in [`index_project`].
/// Respects `path_prefix` the same way `sweep_secret_rows` does. A file
/// whose text can't be read at `head` (should not happen given the
/// invariant above, but git is an external process) is skipped without
/// spending any of `remaining`.
fn backfill_symbols(
    conn: &Connection,
    repo: &Repo,
    head: &str,
    project_id: i64,
    path_prefix: Option<&str>,
    remaining: &mut usize,
) -> Result<usize, KbError> {
    if *remaining == 0 {
        return Ok(0);
    }
    let keep = prefix_matcher(path_prefix);
    let mut stmt = conn.prepare(
        "SELECT path, blob FROM code_files
         WHERE project_id = ?1 AND status = 'indexed' AND (lang IS NULL OR lang != 'Markdown')
           AND (refs_blob IS NULL OR refs_blob != blob)
         ORDER BY path",
    )?;
    let rows: Vec<(String, String)> =
        stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<Result<_, _>>()?;

    let mut done = 0usize;
    for (path, blob) in rows {
        if *remaining == 0 {
            break;
        }
        if !keep(&path) {
            continue;
        }
        let Ok(text) = repo.show(head, &path) else { continue };
        let refs = chunk::extract_refs(&path, &text);
        // Records `refs_blob = blob` even when `refs` is empty, so a file
        // that legitimately has no symbols is never re-selected.
        store::replace_file_symbols_for_blob(conn, project_id, &path, &blob, &refs.symbols, &refs.edges)?;
        *remaining -= 1;
        done += 1;
    }
    Ok(done)
}

/// Records `path` as `skipped` with `reason` (a pattern label, never the
/// matched value) in `code_files.lang` -- for a `skipped` row that column
/// carries the skip reason, since a skipped file has no language to record
/// and a new column would need a schema migration for one string -- and
/// drops any chunks it had. Keyed on `blob`, so an unchanged skipped file is
/// never re-read on later runs. Also drops any summary the file had (a
/// secret-skipped file is never summarised, whether it was always secret or
/// only just swept) and stales its ancestor module paths -- the single
/// choke point every secret path (new file, new content, and the
/// `sweep_secret_rows` safety net) goes through, so "secret-skipped files
/// never summarised" holds everywhere `mark_skipped` is called from.
///
/// If the file HAD a summary (it was summarised before it turned secret),
/// every ancestor module/repo row is placeholder-ized right away (text
/// cleared, stale, so it regenerates from the remaining children) and its
/// mirrored memories invalidated (final-review item 13): their text may
/// have been generated from the now-secret file, and must stop being
/// served now, not whenever the module stage next gets budget.
fn mark_skipped(conn: &Connection, project_id: i64, project_name: &str, path: &str, blob: &str, reason: &str, now: &str) -> Result<(), KbError> {
    let had_summary = store::code_summary_get(conn, project_id, path)?.is_some();
    store::replace_code_chunks(conn, project_id, path, &[])?;
    store::delete_file_symbols(conn, project_id, path)?;
    store::code_file_upsert(conn, project_id, path, blob, Some(reason), 0, "skipped", now)?;
    store::code_summary_delete(conn, project_id, path)?;
    let ancestors = ancestor_summary_paths(path);
    if had_summary {
        for dir in &ancestors {
            if let Some(mid) = store::code_summary_placeholderize(conn, project_id, dir, now)? {
                store::invalidate_memory(conn, mid, now)?;
            }
            let source = format!("{}{}:{}", store::INDEX_OWNED_SOURCE_PREFIX, project_name, dir);
            store::invalidate_index_memories_by_source(conn, &source, now)?;
        }
    }
    store::mark_code_summaries_stale(conn, project_id, &ancestors, now)?;
    Ok(())
}

/// Safety net for rows stored before secret filtering existed (or before a
/// pattern was added): every non-`skipped` row whose path matches a secret
/// pattern, or any of whose chunks' text carries a secret marker, is turned
/// into a `skipped` row and its chunks deleted. Runs every time; it reads
/// chunk text only (no git, no LLM). Respects `path_prefix`.
fn sweep_secret_rows(conn: &Connection, project_id: i64, project_name: &str, path_prefix: Option<&str>, now: &str) -> Result<usize, KbError> {
    let keep = prefix_matcher(path_prefix);
    let rows = all_code_file_rows(conn, project_id)?;
    let mut hits: Vec<(String, String, &'static str)> = Vec::new();
    let mut flagged: HashSet<String> = HashSet::new();
    for (path, (blob, status)) in &rows {
        if status == "skipped" || !keep(path) {
            continue;
        }
        if let Some(reason) = secrets::path_reason(path) {
            flagged.insert(path.clone());
            hits.push((path.clone(), blob.clone(), reason));
        }
    }
    {
        let mut stmt = conn.prepare("SELECT path, text FROM code_chunks WHERE project_id = ?1")?;
        let mut q = stmt.query(params![project_id])?;
        while let Some(r) = q.next()? {
            let path: String = r.get(0)?;
            if flagged.contains(&path) || !keep(&path) {
                continue;
            }
            let text: String = r.get(1)?;
            if let Some(reason) = secrets::content_reason(&text) {
                if let Some((blob, status)) = rows.get(&path) {
                    if status != "skipped" {
                        flagged.insert(path.clone());
                        hits.push((path, blob.clone(), reason));
                    }
                }
            }
        }
    }
    for (path, blob, reason) in &hits {
        mark_skipped(conn, project_id, project_name, path, blob, reason, now)?;
    }
    Ok(hits.len())
}

/// One dirty file's header + embed pass. Returns whether a header call was
/// actually attempted (so the caller can charge the budget) — a file with
/// zero chunks (an empty file) is finalized straight to `indexed` with no
/// call at all, and a file whose text trips the secret check is turned
/// `skipped` with no call either (defense in depth: it could only be dirty
/// here if it was chunked by an older binary).
///
/// Every attempted call is recorded on `budget` with its outcome. On an LLM
/// failure the file is left exactly as it was (still `dirty`) — retried
/// whole next time. On success, headers are applied per the reply,
/// then every chunk is embedded (an embedder failure leaves that one
/// embedding `NULL` for the backfill step, and does not stop the file from
/// being finalized). If the reply also carries a `SUMMARY:` line, the file
/// summary is written (`code_summary_upsert`, `level = "file"`,
/// `source_digest = blob`), embedded the same NULL-on-failure way, and its
/// ancestor module paths + the repo root are marked stale. A reply with no
/// summary line leaves the file's `code_summaries` row exactly as it was
/// (missing, or stale-digest) — headers are the phase-1 contract, so the
/// file still finalizes either way, and `run_summary_pass` picks up the gap
/// afterward. Final status is `fallback` when every chunk is a `"window"`
/// chunk, else `indexed`.
#[allow(clippy::too_many_arguments)]
fn finalize_dirty_file<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    header_llm: &LoggedLlm<L>,
    budget: &mut Budget,
    embedder: &E,
    repo: &Repo,
    head: &str,
    project_name: &str,
    project_id: i64,
    path: &str,
    blob: &str,
    now: &str,
) -> Result<bool, KbError> {
    let metas = code_chunks_for_file(conn, project_id, path)?;
    if metas.is_empty() {
        store::code_file_set_status(conn, project_id, path, "indexed")?;
        return Ok(false);
    }

    let file_text = repo.show(head, path)?;
    if let Some(reason) = secrets::reason(path, &file_text) {
        mark_skipped(conn, project_id, project_name, path, blob, reason, now)?;
        return Ok(false);
    }
    let prompt = build_header_prompt(project_name, path, &file_text, &metas);
    let result = header_llm.call("haiku", &prompt, TIMEOUT_INDEX_HAIKU);
    budget.record(result.as_ref().err().map(String::as_str));
    match result {
        Err(_) => Ok(true),
        Ok(reply) => {
            let (parsed, summary) = parse_header_reply(&reply);
            for (i, m) in metas.iter().enumerate() {
                if let Some(h) = parsed.get(&(i + 1)) {
                    store::set_code_chunk_header(conn, m.id, h)?;
                }
            }
            for (i, m) in metas.iter().enumerate() {
                let header = parsed.get(&(i + 1)).map(|s| s.as_str());
                let input = embedding_input(header, &m.text);
                if let Ok(v) = embedder.embed(&input) {
                    store::set_code_chunk_embedding(conn, m.id, &v)?;
                }
            }
            if let Some(summary) = summary {
                store::code_summary_upsert(conn, project_id, path, "file", &summary, blob, now)?;
                if let Ok(v) = embedder.embed(&summary) {
                    store::set_code_summary_embedding(conn, project_id, path, &v)?;
                }
                store::mark_code_summaries_stale(conn, project_id, &ancestor_summary_paths(path), now)?;
            } else {
                // The call succeeded but produced no parseable SUMMARY:
                // line for this blob -- counts toward the give-up cap so
                // `run_summary_pass` doesn't retry it forever (fix-list
                // item 1). Headers still finalize the file either way.
                record_unparseable_summary(conn, project_id, path, now)?;
            }
            let fallback = metas.iter().all(|m| m.kind == "window");
            store::code_file_set_status(conn, project_id, path, if fallback { "fallback" } else { "indexed" })?;
            Ok(true)
        }
    }
}

/// Counts one "succeeded, but no parseable SUMMARY:" attempt for `path`'s
/// current blob. The attempt that reaches [`SUMMARY_GIVE_UP_ATTEMPTS`]
/// (final-review item 11) also deletes the file's summary row if one is
/// left over from an older blob -- it describes code that no longer exists
/// and would otherwise keep feeding its modules -- and stales its
/// ancestors so they are re-derived without it.
fn record_unparseable_summary(conn: &Connection, project_id: i64, path: &str, now: &str) -> Result<(), KbError> {
    let attempts = store::code_file_increment_summary_attempts(conn, project_id, path)?;
    if attempts >= SUMMARY_GIVE_UP_ATTEMPTS {
        if store::code_summary_get(conn, project_id, path)?.is_some() {
            store::code_summary_delete(conn, project_id, path)?;
        }
        store::mark_code_summaries_stale(conn, project_id, &ancestor_summary_paths(path), now)?;
    }
    Ok(())
}

/// Embeds up to `limit` chunks whose embedding is NULL on files already
/// finalized (`indexed`/`fallback` -- a `dirty` file's chunks get embedded
/// by its own header pass anyway), optionally only for `project_ids`.
/// Same input shape as the header pass (`embedding_input(header, text)`),
/// which is why this reads `header` alongside the text rather than going
/// through `store::code_chunks_missing_embedding` (text only). Stops at the
/// first embedder failure (it's down; the rest would fail too).
fn backfill_embeddings<E: Embedder>(
    conn: &Connection,
    embedder: &E,
    project_ids: Option<&[i64]>,
    limit: usize,
) -> Result<usize, KbError> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.project_id, c.header, c.text FROM code_chunks c
         JOIN code_files f ON f.project_id = c.project_id AND f.path = c.path
         WHERE c.embedding IS NULL AND f.status IN ('indexed', 'fallback')
         ORDER BY c.id",
    )?;
    let rows: Vec<(i64, i64, Option<String>, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<Result<_, _>>()?;
    let mut done = 0usize;
    for (id, pid, header, text) in rows {
        if done >= limit {
            break;
        }
        if let Some(ids) = project_ids {
            if !ids.contains(&pid) {
                continue;
            }
        }
        match embedder.embed(&embedding_input(header.as_deref(), &text)) {
            Ok(v) => {
                store::set_code_chunk_embedding(conn, id, &v)?;
                done += 1;
            }
            Err(_) => break,
        }
    }
    Ok(done)
}

/// Embeds up to `limit` `code_summaries` rows still missing an embedding --
/// the summary-level twin of `backfill_embeddings`, for the same reason: an
/// embedder outage during a combined or summary-only call leaves a
/// summary's `embedding` NULL, closed here later with no LLM call. Reads
/// the text straight off `code_summaries_missing_embedding` and embeds it
/// as-is -- a summary has no header/chunk-text split to combine the way
/// `embedding_input` does for chunks.
///
/// `project_id` scopes the backfill to one project (fix-list item 3) --
/// `run_index` passes `Some(id)` for a `--project`-limited run (mirroring
/// `backfill_embeddings`'s own `scope_ids`) so a `limit`-sized backfill
/// spends its whole budget on THIS project's rows, never another
/// project's, and `None` for an unscoped run (every project).
fn backfill_summary_embeddings<E: Embedder>(conn: &Connection, embedder: &E, project_id: Option<i64>, limit: usize) -> Result<usize, KbError> {
    let rows = store::code_summaries_missing_embedding(conn, project_id, limit)?;
    let mut done = 0usize;
    for (project_id, path, text) in rows {
        // Skip-and-continue (final-review item 3): one input the embedder
        // rejects must not starve every row after it. `limit` still bounds
        // the attempts when the embedder is down altogether.
        match embedder.embed(&text) {
            Ok(v) => {
                store::set_code_summary_embedding(conn, project_id, &path, &v)?;
                done += 1;
            }
            Err(_) => continue,
        }
    }
    Ok(done)
}

/// The summary-only pass for one project, run once per project after all
/// its dirty files this run finished (`finalize_dirty_file`'s own loop, in
/// `index_project`): every `indexed`/`fallback` file whose `code_summaries`
/// row is missing, or whose `source_digest` no longer matches the file's
/// current blob (the combined call that (re)chunked it didn't produce a
/// `SUMMARY:` line, or the file predates this feature entirely), gets one
/// dedicated haiku call (tag `index_summary`, `build_summary_prompt`)
/// asking only for the summary. `skipped` files are never candidates --
/// their text is never read here, same as everywhere else in this module.
/// A reply with no usable `SUMMARY:` line increments
/// `code_files.summary_attempts` for that blob (`store::
/// code_file_increment_summary_attempts`); once that reaches
/// [`SUMMARY_GIVE_UP_ATTEMPTS`], the file is skipped here entirely (no
/// call, not counted in `attempted`) -- it stays `indexed`/`fallback` with
/// no `code_summaries` row for this blob until the blob itself changes and
/// resets the count. Below the cap, a failed or unparseable attempt just
/// comes back around next run like before.
///
/// Returns the number of calls actually attempted this project (success or
/// failure -- each one spends one unit of `budget`, same accounting as
/// `headers_attempted`).
#[allow(clippy::too_many_arguments)]
fn run_summary_pass<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    summary_llm: &LoggedLlm<L>,
    embedder: &E,
    repo: &Repo,
    head: &str,
    project_id: i64,
    project_name: &str,
    budget: &mut Budget,
    path_prefix: Option<&str>,
    now: &str,
) -> Result<usize, KbError> {
    let in_prefix = prefix_matcher(path_prefix);
    let mut candidates = store::code_files_with_status(conn, project_id, "indexed")?;
    candidates.extend(store::code_files_with_status(conn, project_id, "fallback")?);
    candidates.retain(|f| in_prefix(&f.path));
    candidates.sort_by(|a, b| a.path.cmp(&b.path));

    let mut attempted = 0usize;
    for file in &candidates {
        if !budget.remaining() {
            break;
        }
        let needs_summary = match store::code_summary_get(conn, project_id, &file.path)? {
            None => true,
            Some(row) => row.source_digest != file.blob,
        };
        if !needs_summary {
            continue;
        }
        if file.summary_attempts >= SUMMARY_GIVE_UP_ATTEMPTS {
            // Already tried and failed to parse a summary out of this
            // blob twice; stop queuing it (fix-list item 1). Status stays
            // exactly as it was -- indexed/fallback, just no summary.
            continue;
        }
        let file_text = repo.show(head, &file.path)?;
        // Defense in depth, same rationale as finalize_dirty_file's own
        // check -- could only trip here for a row chunked by an older
        // binary, before this pattern existed.
        if secrets::reason(&file.path, &file_text).is_some() {
            continue;
        }
        let prompt = build_summary_prompt(project_name, &file.path, &file_text);
        attempted += 1;
        let result = summary_llm.call("haiku", &prompt, TIMEOUT_INDEX_HAIKU);
        budget.record(result.as_ref().err().map(String::as_str));
        if let Ok(reply) = result {
            if let Some(summary) = parse_summary_reply(&reply) {
                store::code_summary_upsert(conn, project_id, &file.path, "file", &summary, &file.blob, now)?;
                if let Ok(v) = embedder.embed(&summary) {
                    store::set_code_summary_embedding(conn, project_id, &file.path, &v)?;
                }
                store::mark_code_summaries_stale(conn, project_id, &ancestor_summary_paths(&file.path), now)?;
            } else {
                // Succeeded, but no parseable SUMMARY: line -- counts
                // toward the give-up cap (fix-list item 1).
                record_unparseable_summary(conn, project_id, &file.path, now)?;
            }
        }
    }
    Ok(attempted)
}

// ---------------------------------------------------------------------
// Status.
// ---------------------------------------------------------------------

/// `mach kb index status` for every project `filter` selects (or every
/// registered project when `None`). An unknown `filter` is an error.
pub fn all_project_statuses(conn: &Connection, filter: Option<&str>) -> Result<Vec<ProjectStatus>, KbError> {
    let mut out = Vec::new();
    for p in filter_projects(conn, filter)? {
        out.push(project_status(conn, &p)?);
    }
    Ok(out)
}

fn project_status(conn: &Connection, project: &ProjectRow) -> Result<ProjectStatus, KbError> {
    let root = Path::new(&project.root_path);
    if !Repo::is_repo(root) {
        return Ok(ProjectStatus {
            project: project.name.clone(),
            skipped_reason: Some("not a git repo".to_string()),
            ..Default::default()
        });
    }
    let repo = Repo::new(root);
    let head = repo.head().ok();
    let code_indexed_head = code_indexed_head_of(conn, project.id)?;
    let indexed = store::code_files_with_status(conn, project.id, "indexed")?.len();
    let fallback = store::code_files_with_status(conn, project.id, "fallback")?.len();
    let dirty = store::code_files_with_status(conn, project.id, "dirty")?.len();
    let skip_reasons = skip_reason_counts(conn, project.id)?;
    let skipped = skip_reasons.iter().map(|(_, n)| n).sum();
    let (chunks, embedded) = count_chunks(conn, project.id)?;
    let embedded_pct = if chunks == 0 { 0.0 } else { (embedded as f64 / chunks as f64) * 100.0 };
    let summaries_given_up = given_up_summary_count(conn, project.id)?;
    let (symbols, edges, edges_resolved_pct) = count_symbols_and_edges(conn, project.id)?;
    Ok(ProjectStatus {
        project: project.name.clone(),
        skipped_reason: None,
        head,
        code_indexed_head,
        indexed,
        fallback,
        dirty,
        skipped,
        skip_reasons,
        chunks,
        embedded_pct,
        summaries_given_up,
        symbols,
        edges,
        edges_resolved_pct,
    })
}

/// `code_symbols`/`code_edges` row counts for `index status`'s `symbols`/
/// `edges`/`edges_resolved %` fields. An edge counts as resolved when
/// `resolve_edges` has done everything it ever will for it: `calls`/
/// `inherits` need `dst_symbol_id IS NOT NULL`; `includes`/`imports` never
/// get one (they resolve to a file, not a symbol) but count as resolved
/// once `dst_path` holds the tracked path they resolved to
/// (`resolve_edges`'s own success condition for that kind).
fn count_symbols_and_edges(conn: &Connection, project_id: i64) -> Result<(usize, usize, f64), KbError> {
    let symbols: i64 = conn.query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1", params![project_id], |r| r.get(0))?;
    let edges: i64 = conn.query_row("SELECT COUNT(*) FROM code_edges WHERE project_id = ?1", params![project_id], |r| r.get(0))?;
    let resolved: i64 = conn.query_row(
        "SELECT COALESCE(SUM(CASE
             WHEN kind IN ('calls','inherits') THEN (dst_symbol_id IS NOT NULL)
             WHEN kind IN ('includes','imports') THEN (dst_path IS NOT NULL)
             ELSE 0
         END), 0)
         FROM code_edges WHERE project_id = ?1",
        params![project_id],
        |r| r.get(0),
    )?;
    let pct = if edges == 0 { 0.0 } else { (resolved as f64 / edges as f64) * 100.0 };
    Ok((symbols as usize, edges as usize, pct))
}

fn skip_reason_counts(conn: &Connection, project_id: i64) -> Result<Vec<(String, usize)>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(lang, 'unknown'), COUNT(*) FROM code_files
         WHERE project_id = ?1 AND status = 'skipped' GROUP BY 1 ORDER BY 1",
    )?;
    let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as usize)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Count of `indexed`/`fallback` files that have given up on ever getting a
/// file summary for their current blob -- `summary_attempts` reached
/// [`SUMMARY_GIVE_UP_ATTEMPTS`] and there is still no `code_summaries` row
/// whose `source_digest` matches the file's current blob (fix-list item 1's
/// `index status` surface).
fn given_up_summary_count(conn: &Connection, project_id: i64) -> Result<usize, KbError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM code_files f
         LEFT JOIN code_summaries s ON s.project_id = f.project_id AND s.path = f.path AND s.level = 'file'
         WHERE f.project_id = ?1 AND f.status IN ('indexed', 'fallback')
           AND f.summary_attempts >= ?2
           AND (s.path IS NULL OR s.source_digest != f.blob)",
        params![project_id, SUMMARY_GIVE_UP_ATTEMPTS],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

// ---------------------------------------------------------------------
// Change detection, shared by planning and the real run.
// ---------------------------------------------------------------------

/// What changed for one project, before scope filtering and guarding.
struct Changes {
    add_or_modify: Vec<TrackedFile>,
    delete: Vec<String>,
    moved: Vec<(String, String)>,
}

fn prefix_matcher(path_prefix: Option<&str>) -> impl Fn(&str) -> bool + '_ {
    // Trailing '/' normalized away: "src/" and "src" select the same files.
    let prefix = path_prefix.map(|p| p.trim_end_matches('/'));
    move |p: &str| match prefix {
        Some(prefix) => p == prefix || (p.len() > prefix.len() && p.starts_with(prefix) && p.as_bytes()[prefix.len()] == b'/'),
        None => true,
    }
}

/// Changes for one project, always derived from the blob state, never from
/// `git diff`'s Added/Modified list:
///
/// - `add_or_modify`: every tracked file (committed tree at `head`) whose
///   blob differs from its `code_files` row, or that has no row. A file
///   whose blob matches its row -- whatever its status, `dirty` included --
///   is never re-chunked (the dirty-file pass picks up leftovers
///   independently). This is what makes a budget-limited run converge: the
///   old diff-derived list re-reported (and re-dirtied) every file changed
///   since `code_indexed_head` on every run until the head advanced.
/// - `delete`: every row whose path is no longer tracked.
/// - `moved`: `git diff -M prev_head head` renames whose target blob equals
///   the source row's blob (rows moved in place, headers and embeddings
///   kept) -- only when `prev_head` exists, differs from `head`, and is
///   still a commit in the repo (a gc'd or rewritten sha falls back to plain
///   blob-compare: the rename just shows up as a delete + an add). A failing
///   diff likewise just disables rename detection.
fn compute_changes(
    conn: &Connection,
    repo: &Repo,
    project_id: i64,
    prev_head: Option<&str>,
    head: &str,
    tracked: &[TrackedFile],
    path_prefix: Option<&str>,
) -> Result<Changes, KbError> {
    let rows = all_code_file_rows(conn, project_id)?;
    let tracked_by_path: HashMap<&str, &str> = tracked.iter().map(|f| (f.path.as_str(), f.blob.as_str())).collect();

    let mut moved: Vec<(String, String)> = Vec::new();
    if let Some(prev) = prev_head {
        if prev != head && repo.is_commit(prev) {
            for change in repo.diff(prev, head).unwrap_or_default() {
                if let Change::Renamed { from, to } = change {
                    let same_blob = match (rows.get(&from), tracked_by_path.get(to.as_str())) {
                        (Some((row_blob, _)), Some(&new_blob)) => row_blob == new_blob,
                        _ => false,
                    };
                    if same_blob && !rows.contains_key(&to) && !tracked_by_path.contains_key(from.as_str()) {
                        moved.push((from, to));
                    }
                }
            }
        }
    }
    let moved_from: HashSet<&str> = moved.iter().map(|(f, _)| f.as_str()).collect();
    let moved_to: HashSet<&str> = moved.iter().map(|(_, t)| t.as_str()).collect();

    let mut add_or_modify: Vec<TrackedFile> = tracked
        .iter()
        .filter(|f| !moved_to.contains(f.path.as_str()))
        .filter(|f| rows.get(&f.path).map(|(blob, _)| blob != &f.blob).unwrap_or(true))
        .cloned()
        .collect();
    let mut delete: Vec<String> = rows
        .keys()
        .filter(|p| !tracked_by_path.contains_key(p.as_str()) && !moved_from.contains(p.as_str()))
        .cloned()
        .collect();
    delete.sort();

    if let Some(raw_prefix) = path_prefix {
        let keep = prefix_matcher(Some(raw_prefix));
        // Checked against ALL tracked files: a valid prefix with nothing new
        // to do must not error.
        if !tracked.iter().any(|f| keep(&f.path)) {
            return Err(KbError::Other(format!(
                "--path-prefix '{}' matches no tracked files",
                raw_prefix.trim_end_matches('/')
            )));
        }
        add_or_modify.retain(|f| keep(&f.path));
        delete.retain(|p| keep(p));
        moved.retain(|(from, to)| keep(from) || keep(to));
    }

    Ok(Changes { add_or_modify, delete, moved })
}

fn filter_in_scope(scope_rows: &[ScopeRow], files: Vec<TrackedFile>) -> Vec<TrackedFile> {
    files
        .into_iter()
        .filter(|f| matches!(scope::category_for(scope_rows, &f.path), Category::Product | Category::Tests | Category::Docs))
        .collect()
}

fn top_level_dirs(tracked: &[TrackedFile]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for f in tracked {
        if let Some(idx) = f.path.find('/') {
            out.insert(f.path[..idx].to_string());
        }
    }
    out
}

/// `path -> (blob, status)` for every `code_files` row of the project.
fn all_code_file_rows(conn: &Connection, project_id: i64) -> Result<HashMap<String, (String, String)>, KbError> {
    let mut stmt = conn.prepare("SELECT path, blob, status FROM code_files WHERE project_id = ?1")?;
    let rows = stmt.query_map(params![project_id], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, String>(1)?, r.get::<_, String>(2)?))))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Recategorization cleanup: a `code_files` row whose directory's
/// *current* effective category (`scope::category_for`) is no longer
/// `product`/`tests`/`docs` -- because a scope re-run just above
/// reclassified it, or because `mach kb index scope <project> --set
/// <dir>=<category>` wrote a `user` override before this run started --
/// has its row (and, cascading, its chunks) deleted. Not something
/// `compute_changes`/git-diff ever surfaces: the file itself may not have
/// changed at all, only its directory's scope decision did.
///
/// A file whose *new* category is `vendored` also gets that directory's
/// vendored memory written (the same one `run_scope_pass` writes when an
/// LLM reply itself says `vendored`) -- a `--set` override never goes
/// through that reply path, so this is the only place it happens for a
/// user-set vendored directory.
///
/// Respects `path_prefix` exactly like `compute_changes` does: a
/// `--path-prefix`-scoped run only cleans up files under that subtree,
/// same normalization (trailing slash stripped) as there.
///
/// Also deletes each removed file's summary and stales its ancestor module
/// paths (batched into one `mark_code_summaries_stale` call after the
/// loop) -- a file recategorized out of scope is gone from the index the
/// same way a deleted file is, so it must leave the same trail behind.
fn cleanup_recategorized_files(
    conn: &Connection,
    project_id: i64,
    project_name: &str,
    scope_rows: &[ScopeRow],
    path_prefix: Option<&str>,
    now: &str,
) -> Result<usize, KbError> {
    let prefix = path_prefix.map(|p| p.trim_end_matches('/'));
    let keep = |p: &str| match prefix {
        Some(prefix) => p == prefix || p.starts_with(&format!("{}/", prefix)),
        None => true,
    };

    let mut removed = 0usize;
    let mut vendored_dirs: HashSet<String> = HashSet::new();
    let mut stale_paths: HashSet<String> = HashSet::new();
    for path in all_code_file_paths(conn, project_id)? {
        if !keep(&path) {
            continue;
        }
        let category = scope::category_for(scope_rows, &path);
        if matches!(category, Category::Product | Category::Tests | Category::Docs) {
            continue;
        }
        store::code_file_delete(conn, project_id, &path)?;
        store::delete_file_symbols(conn, project_id, &path)?;
        store::code_summary_delete(conn, project_id, &path)?;
        stale_paths.extend(ancestor_summary_paths(&path));
        removed += 1;
        if category == Category::Vendored {
            if let Some(dir) = scope::owning_dir(scope_rows, &path) {
                vendored_dirs.insert(dir.to_string());
            }
        }
    }
    if !stale_paths.is_empty() {
        let paths: Vec<String> = stale_paths.into_iter().collect();
        store::mark_code_summaries_stale(conn, project_id, &paths, now)?;
    }
    for dir in vendored_dirs {
        scope::write_vendored_memory(conn, project_name, &dir, None)?;
    }
    Ok(removed)
}

// ---------------------------------------------------------------------
// Small SQL helpers not in store.rs's public surface (same rationale as
// scope.rs's `project_name`/`memory_source_exists`: narrow, private,
// crate-internal, promote to store.rs later if another caller needs them).
// ---------------------------------------------------------------------

/// `projects.code_indexed_head` for one project. Not exposed by
/// `store::ProjectRow`/`row_to_project` today (only `set_code_indexed_head`
/// exists in the public store surface, no getter) -- a narrow, private,
/// crate-internal read, same rationale as `scope.rs`'s own small SQL
/// helpers below the same heading there.
fn code_indexed_head_of(conn: &Connection, project_id: i64) -> Result<Option<String>, KbError> {
    Ok(conn.query_row("SELECT code_indexed_head FROM projects WHERE id = ?1", params![project_id], |r| r.get(0))?)
}

/// Every path this project currently has a `code_files` row for,
/// regardless of status -- what [`cleanup_recategorized_files`] scans.
/// `store::code_files_with_status` only offers a per-status query, no
/// "all of them" one, so this is the same kind of narrow private helper.
fn all_code_file_paths(conn: &Connection, project_id: i64) -> Result<Vec<String>, KbError> {
    let mut stmt = conn.prepare("SELECT path FROM code_files WHERE project_id = ?1")?;
    let rows = stmt.query_map(params![project_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

struct ChunkMeta {
    id: i64,
    symbol: Option<String>,
    kind: String,
    start_line: i64,
    end_line: i64,
    text: String,
}

fn code_chunks_for_file(conn: &Connection, project_id: i64, path: &str) -> Result<Vec<ChunkMeta>, KbError> {
    let mut stmt = conn.prepare(
        "SELECT id, symbol, kind, start_line, end_line, text FROM code_chunks \
         WHERE project_id = ?1 AND path = ?2 ORDER BY start_line, id",
    )?;
    let rows = stmt.query_map(params![project_id, path], |r| {
        Ok(ChunkMeta { id: r.get(0)?, symbol: r.get(1)?, kind: r.get(2)?, start_line: r.get(3)?, end_line: r.get(4)?, text: r.get(5)? })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn count_chunks(conn: &Connection, project_id: i64) -> Result<(usize, usize), KbError> {
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM code_chunks WHERE project_id = ?1", params![project_id], |r| r.get(0))?;
    let embedded: i64 = conn.query_row(
        "SELECT COUNT(*) FROM code_chunks WHERE project_id = ?1 AND embedding IS NOT NULL",
        params![project_id],
        |r| r.get(0),
    )?;
    Ok((total as usize, embedded as usize))
}

/// Moves every `code_files`/`code_chunks` row of `from` to `to` in place —
/// the rename-with-unchanged-blob case, so headers and embeddings survive
/// a pure `git mv`. `code_chunks_fts` needs no update: it indexes
/// `header`/`text`/`symbol` only, never `path`. The file's summary (if any)
/// moves the same way — see `move_code_summary_row`.
fn move_code_rows(conn: &Connection, project_id: i64, from: &str, to: &str) -> Result<(), KbError> {
    conn.execute("UPDATE code_files SET path = ?1 WHERE project_id = ?2 AND path = ?3", params![to, project_id, from])?;
    conn.execute("UPDATE code_chunks SET path = ?1 WHERE project_id = ?2 AND path = ?3", params![to, project_id, from])?;
    move_code_summary_row(conn, project_id, from, to)?;
    Ok(())
}

/// Moves a file-level `code_summaries` row (and its FTS entry) along with
/// `move_code_rows`'s file/chunk rows, for a pure rename with unchanged
/// blob. A summary is keyed by path alone (`PRIMARY KEY (project_id,
/// path)`), so without this a renamed file's summary would silently vanish
/// from `code_summary_get`/`search_code_summaries` — still present under
/// the old path, findable by nothing. A no-op when the file had no summary
/// yet: the first `SELECT` (and so the whole three-statement dance) touches
/// zero rows.
///
/// Unlike `code_summary_upsert`/`code_summary_delete` (`store.rs`), this
/// stays in `job.rs` as raw SQL — same rationale as `move_code_rows` itself
/// not living in `store.rs`: narrow, single-caller, and this task keeps new
/// storage surface out of `store.rs`. Deliberately does NOT stale the
/// file's own summary text or its ancestor module paths: the content is
/// unchanged (same blob), only its path moved, and the brief's rename
/// behaviour is "move the row", not "invalidate it" — a rename that also
/// crosses module boundaries is left for the module stage to notice.
fn move_code_summary_row(conn: &Connection, project_id: i64, from: &str, to: &str) -> Result<(), KbError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO code_summaries_fts(code_summaries_fts, rowid, text, path)
         SELECT 'delete', rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, from],
    )?;
    tx.execute("UPDATE code_summaries SET path = ?1 WHERE project_id = ?2 AND path = ?3", params![to, project_id, from])?;
    tx.execute(
        "INSERT INTO code_summaries_fts(rowid, text, path)
         SELECT rowid, text, path FROM code_summaries WHERE project_id = ?1 AND path = ?2",
        params![project_id, to],
    )?;
    tx.commit()?;
    Ok(())
}

/// Ancestor `code_summaries` paths staled when a file is created/changed,
/// deleted, or recategorized out of scope: the first two directory levels
/// only (module-summary depth is capped at 2 for now, see the design doc)
/// plus the repo root `"/"`. For `"a/b/c/x.cpp"` this is `["a/", "a/b/",
/// "/"]` — depth-3 (`"a/b/c/"`) is deliberately excluded. A file with fewer
/// than two directory levels (`"x.cpp"`, `"a/x.cpp"`) yields only the
/// ancestors it actually has, plus `"/"`.
/// `path`'s containing directory, without a trailing slash -- `"a/b/x.rs"`
/// -> `"a/b"`, `"x.rs"` -> `""`. Used only to tell whether a rename crosses
/// a directory boundary (`index_project`'s moved loop); `ancestor_summary_paths`
/// itself is the one that knows about the depth-2 cap and the repo root.
fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

fn ancestor_summary_paths(path: &str) -> Vec<String> {
    let segments: Vec<&str> = path.split('/').collect();
    let dir_segments = &segments[..segments.len().saturating_sub(1)];
    let mut out = Vec::new();
    let mut prefix = String::new();
    for seg in dir_segments.iter().take(2) {
        prefix.push_str(seg);
        prefix.push('/');
        out.push(prefix.clone());
    }
    out.push("/".to_string());
    out
}

// ---------------------------------------------------------------------
// Header prompt / reply grammar.
// ---------------------------------------------------------------------

/// Truncates `file_text` to [`HEADER_TRUNCATE_CHARS`], returning the
/// (possibly truncated) text and whether truncation happened. Shared by
/// `build_header_prompt` and `build_summary_prompt`, which cap a file's
/// text into a prompt the same way.
fn truncate_for_prompt(file_text: &str) -> (String, bool) {
    let char_count = file_text.chars().count();
    if char_count > HEADER_TRUNCATE_CHARS {
        (file_text.chars().take(HEADER_TRUNCATE_CHARS).collect(), true)
    } else {
        (file_text.to_string(), false)
    }
}

/// "Project: <name>\nFile: <path>\n\n<file text, truncated>\n\nChunks:\n
/// #<n> <kind> <symbol-or-dash> lines <a>-<b>\n...\n<instruction>\n
/// <summary instruction>". The reply this asks for carries both the
/// per-chunk headers AND (per `docs/superpowers/specs/*code-index-design*`,
/// task 2) a file summary in one call -- `parse_header_reply` splits the
/// two back apart.
fn build_header_prompt(project: &str, path: &str, file_text: &str, chunks: &[ChunkMeta]) -> String {
    let (text, truncated) = truncate_for_prompt(file_text);

    let mut s = String::new();
    s.push_str(&format!("Project: {}\nFile: {}\n\n", project, path));
    s.push_str(&text);
    if truncated {
        s.push_str(&format!("\n\n[... truncated at {} characters; file continues]", HEADER_TRUNCATE_CHARS));
    }
    s.push_str("\n\nChunks:\n");
    for (i, c) in chunks.iter().enumerate() {
        s.push_str(&format!("#{} {} {} lines {}-{}\n", i + 1, c.kind, c.symbol.as_deref().unwrap_or("-"), c.start_line, c.end_line));
    }
    s.push_str(
        "\nFor each chunk above, reply with exactly one line, in this exact format:\n\
         #<n>: <at most 2 sentences situating this chunk in the file and the project>\n\
         One line per chunk, nothing else. Skip a chunk (omit its line) if you have nothing to say.\n\
         Then a line \"SUMMARY:\" followed by at most 200 words: what this file does, its key types and functions, invariants and gotchas. No chunk numbers in the summary.\n",
    );
    s
}

/// The summary-only prompt for a file that needs a `code_summaries`
/// (file-level) row but not a header call: same project/path preamble and
/// truncation as `build_header_prompt`, no chunk list -- nothing here
/// touches `code_chunks` at all. Reply grammar (the `SUMMARY:` marker) is
/// the same, parsed by `parse_summary_reply`.
fn build_summary_prompt(project: &str, path: &str, file_text: &str) -> String {
    let (text, truncated) = truncate_for_prompt(file_text);

    let mut s = String::new();
    s.push_str(&format!("Project: {}\nFile: {}\n\n", project, path));
    s.push_str(&text);
    if truncated {
        s.push_str(&format!("\n\n[... truncated at {} characters; file continues]", HEADER_TRUNCATE_CHARS));
    }
    s.push_str(
        "\n\nReply with a line \"SUMMARY:\" followed by at most 200 words: what this file does, its key types and functions, invariants and gotchas. No chunk numbers in the summary.\n",
    );
    s
}

/// Parses a combined header+summary reply into `(chunk number -> header
/// text, file summary)`. Header-line grammar unchanged, per the spec
/// ("parser accepts `#n:` / `n:`"): a leading `#` is optional, the number
/// must parse, and an empty header after the `:` is dropped (same as never
/// mentioning that chunk — it keeps `header = NULL`); a garbage line,
/// unparseable number, or missing `:` is skipped.
///
/// The summary is everything from the first line that is a SUMMARY marker
/// (`SUMMARY:`, also markdown-decorated `**SUMMARY:**`, `**SUMMARY**:`,
/// `## SUMMARY:` -- see `summary_marker`) -- any text after the marker on
/// the same line, plus every line after it EXCEPT strict `#<n>: text`
/// header lines (a SUMMARY block placed before the headers still yields
/// its headers), joined with `\n` and trimmed as a whole. A second `SUMMARY:`-looking line further down is ordinary
/// summary text, not a new marker. A trimmed-empty result (`SUMMARY:`
/// alone, nothing after it anywhere) is `None`.
///
/// Also used for the summary-only `index_summary` reply, via
/// `parse_summary_reply` -- that prompt (`build_summary_prompt`) never
/// produces any `#n:` lines, so the headers map just comes back empty
/// there.
fn parse_header_reply(reply: &str) -> (std::collections::HashMap<usize, String>, Option<String>) {
    let mut headers = std::collections::HashMap::new();
    let mut summary_lines: Option<Vec<String>> = None;
    for line in reply.lines() {
        let trimmed = line.trim();
        if let Some(lines) = summary_lines.as_mut() {
            // A SUMMARY block placed BEFORE the header lines (final-review
            // item 12): a strict `#<n>: text` line inside the block is
            // still a header, not summary prose. Only the `#`-prefixed form
            // is taken here, so a summary's own "1: ..." prose survives.
            if let Some((n, text)) = trimmed.strip_prefix('#').and_then(parse_header_line) {
                headers.insert(n, text);
                continue;
            }
            lines.push(line.to_string());
            continue;
        }
        if let Some(after) = summary_marker(trimmed) {
            summary_lines = Some(if after.is_empty() { Vec::new() } else { vec![after.to_string()] });
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let rest = trimmed.strip_prefix('#').unwrap_or(trimmed);
        if let Some((n, text)) = parse_header_line(rest) {
            headers.insert(n, text);
        }
    }
    let summary = summary_lines.map(|lines| lines.join("\n").trim().to_string()).filter(|s| !s.is_empty());
    (headers, summary)
}

/// `<n>: <text>` (the part after an optional `#`) -> `(n, text)`; `None` for
/// an unparseable number, a missing `:`, or empty text.
fn parse_header_line(rest: &str) -> Option<(usize, String)> {
    let (num_str, text) = rest.split_once(':')?;
    let n = num_str.trim().parse::<usize>().ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some((n, text.to_string()))
}

/// If `trimmed` is a SUMMARY marker line, what follows the marker on that
/// line. Tolerates markdown decoration models add (final-review item 12):
/// `SUMMARY:`, `**SUMMARY:**`, `**SUMMARY**:`, `## SUMMARY:`.
fn summary_marker(trimmed: &str) -> Option<&str> {
    let t = trimmed.trim_start_matches(|c: char| c == '#' || c == '*' || c == ' ');
    let r = t.strip_prefix("SUMMARY")?;
    let r = r.trim_start_matches('*').strip_prefix(':')?;
    Some(r.trim_start_matches('*').trim())
}

/// The summary-only reply's grammar is the summary half of
/// `parse_header_reply`'s (see there for the `SUMMARY:` marker rules) --
/// this just discards the (always-empty, for that prompt) headers map.
fn parse_summary_reply(reply: &str) -> Option<String> {
    parse_header_reply(reply).1
}

/// `header + "\n" + first line of text + "\n" + text`, truncated to
/// `transcripts::CHUNK_CHARS` — the same cap `cmd_index_transcripts`
/// applies to a transcript passage before embedding it (see
/// `transcripts::chunk_turns`, which groups passages to roughly this
/// size), reused here rather than inventing a second embedder-input-size
/// constant.
fn embedding_input(header: Option<&str>, text: &str) -> String {
    let first_line = text.lines().next().unwrap_or("");
    let combined = format!("{}\n{}\n{}", header.unwrap_or(""), first_line, text);
    if combined.chars().count() > crate::transcripts::CHUNK_CHARS {
        combined.chars().take(crate::transcripts::CHUNK_CHARS).collect()
    } else {
        combined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    // -- fixture repo (replicated minimally from git.rs's/scope.rs's private
    // `FixtureRepo` -- neither is reachable from here, and the brief scopes
    // this task to job.rs/cli.rs only) ------------------------------------

    struct FixtureRepo {
        path: PathBuf,
    }

    impl FixtureRepo {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mach-kb-job-test-{}-{}-{}", std::process::id(), tag, n));
            std::fs::create_dir_all(&path).unwrap();
            let repo = FixtureRepo { path };
            repo.git(&["init", "--quiet"]);
            repo
        }

        fn git(&self, args: &[&str]) {
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        }

        fn write(&self, path: &str, content: &[u8]) {
            let full = self.path.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, content).unwrap();
        }

        fn rm(&self, path: &str) {
            std::fs::remove_file(self.path.join(path)).unwrap();
        }

        fn mv(&self, from: &str, to: &str) {
            if let Some(parent) = self.path.join(to).parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::rename(self.path.join(from), self.path.join(to)).unwrap();
        }

        fn commit(&self, msg: &str) {
            self.git(&["add", "-A"]);
            self.git(&["commit", "--quiet", "--no-gpg-sign", "-m", msg]);
        }

        fn repo(&self) -> Repo {
            Repo::new(self.path.clone())
        }
    }

    impl Drop for FixtureRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn mem_conn() -> Connection {
        store::open_with_path(Path::new(":memory:")).expect("open in-memory store")
    }

    fn new_project(conn: &Connection, name: &str, root: &Path) -> ProjectRow {
        store::upsert_project(conn, &format!("fp-{name}"), name, &root.to_string_lossy(), "2026-09-23T00:00:00Z").unwrap();
        store::get_project_by_name(conn, name).unwrap().unwrap()
    }

    // -- fakes ---------------------------------------------------------

    /// A `sonnet` prompt's module path, if it's one of
    /// `summary::build_module_prompt`'s own ("Project: ..\nModule: ..") --
    /// `None` for a real `scope::build_scope_prompt` call ("You are scoping
    /// a code repository..."), which is also `model = "sonnet"`. Shared by
    /// every fake below that needs to tell the two apart.
    fn module_prompt_dir(prompt: &str) -> Option<String> {
        if !prompt.starts_with("Project: ") {
            return None;
        }
        prompt.lines().find_map(|l| l.strip_prefix("Module: ")).map(|s| s.to_string())
    }

    /// Whether a `sonnet` prompt is `history::build_history_prompt`'s own
    /// (its instruction always opens "Summarise what changed in ") --
    /// distinguishes it from a module/repo summary or a scope call, the
    /// other two `model = "sonnet"` shapes. Every fake below that already
    /// special-cases `module_prompt_dir` needs this too: otherwise it falls
    /// into the "must be a scope call" branch and (for `FakeLlm`) returns
    /// an empty reply, which `history::run_history_pass` treats as "no
    /// summary written" -- leaving `code_history` empty after a "succeeds"
    /// run and making every later "second run makes zero further calls"
    /// test (run against `PanicLlm`) see history retried, and panic.
    fn is_history_prompt(prompt: &str) -> bool {
        prompt.starts_with("Summarise what changed in ")
    }

    /// Always succeeds. Scope calls get an empty reply (every directory
    /// defaults to `product`, per `run_scope_pass`'s own default). Header
    /// calls are answered by echoing `#<n>: header <n>` for every
    /// `#<n> <kind> ...` chunk-list line the prompt actually contains, so
    /// this fake never needs to know a fixture's exact chunk count, PLUS a
    /// trailing `SUMMARY:` line -- the "everything succeeds" happy path
    /// this fake represents means the combined call always produces a
    /// summary too, so a plain `FakeLlm::default()` run never needs a
    /// separate summary-only call (every file already has a
    /// matching-digest summary the moment its combined call succeeds).
    /// Tests that specifically want a combined call to leave the summary
    /// missing (to exercise the summary-only fallback) use
    /// `HeaderOnlyThenSummaryLlm` instead. Every haiku call is logged
    /// (model, and the `File:` path) so tests can assert exactly which
    /// files got a haiku call this run.
    #[derive(Default)]
    struct FakeLlm {
        fail_paths: RefCell<HashSet<String>>,
        header_calls: RefCell<Vec<String>>,
        scope_calls: RefCell<usize>,
        prompts: RefCell<Vec<String>>,
        fail_scope: bool,
    }

    impl FakeLlm {
        fn failing(paths: &[&str]) -> Self {
            FakeLlm { fail_paths: RefCell::new(paths.iter().map(|s| s.to_string()).collect()), ..Default::default() }
        }

        fn header_calls(&self) -> Vec<String> {
            self.header_calls.borrow().clone()
        }
    }

    impl ReflectLlm for FakeLlm {
        fn call(&self, model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            self.prompts.borrow_mut().push(prompt.to_string());
            if model == "sonnet" {
                if let Some(dir) = module_prompt_dir(prompt) {
                    // A module/repo summary call (summary::try_regenerate_one),
                    // also `model = "sonnet"` -- told apart from a real scope
                    // call (`scope::run_scope_pass`, whose prompt always opens
                    // "You are scoping...") by `build_module_prompt`'s own
                    // "Project: .. \nModule: .." preamble. Always succeeds with
                    // a non-empty, distinct reply so a module/repo summary is
                    // actually written and stops being a stale-or-missing
                    // candidate -- otherwise every "second run makes zero
                    // calls" test below would see it retried forever.
                    return Ok(format!("fake module summary for {}", dir));
                }
                if is_history_prompt(prompt) {
                    // Always succeeds with a distinct, non-empty reply --
                    // same "must actually write something, or every later
                    // month/run keeps retrying it" reasoning as the
                    // module/repo branch above.
                    return Ok("fake history summary".to_string());
                }
                *self.scope_calls.borrow_mut() += 1;
                if self.fail_scope {
                    return Err("simulated scope failure".to_string());
                }
                return Ok(String::new());
            }
            let path = prompt.lines().find_map(|l| l.strip_prefix("File: ")).unwrap_or("").to_string();
            self.header_calls.borrow_mut().push(path.clone());
            if self.fail_paths.borrow().contains(&path) {
                return Err("simulated llm failure".to_string());
            }
            let mut out = String::new();
            for line in prompt.lines() {
                if let Some(rest) = line.strip_prefix('#') {
                    if let Some((num, _)) = rest.split_once(' ') {
                        if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                            out.push_str(&format!("#{}: header for chunk {}\n", num, num));
                        }
                    }
                }
            }
            out.push_str(&format!("SUMMARY: fake summary for {}\n", path));
            Ok(out)
        }
    }

    struct PanicLlm;
    impl ReflectLlm for PanicLlm {
        fn call(&self, model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            panic!("no llm call was expected, but one was made (model={model})");
        }
    }

    /// Like `PanicLlm`, except a history call (`is_history_prompt`) always
    /// succeeds instead of panicking. A commit that only renames a file (no
    /// content, header, or summary work needed) still adds a new commit sha
    /// to its month, which legitimately changes that month's digest -- so a
    /// real run makes exactly one `index_history` call even when nothing
    /// else does. Tests asserting "no HEADER/SUMMARY/SCOPE call happens"
    /// after such a commit use this instead of bare `PanicLlm`.
    struct PanicUnlessHistoryLlm;
    impl ReflectLlm for PanicUnlessHistoryLlm {
        fn call(&self, model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            if model == "sonnet" && is_history_prompt(prompt) {
                return Ok("fake history summary".to_string());
            }
            panic!("no llm call other than history was expected, but one was made (model={model})");
        }
    }

    /// Always succeeds, but -- unlike `FakeLlm` -- a combined (`index_header`)
    /// call NEVER includes a `SUMMARY:` line, only headers: exactly what a
    /// real flaky/incomplete reply, or a pre-summary-feature binary, would
    /// leave behind. A summary-only (`index_summary`) call, told apart from
    /// a combined one by the absence of `build_header_prompt`'s "Chunks:"
    /// section, always succeeds with a fixed summary text. Lets tests
    /// exercise "missing summary queues (and finishes) a summary-only call"
    /// without depending on `FakeLlm`'s own always-summarize default.
    #[derive(Default)]
    struct HeaderOnlyThenSummaryLlm {
        header_calls: RefCell<Vec<String>>,
        summary_calls: RefCell<Vec<String>>,
    }

    impl ReflectLlm for HeaderOnlyThenSummaryLlm {
        fn call(&self, model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            if model == "sonnet" {
                if let Some(dir) = module_prompt_dir(prompt) {
                    return Ok(format!("fake module summary for {}", dir));
                }
                if is_history_prompt(prompt) {
                    return Ok("fake history summary".to_string());
                }
                return Ok(String::new());
            }
            let path = prompt.lines().find_map(|l| l.strip_prefix("File: ")).unwrap_or("").to_string();
            if prompt.contains("\nChunks:\n") {
                self.header_calls.borrow_mut().push(path.clone());
                let mut out = String::new();
                for line in prompt.lines() {
                    if let Some(rest) = line.strip_prefix('#') {
                        if let Some((num, _)) = rest.split_once(' ') {
                            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                                out.push_str(&format!("#{}: header for chunk {}\n", num, num));
                            }
                        }
                    }
                }
                Ok(out)
            } else {
                self.summary_calls.borrow_mut().push(path.clone());
                Ok(format!("SUMMARY: fallback summary for {}\n", path))
            }
        }
    }

    /// Always finalizes headers but NEVER produces a parseable summary --
    /// neither the combined call nor the dedicated summary-only call ever
    /// includes a usable `SUMMARY:` line. Module/repo (`sonnet`) calls
    /// succeed normally, out of the way of this fake's own purpose: giving
    /// fix-list item 1's give-up accounting something to count.
    #[derive(Default)]
    struct NeverSummarizesLlm {
        header_calls: RefCell<Vec<String>>,
        summary_calls: RefCell<Vec<String>>,
    }

    impl ReflectLlm for NeverSummarizesLlm {
        fn call(&self, model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            if model == "sonnet" {
                if let Some(dir) = module_prompt_dir(prompt) {
                    return Ok(format!("fake module summary for {}", dir));
                }
                if is_history_prompt(prompt) {
                    return Ok("fake history summary".to_string());
                }
                return Ok(String::new());
            }
            let path = prompt.lines().find_map(|l| l.strip_prefix("File: ")).unwrap_or("").to_string();
            if prompt.contains("\nChunks:\n") {
                self.header_calls.borrow_mut().push(path.clone());
                let mut out = String::new();
                for line in prompt.lines() {
                    if let Some(rest) = line.strip_prefix('#') {
                        if let Some((num, _)) = rest.split_once(' ') {
                            if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()) {
                                out.push_str(&format!("#{}: header for chunk {}\n", num, num));
                            }
                        }
                    }
                }
                Ok(out) // headers only, never a SUMMARY: line
            } else {
                self.summary_calls.borrow_mut().push(path.clone());
                Ok("nothing useful to say here".to_string()) // no SUMMARY: marker at all
            }
        }
    }

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().take(4).map(|b| b as f32).collect())
        }
    }

    struct FailingEmbedder;
    impl Embedder for FailingEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, KbError> {
            Err(KbError::Embed("no embedder".to_string()))
        }
    }

    fn opts() -> IndexOptions {
        IndexOptions::new()
    }

    // -- run_index -------------------------------------------------------

    #[test]
    fn first_run_indexes_all_in_scope_files_and_advances_head() {
        let fx = FixtureRepo::new("first-run");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let expected_head = fx.repo().head().unwrap();

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;

        let report = run_index(&conn, &llm, &embedder, &opts()).unwrap();
        assert!(!report.budget_reached);
        assert_eq!(report.projects.len(), 1);
        let outcome = &report.projects[0];
        assert_eq!(outcome.files_chunked, 2, "{:?}", outcome);
        assert!(outcome.head_advanced);

        let reloaded_head = code_indexed_head_of(&conn, project.id).unwrap();
        assert_eq!(reloaded_head.as_deref(), Some(expected_head.as_str()));

        for path in ["src/a.rs", "src/b.rs"] {
            let row = store::code_file_get(&conn, project.id, path).unwrap().unwrap();
            assert_eq!(row.status, "indexed", "{}: {:?}", path, row);
        }

        let (chunks, embedded) = count_chunks(&conn, project.id).unwrap();
        assert!(chunks >= 2, "expected at least one chunk per file, got {}", chunks);
        assert_eq!(chunks, embedded, "every chunk should have been embedded");
    }

    #[test]
    fn second_run_with_no_changes_makes_zero_calls() {
        let fx = FixtureRepo::new("no-changes");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        run_index(&conn, &llm, &embedder, &opts()).unwrap();

        // A second run must not call the LLM at all -- PanicLlm blows up
        // the test immediately if it tries.
        let report2 = run_index(&conn, &PanicLlm, &embedder, &opts()).unwrap();
        assert_eq!(report2.calls_used, 0);
        assert_eq!(report2.projects[0].files_chunked, 0);
        assert!(!report2.projects[0].scope_ran);
    }

    #[test]
    fn modifying_one_file_only_rechunks_and_reheads_that_file() {
        let fx = FixtureRepo::new("modify-one");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        run_index(&conn, &llm, &embedder, &opts()).unwrap();
        let b_row_before = store::code_file_get(&conn, project.id, "src/b.rs").unwrap().unwrap();

        fx.write("src/a.rs", b"fn a() { let x = 1; }\n");
        fx.commit("modify a");

        let llm2 = FakeLlm::default();
        let outcome = run_index(&conn, &llm2, &embedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.files_chunked, 1, "{:?}", outcome);
        assert_eq!(llm2.header_calls(), vec!["src/a.rs".to_string()]);
        assert!(!outcome.scope_ran, "top-level directory set didn't move, no re-scope needed");

        let b_row_after = store::code_file_get(&conn, project.id, "src/b.rs").unwrap().unwrap();
        assert_eq!(b_row_before, b_row_after, "b.rs must be untouched by a.rs's change");
    }

    #[test]
    fn deleting_a_file_removes_its_rows() {
        let fx = FixtureRepo::new("delete");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        run_index(&conn, &llm, &embedder, &opts()).unwrap();

        fx.rm("src/b.rs");
        fx.commit("delete b");
        let outcome = run_index(&conn, &FakeLlm::default(), &embedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.files_deleted, 1);

        assert!(store::code_file_get(&conn, project.id, "src/b.rs").unwrap().is_none());
        let (chunks, _) = count_chunks(&conn, project.id).unwrap();
        let remaining_for_b: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_chunks WHERE project_id = ?1 AND path = 'src/b.rs'",
                params![project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining_for_b, 0);
        assert!(chunks > 0, "a.rs's chunks must still be present");
    }

    #[test]
    fn renaming_a_file_with_unchanged_blob_moves_rows_without_a_header_call() {
        let fx = FixtureRepo::new("rename");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let embedder = FakeEmbedder;
        run_index(&conn, &FakeLlm::default(), &embedder, &opts()).unwrap();
        let before = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        let chunk_id_before: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE project_id = ?1 AND path = 'src/a.rs' LIMIT 1",
                params![project.id],
                |r| r.get(0),
            )
            .unwrap();

        fx.mv("src/a.rs", "src/renamed.rs");
        fx.commit("rename a");

        let llm2 = FakeLlm::default();
        let outcome = run_index(&conn, &llm2, &embedder, &opts()).unwrap().projects.remove(0);
        assert!(llm2.header_calls().is_empty(), "a pure content-unchanged rename needs no header call");
        assert!(outcome.head_advanced);

        assert!(store::code_file_get(&conn, project.id, "src/a.rs").unwrap().is_none());
        let after = store::code_file_get(&conn, project.id, "src/renamed.rs").unwrap().unwrap();
        assert_eq!(after.status, before.status, "status (indexed) survives the rename");

        let chunk_id_after: i64 = conn
            .query_row(
                "SELECT id FROM code_chunks WHERE project_id = ?1 AND path = 'src/renamed.rs' LIMIT 1",
                params![project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(chunk_id_before, chunk_id_after, "the same chunk row moved, it wasn't replaced");
    }

    #[test]
    fn llm_failure_leaves_file_dirty_head_not_advanced_and_next_run_completes_it() {
        let fx = FixtureRepo::new("llm-fail");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let embedder = FakeEmbedder;
        let llm = FakeLlm::failing(&["src/a.rs"]);
        let outcome = run_index(&conn, &llm, &embedder, &opts()).unwrap().projects.remove(0);
        assert!(!outcome.head_advanced);

        let a_row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(a_row.status, "dirty");
        let b_row = store::code_file_get(&conn, project.id, "src/b.rs").unwrap().unwrap();
        assert_eq!(b_row.status, "indexed");
        assert!(code_indexed_head_of(&conn, project.id).unwrap().is_none());

        // Next run: only a.rs needs a header call (b.rs's blob is
        // unchanged and already indexed, so it's not re-chunked).
        let llm2 = FakeLlm::default();
        let outcome2 = run_index(&conn, &llm2, &embedder, &opts()).unwrap().projects.remove(0);
        assert!(outcome2.head_advanced);
        assert_eq!(llm2.header_calls(), vec!["src/a.rs".to_string()]);
        let a_row2 = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(a_row2.status, "indexed");
    }

    #[test]
    fn failed_header_calls_are_counted_separately_from_attempts() {
        let fx = FixtureRepo::new("llm-fail-count");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        let report = run_index(&conn, &FakeLlm::failing(&["src/a.rs"]), &FakeEmbedder, &opts()).unwrap();
        let outcome = &report.projects[0];
        assert_eq!((outcome.headers_attempted, outcome.headers_failed), (2, 1));
        assert_eq!(report.calls_failed, 1);
        assert_eq!(report.last_error.as_deref(), Some("simulated llm failure"));
        assert!(!report.stopped_on_failures, "one failure among successes must not stop the run");
    }

    #[test]
    fn consecutive_failures_stop_the_run_before_the_budget_is_spent() {
        let fx = FixtureRepo::new("llm-outage");
        let names: Vec<String> = (0..MAX_CONSECUTIVE_FAILURES + 3).map(|i| format!("src/f{}.rs", i)).collect();
        for (i, n) in names.iter().enumerate() {
            fx.write(n, format!("fn f{}() {{}}\n", i).as_bytes());
        }
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let failing: Vec<&str> = names.iter().map(String::as_str).collect();
        let llm = FakeLlm::failing(&failing);
        let report = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();

        assert!(report.stopped_on_failures);
        assert!(!report.budget_reached);
        assert_eq!(llm.header_calls().len(), MAX_CONSECUTIVE_FAILURES, "no call after the breaker trips");
        assert_eq!(report.projects[0].headers_failed, MAX_CONSECUTIVE_FAILURES);
        assert_eq!(report.projects[0].months_summarized, 0, "later stages stop too");
        assert_eq!(store::code_files_with_status(&conn, project.id, "dirty").unwrap().len(), names.len());
    }

    #[test]
    fn tool_call_narration_is_recognised_but_prose_is_not() {
        assert!(is_tool_transcript("Let me check.\n\n**Tool: glob**\n\nParameters:\n```json\n{}\n```"));
        assert!(is_tool_transcript("<function_calls>\n<invoke name=\"Bash\">"));
        assert!(!is_tool_transcript("**Purpose:** helios-unlit is a reference plugin.\nIt uses a tool: glslc."));
    }

    #[test]
    fn a_success_resets_the_consecutive_failure_count() {
        let mut budget = Budget::new(100);
        for _ in 0..MAX_CONSECUTIVE_FAILURES - 1 {
            budget.record(Some("timeout"));
        }
        budget.record(None);
        for _ in 0..MAX_CONSECUTIVE_FAILURES - 1 {
            budget.record(Some("timeout"));
        }
        assert!(budget.remaining());
        budget.record(Some("timeout"));
        assert!(!budget.remaining());
        assert_eq!(budget.failed(), 2 * MAX_CONSECUTIVE_FAILURES - 1);
    }

    #[test]
    fn path_prefix_run_leaves_dirty_files_outside_the_prefix_alone() {
        let fx = FixtureRepo::new("prefix-backlog");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("lib/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        // First run: both files chunked, both header calls fail -> both dirty.
        run_index(&conn, &FakeLlm::failing(&["src/a.rs", "lib/b.rs"]), &FakeEmbedder, &opts()).unwrap();

        let llm = FakeLlm::default();
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("src".to_string());
        run_index(&conn, &llm, &FakeEmbedder, &o).unwrap();
        assert_eq!(llm.header_calls(), vec!["src/a.rs".to_string()]);
        assert_eq!(store::code_file_get(&conn, project.id, "lib/b.rs").unwrap().unwrap().status, "dirty");
    }

    #[test]
    fn budget_stop_and_resume() {
        let fx = FixtureRepo::new("budget");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        let embedder = FakeEmbedder;
        // Budget 1: the scope pass alone spends it (first run, no scope
        // decisions yet), so no header call happens at all this run --
        // both files stay dirty.
        let mut o = opts();
        o.budget = 1;
        let report = run_index(&conn, &FakeLlm::default(), &embedder, &o).unwrap();
        assert!(report.budget_reached);
        assert_eq!(report.calls_used, 1);
        assert!(!report.projects[0].head_advanced);

        // Resume with a real budget: finishes both files, no re-scope
        // (already decided last run).
        let llm2 = FakeLlm::default();
        let report2 = run_index(&conn, &llm2, &embedder, &opts()).unwrap();
        assert!(!report2.budget_reached);
        assert!(report2.projects[0].head_advanced);
        assert_eq!(llm2.header_calls().len(), 2);
    }

    #[test]
    fn path_prefix_leaves_head_unchanged() {
        let fx = FixtureRepo::new("prefix");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let embedder = FakeEmbedder;
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("src".to_string());
        let outcome = run_index(&conn, &FakeLlm::default(), &embedder, &o).unwrap().projects.remove(0);

        assert_eq!(outcome.files_chunked, 1);
        assert!(!outcome.head_advanced, "a --path-prefix run must never advance code_indexed_head");
        assert!(code_indexed_head_of(&conn, project.id).unwrap().is_none());
    }

    #[test]
    fn path_prefix_skips_the_history_stage_entirely() {
        let fx = FixtureRepo::new("history-prefix-skip");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("src".to_string());
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap().projects.remove(0);

        assert_eq!(outcome.months_summarized, 0, "a --path-prefix run must skip the history stage, not just fail to write anything");
        assert!(store::code_history_list(&conn, project.id).unwrap().is_empty());
    }

    #[test]
    fn a_normal_run_reaches_the_history_stage_when_no_path_prefix_is_given() {
        let fx = FixtureRepo::new("history-runs-normally");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap().projects.remove(0);

        assert!(outcome.months_summarized >= 1, "the current month must be attempted when the repo has commits and no --path-prefix is set: {:?}", outcome);
    }

    #[test]
    fn path_prefix_with_a_trailing_slash_matches_the_same_files_as_without_one() {
        // Regression: `format!("{}/", prefix)` used to become
        // "helios-picking//" when the caller's own prefix already ended in
        // '/', which no real tracked path starts with -- every file was
        // silently filtered out, with no error, "0 files matched"-looking
        // output. Both spellings must now chunk identically.
        let fx = FixtureRepo::new("prefix-slash");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("other/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj-noslash", &fx.path);
        let mut o = opts();
        o.project = Some("proj-noslash".to_string());
        o.path_prefix = Some("src".to_string());
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap().projects.remove(0);
        assert_eq!(outcome.files_chunked, 1);

        let conn2 = mem_conn();
        new_project(&conn2, "proj-slash", &fx.path);
        let mut o2 = opts();
        o2.project = Some("proj-slash".to_string());
        o2.path_prefix = Some("src/".to_string());
        let outcome2 = run_index(&conn2, &FakeLlm::default(), &FakeEmbedder, &o2).unwrap().projects.remove(0);
        assert_eq!(outcome2.files_chunked, 1, "trailing slash on --path-prefix must match the same file as no trailing slash");
    }

    #[test]
    fn path_prefix_matching_zero_tracked_files_is_a_loud_error() {
        let fx = FixtureRepo::new("prefix-zero");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("does-not-exist".to_string());
        let err = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("does-not-exist"), "error should name the offending prefix: {msg}");
        assert!(msg.contains("no tracked files") || msg.contains("no matching"), "error should say why: {msg}");
    }

    #[test]
    fn path_prefix_matching_tracked_files_with_zero_changes_is_not_an_error() {
        // The zero-match error must fire only when the prefix matches no
        // TRACKED file, never when the prefix is valid but there is simply
        // nothing new to do (e.g. a re-run after everything under it is
        // already indexed).
        let fx = FixtureRepo::new("prefix-noop");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("src".to_string());
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap();

        // Second run: nothing changed under src/, must not error and must
        // make zero further calls.
        let report = run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap();
        assert_eq!(report.calls_used, 0);
        assert!(!report.projects[0].head_advanced);
        let _ = project;
    }

    #[test]
    fn dry_run_changes_nothing() {
        let fx = FixtureRepo::new("dry-run");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.dry_run = true;
        let plans = plan_index(&conn, &o).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].files_to_index, 2);
        assert!(plans[0].needs_scope);
        assert_eq!(plans[0].months_to_summarize, 1, "the one commit month has no history row yet");
        assert_eq!(plans[0].calls_needed, 1 /* scope */ + 2 /* files */ + 1 /* month */);

        assert!(store::code_scope_get(&conn, project.id).unwrap().is_empty());
        assert!(store::code_files_with_status(&conn, project.id, "indexed").unwrap().is_empty());
        assert!(store::code_files_with_status(&conn, project.id, "dirty").unwrap().is_empty());
        let (chunks, _) = count_chunks(&conn, project.id).unwrap();
        assert_eq!(chunks, 0);
    }

    #[test]
    fn a_scope_recategorization_to_vendored_deletes_its_files_chunks_on_the_next_run_and_writes_the_memory() {
        let fx = FixtureRepo::new("recategorize-vendored");
        fx.write("vendor/lib.cpp", b"int lib() { return 1; }\n");
        fx.write("src/a.cpp", b"int a() { return 1; }\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);

        // First run: FakeLlm's scope pass returns an empty reply, so
        // every directory (including "vendor") defaults to product --
        // both files get indexed.
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(store::code_file_get(&conn, project.id, "vendor/lib.cpp").unwrap().is_some());
        assert!(store::code_file_get(&conn, project.id, "src/a.cpp").unwrap().is_some());
        assert!(!code_chunks_for_file(&conn, project.id, "vendor/lib.cpp").unwrap().is_empty());

        // Simulate `mach kb index scope proj --set vendor=vendored`.
        store::code_scope_set(&conn, project.id, "vendor", "vendored", "user", &store::now_rfc3339()).unwrap();

        // Second run: nothing changed in git (head unchanged), and the
        // directory set is unchanged too, so no scope pass is needed and no
        // file needs re-chunking or re-summarising -- the cleanup itself
        // costs zero LLM calls. It DOES leave "vendor/"'s module summary
        // (written by the first run's module stage) with no indexed files
        // any more, which orphan-prunes it for free, and leaves the repo
        // summary's own child set changed (vendor/ is no longer one of its
        // depth-1 modules), which is a legitimate reason for the module
        // stage to regenerate "/" -- so `FakeLlm`, not `PanicLlm`, this
        // time; what's asserted directly is that no header/summary-only
        // call happened.
        let report = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report.projects[0].headers_attempted, 0, "no file needs re-chunking");
        assert_eq!(report.projects[0].summaries_attempted, 0, "no file needs re-summarising");

        assert!(
            store::code_file_get(&conn, project.id, "vendor/lib.cpp").unwrap().is_none(),
            "the recategorized file's code_files row must be gone"
        );
        assert!(
            code_chunks_for_file(&conn, project.id, "vendor/lib.cpp").unwrap().is_empty(),
            "and its chunks with it"
        );
        assert!(
            store::code_file_get(&conn, project.id, "src/a.cpp").unwrap().is_some(),
            "a file under an untouched directory must be left alone"
        );
        assert!(
            store::code_summary_get(&conn, project.id, "vendor/").unwrap().is_none(),
            "vendor/'s module summary is pruned once it has no indexed files left"
        );

        let source = format!("code-index:{}:vendored:vendor", project.name);
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "a --set-driven vendored recategorization must still write the vendored memory");
        let content: String =
            conn.query_row("SELECT content FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert!(content.contains("vendor"), "{}", content);

        // Third run: re-running again must not duplicate the memory.
        run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        let count2: i64 =
            conn.query_row("SELECT COUNT(*) FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(count2, 1);
    }

    #[test]
    fn a_scope_recategorization_to_generated_deletes_files_without_writing_a_vendored_memory() {
        let fx = FixtureRepo::new("recategorize-generated");
        fx.write("gen/codegen.cpp", b"int g() { return 1; }\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(store::code_file_get(&conn, project.id, "gen/codegen.cpp").unwrap().is_some());

        store::code_scope_set(&conn, project.id, "gen", "generated", "user", &store::now_rfc3339()).unwrap();
        run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();

        assert!(store::code_file_get(&conn, project.id, "gen/codegen.cpp").unwrap().is_none());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE source LIKE 'code-index:%vendored%'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "a non-vendored recategorization must not write a vendored memory");
    }

    #[test]
    fn a_project_whose_root_is_not_a_git_repo_is_reported_skipped() {
        let dir = std::env::temp_dir().join(format!("mach-kb-job-test-notgit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let conn = mem_conn();
        new_project(&conn, "notgit", &dir);
        let report = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report.projects.len(), 1);
        assert_eq!(report.projects[0].skipped_reason.as_deref(), Some("not a git repo"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn embedder_failure_leaves_embedding_null_but_still_finalizes_the_file() {
        // The reachability ping succeeds, the per-chunk embeds fail: the
        // file is still finalized, its embeddings left NULL for backfill.
        let fx = FixtureRepo::new("embed-fail");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let outcome = run_index(&conn, &FakeLlm::default(), &PingOnlyEmbedder, &opts()).unwrap().projects.remove(0);
        assert!(outcome.head_advanced);

        let row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(row.status, "indexed");
        let (chunks, embedded) = count_chunks(&conn, project.id).unwrap();
        assert!(chunks > 0);
        assert_eq!(embedded, 0);
    }

    #[test]
    fn an_unreachable_embedder_skips_header_calls_so_none_are_wasted() {
        // Item 9: pre-check once per run; embedder down => no header calls.
        let fx = FixtureRepo::new("embed-down");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let report = run_index(&conn, &llm, &FailingEmbedder, &opts()).unwrap();
        assert!(!report.embedder_up);
        assert!(llm.header_calls().is_empty());
        assert!(!report.projects[0].head_advanced);
        assert_eq!(store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap().status, "dirty");

        let llm2 = FakeLlm::default();
        assert!(run_index(&conn, &llm2, &FakeEmbedder, &opts()).unwrap().projects[0].head_advanced);
        assert_eq!(llm2.header_calls(), vec!["src/a.rs".to_string()]);
    }

    // -- final-review fixes --------------------------------------------------

    fn indexed_rows(conn: &Connection, project_id: i64) -> Vec<String> {
        store::code_files_with_status(conn, project_id, "indexed").unwrap().into_iter().map(|r| r.path).collect()
    }

    #[test]
    fn three_changed_files_with_budget_one_finish_in_three_runs_and_no_file_is_reheaded_twice() {
        // Item 1 regression: with code_indexed_head set, the old code took
        // Added/Modified from `git diff prev_head HEAD` on every run until
        // the head advanced -- so run 2 re-chunked (re-dirtied) a.rs, which
        // run 1 had just finished, and the budget-1 loop re-headed a.rs
        // forever.
        let fx = FixtureRepo::new("budget-one-loop");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.write("src/c.rs", b"fn c() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        assert!(run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap().projects[0].head_advanced);

        fx.write("src/a.rs", b"fn a() { 1; }\n");
        fx.write("src/b.rs", b"fn b() { 2; }\n");
        fx.write("src/c.rs", b"fn c() { 3; }\n");
        fx.commit("change all three");
        let new_head = fx.repo().head().unwrap();

        let llm = FakeLlm::default();
        let mut o = opts();
        o.budget = 1;
        for _ in 0..3 {
            run_index(&conn, &llm, &FakeEmbedder, &o).unwrap();
        }
        let mut calls = llm.header_calls();
        calls.sort();
        assert_eq!(calls, vec!["src/a.rs", "src/b.rs", "src/c.rs"], "each file headed exactly once");
        assert_eq!(indexed_rows(&conn, project.id).len(), 3);
        assert_eq!(code_indexed_head_of(&conn, project.id).unwrap().as_deref(), Some(new_head.as_str()));
    }

    #[test]
    fn a_branch_switch_sized_change_completes_over_several_budgeted_runs() {
        let fx = FixtureRepo::new("branch-switch");
        for i in 0..12 {
            fx.write(&format!("src/f{i:02}.rs"), format!("fn f{i}() {{}}\n").as_bytes());
        }
        fx.commit("base");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        for i in 0..10 {
            fx.write(&format!("src/f{i:02}.rs"), format!("fn f{i}() {{ let other_branch = {i}; }}\n").as_bytes());
        }
        fx.rm("src/f11.rs");
        for i in 0..3 {
            fx.write(&format!("src/new{i}.rs"), format!("fn new{i}() {{}}\n").as_bytes());
        }
        fx.commit("other branch");

        let llm = FakeLlm::default();
        let mut o = opts();
        o.budget = 4;
        let mut runs = 0;
        loop {
            runs += 1;
            assert!(runs <= 4, "13 header calls at 4/run must finish within 4 runs");
            if run_index(&conn, &llm, &FakeEmbedder, &o).unwrap().projects[0].head_advanced {
                break;
            }
        }
        let calls = llm.header_calls();
        let unique: HashSet<&String> = calls.iter().collect();
        assert_eq!(calls.len(), 13, "{:?}", calls);
        assert_eq!(unique.len(), 13, "no file re-headed: {:?}", calls);
        assert!(store::code_file_get(&conn, project.id, "src/f11.rs").unwrap().is_none());
        assert_eq!(indexed_rows(&conn, project.id).len(), 14);
    }

    #[test]
    fn a_staged_but_uncommitted_change_is_invisible_to_the_index() {
        // Item 8: the file list comes from the committed tree, so a staged
        // edit can't make a file's recorded blob disagree with HEAD.
        let fx = FixtureRepo::new("staged-only");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        fx.write("src/a.rs", b"fn a() { staged(); }\n");
        fx.git(&["add", "-A"]);
        let report = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report.calls_used, 0);
        assert_eq!(report.projects[0].files_chunked, 0);
    }

    #[test]
    fn one_projects_error_is_recorded_and_the_run_continues_with_the_next() {
        // Item 3. An initialized repo with no commit yet: `is_repo` is true,
        // `head()` fails -- the old code aborted the whole run with `?`.
        let broken = FixtureRepo::new("iso-broken");
        let good = FixtureRepo::new("iso-good");
        good.write("src/a.rs", b"fn a() {}\n");
        good.commit("first");

        let conn = mem_conn();
        new_project(&conn, "broken", &broken.path);
        let good_project = new_project(&conn, "good", &good.path);
        let report = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let b = report.projects.iter().find(|p| p.project == "broken").unwrap();
        let g = report.projects.iter().find(|p| p.project == "good").unwrap();
        assert!(b.error.is_some(), "{:?}", b);
        assert!(g.error.is_none() && g.head_advanced, "{:?}", g);
        assert_eq!(indexed_rows(&conn, good_project.id), vec!["src/a.rs".to_string()]);
    }

    #[test]
    fn a_prev_head_that_is_not_a_commit_falls_back_to_blob_compare() {
        let fx = FixtureRepo::new("bogus-prev-head");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        // e.g. history rewritten + gc'd: the stored sha no longer exists.
        store::set_code_indexed_head(&conn, project.id, &"d".repeat(40), &store::now_rfc3339()).unwrap();
        fx.write("src/a.rs", b"fn a() { changed(); }\n");
        fx.commit("change a");

        let llm = FakeLlm::default();
        let report = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        let o = &report.projects[0];
        assert!(o.error.is_none(), "{:?}", o);
        assert!(o.head_advanced);
        assert_eq!(llm.header_calls(), vec!["src/a.rs".to_string()], "b.rs's blob is unchanged, so it's not re-headed");
    }

    #[test]
    fn secret_files_are_skipped_never_chunked_never_sent_and_reported_in_status() {
        // Item 5. Fake tokens are assembled at runtime -- no literal here.
        let token = format!("{}{}", "gh".to_string() + "p_", "Q".repeat(36));
        let pem = format!("{}BEGIN EC PRIVATE KEY{}\nAAAA\n", "-".repeat(5), "-".repeat(5));
        let fx = FixtureRepo::new("secrets");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/.env", format!("TOKEN={}\n", token).as_bytes());
        fx.write("src/config.rs", format!("const T: &str = \"{}\";\n", token).as_bytes());
        fx.write("src/server.pem", pem.as_bytes());
        fx.write("src/deploy.rs", pem.as_bytes());
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm::default();
        let report = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        let o = &report.projects[0];
        assert!(o.head_advanced, "{:?}", o);
        assert_eq!(o.files_skipped_secret, 4, "{:?}", o);
        assert_eq!(llm.header_calls(), vec!["src/a.rs".to_string()]);
        for prompt in llm.prompts.borrow().iter() {
            assert!(!prompt.contains(&token) && !prompt.contains("PRIVATE KEY"), "a secret reached an LLM prompt");
        }
        let logged: Vec<String> = {
            let mut stmt = conn.prepare("SELECT prompt FROM judge_log").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        assert!(logged.iter().all(|p| !p.contains(&token)), "a secret reached judge_log");
        for path in ["src/.env", "src/config.rs", "src/server.pem", "src/deploy.rs"] {
            let row = store::code_file_get(&conn, project.id, path).unwrap().unwrap();
            assert_eq!(row.status, "skipped", "{}", path);
            assert!(row.lang.as_deref().unwrap_or("").starts_with("secret-"), "{}: {:?}", path, row.lang);
            assert!(code_chunks_for_file(&conn, project.id, path).unwrap().is_empty(), "{} has chunks", path);
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_chunks WHERE text LIKE '%' || ?1 || '%'", params![token], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);

        let status = all_project_statuses(&conn, Some("proj")).unwrap().remove(0);
        assert_eq!(status.skipped, 4);
        let reasons: Vec<&str> = status.skip_reasons.iter().map(|(r, _)| r.as_str()).collect();
        assert!(reasons.contains(&"secret-path(.env)") && reasons.contains(&"secret-content(github-token)"), "{:?}", reasons);
        assert!(status.skip_reasons.iter().all(|(r, _)| !r.contains(&token)));

        // An unchanged skipped file is not re-read or re-evaluated next run.
        let report2 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report2.projects[0].files_chunked, 0);
    }

    #[test]
    fn chunks_already_holding_a_secret_are_swept_to_skipped_on_the_next_run() {
        let token = format!("{}{}", "AK".to_string() + "IA", "Z".repeat(16));
        let fx = FixtureRepo::new("secret-sweep");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        // Simulate a row indexed by an older binary, before secret filtering.
        let blob = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap().blob;
        store::replace_code_chunks(
            &conn,
            project.id,
            "src/a.rs",
            &[store::NewCodeChunk { kind: "window".into(), start_line: 1, end_line: 1, text: format!("key={}", token), ..Default::default() }],
        )
        .unwrap();
        store::code_file_upsert(&conn, project.id, "src/a.rs", &blob, None, 1, "indexed", &store::now_rfc3339()).unwrap();

        run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        let row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(row.status, "skipped");
        assert!(code_chunks_for_file(&conn, project.id, "src/a.rs").unwrap().is_empty());
    }

    #[test]
    fn a_failed_scope_pass_skips_the_project_instead_of_indexing_with_defaults() {
        // Item 6.
        let fx = FixtureRepo::new("scope-fail");
        fx.write("vendor/lib.c", b"int lib(void) { return 1; }\n");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = FakeLlm { fail_scope: true, ..Default::default() };
        let report = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        let o = &report.projects[0];
        assert!(o.skipped_reason.as_deref().unwrap_or("").contains("scope"), "{:?}", o);
        assert!(llm.header_calls().is_empty());
        assert_eq!(report.calls_used, 1, "the failed scope call still spent budget");
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM code_files WHERE project_id = ?1", params![project.id], |r| r.get(0)).unwrap();
        assert_eq!(rows, 0, "nothing chunked under a default-product guess");

        // Next run with a working LLM does the whole thing.
        let report2 = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(report2.projects[0].head_advanced);
    }

    #[test]
    fn recategorizing_a_directory_back_into_scope_indexes_it_without_any_git_change() {
        let fx = FixtureRepo::new("recategorize-in");
        fx.write("vendor/lib.c", b"int lib(void) { return 1; }\n");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        store::code_scope_set(&conn, project.id, "vendor", "vendored", "user", &store::now_rfc3339()).unwrap();
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(store::code_file_get(&conn, project.id, "vendor/lib.c").unwrap().is_none());

        store::code_scope_set(&conn, project.id, "vendor", "product", "user", &store::now_rfc3339()).unwrap();
        let llm = FakeLlm::default();
        run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(llm.header_calls(), vec!["vendor/lib.c".to_string()]);
        assert_eq!(store::code_file_get(&conn, project.id, "vendor/lib.c").unwrap().unwrap().status, "indexed");
    }

    #[test]
    fn a_vanished_top_level_dir_prunes_its_rows_and_does_not_rescope() {
        let fx = FixtureRepo::new("dir-gone");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("old/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        fx.rm("old/b.rs");
        fx.commit("drop old");
        // "old/" loses its only file, which orphan-prunes its module
        // summary (written by the first run) and leaves the repo's own
        // child set changed -- a legitimate `sonnet` call to regenerate
        // "/", so `FakeLlm`, not `PanicLlm`; no file needs re-chunking or
        // re-summarising either way.
        let report = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(!report.projects[0].scope_ran);
        assert_eq!(report.projects[0].headers_attempted, 0);
        assert_eq!(report.projects[0].summaries_attempted, 0);
        let dirs: Vec<String> = store::code_scope_get(&conn, project.id).unwrap().into_iter().map(|r| r.dir).collect();
        assert_eq!(dirs, vec!["src".to_string()]);
        assert!(store::code_summary_get(&conn, project.id, "old/").unwrap().is_none(), "old/'s module summary is pruned once its file is gone");
    }

    #[test]
    fn path_prefix_without_project_is_rejected() {
        // Item 10.
        let conn = mem_conn();
        let mut o = opts();
        o.path_prefix = Some("src".to_string());
        assert!(run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap_err().to_string().contains("--project"));
        assert!(plan_index(&conn, &o).unwrap_err().to_string().contains("--project"));
    }

    #[test]
    fn history_only_without_project_is_rejected() {
        let conn = mem_conn();
        let mut o = opts();
        o.history_only = true;
        assert!(run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap_err().to_string().contains("--project"));
        assert!(plan_index(&conn, &o).unwrap_err().to_string().contains("--project"));
    }

    #[test]
    fn history_only_combined_with_path_prefix_is_rejected() {
        let conn = mem_conn();
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.history_only = true;
        o.path_prefix = Some("src".to_string());
        assert!(run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap_err().to_string().contains("--path-prefix"));
        assert!(plan_index(&conn, &o).unwrap_err().to_string().contains("--path-prefix"));
    }

    #[test]
    fn history_only_runs_only_the_history_stage() {
        let fx = FixtureRepo::new("history-only-runs-only-history");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        o.history_only = true;
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap().projects.remove(0);

        assert!(outcome.months_summarized >= 1, "the current month must still be attempted: {:?}", outcome);
        assert!(!store::code_history_list(&conn, project.id).unwrap().is_empty());

        // Everything else this stage would normally touch stays untouched.
        assert_eq!(outcome.files_chunked, 0, "a --history-only run must never chunk files");
        assert_eq!(outcome.headers_attempted, 0);
        assert_eq!(outcome.summaries_attempted, 0);
        assert_eq!(outcome.modules_attempted, 0);
        assert!(!outcome.scope_ran, "a --history-only run must never run the scope pass");
        assert!(!outcome.head_advanced, "a --history-only run must never advance code_indexed_head");
        assert_eq!(store::code_files_with_status(&conn, project.id, "dirty").unwrap().len(), 0);
        assert!(store::code_scope_get(&conn, project.id).unwrap().is_empty(), "scope pass must not have run");
    }

    #[test]
    fn history_only_still_reports_symbols_and_edges_totals() {
        // A normal run first, so the project actually has symbols/edges,
        // then a --history-only run must report those SAME totals (it
        // doesn't touch the symbol graph, but the run output should still
        // reflect current state without a separate `index status` call).
        let fx = FixtureRepo::new("history-only-reports-existing-totals");
        fx.write("src/a.rs", b"fn a() { b(); }\nfn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.project = Some("proj".to_string());
        let first = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap().projects.remove(0);
        assert!(first.symbols_total > 0, "the normal run must have populated the symbol graph: {:?}", first);

        let mut o2 = opts();
        o2.project = Some("proj".to_string());
        o2.history_only = true;
        let second = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o2).unwrap().projects.remove(0);
        assert_eq!(second.symbols_total, first.symbols_total);
        assert_eq!(second.edges_total, first.edges_total);
    }

    #[test]
    fn an_unknown_project_name_is_an_error_not_an_empty_run() {
        // Item 12.
        let conn = mem_conn();
        let mut o = opts();
        o.project = Some("nope".to_string());
        assert!(run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap_err().to_string().contains("nope"));
        assert!(plan_index(&conn, &o).unwrap_err().to_string().contains("nope"));
        assert!(all_project_statuses(&conn, Some("nope")).unwrap_err().to_string().contains("nope"));
    }

    /// Answers the reachability ping, fails for real chunk text.
    struct PingOnlyEmbedder;
    impl Embedder for PingOnlyEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            if text == EMBEDDER_PING {
                Ok(vec![1.0])
            } else {
                Err(KbError::Embed("flaky".to_string()))
            }
        }
    }

    #[test]
    fn missing_embeddings_are_backfilled_on_a_later_run_without_any_llm_call() {
        // Item 9.
        let fx = FixtureRepo::new("backfill");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &PingOnlyEmbedder, &opts()).unwrap();
        let (chunks, embedded) = count_chunks(&conn, project.id).unwrap();
        assert!(chunks > 0);
        assert_eq!(embedded, 0);

        let report = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report.backfill_embedded, chunks);
        let (_, embedded2) = count_chunks(&conn, project.id).unwrap();
        assert_eq!(embedded2, chunks);
    }

    // -- pure helpers ------------------------------------------------------

    #[test]
    fn parse_header_reply_accepts_hash_and_bare_number_prefixes() {
        let reply = "#1: first chunk header\n2: second chunk header\ngarbage line\n#3:\n";
        let (parsed, summary) = parse_header_reply(reply);
        assert_eq!(parsed.get(&1).unwrap(), "first chunk header");
        assert_eq!(parsed.get(&2).unwrap(), "second chunk header");
        assert!(!parsed.contains_key(&3), "an empty header after the colon leaves the chunk unheaded");
        assert!(summary.is_none(), "no SUMMARY: line in this reply");
    }

    #[test]
    fn parse_header_reply_extracts_headers_and_a_multi_line_summary_together() {
        let reply = "#1: first chunk header\n2: second chunk header\nSUMMARY: does a thing.\nHas a gotcha on the second line.\n";
        let (headers, summary) = parse_header_reply(reply);
        assert_eq!(headers.get(&1).unwrap(), "first chunk header");
        assert_eq!(headers.get(&2).unwrap(), "second chunk header");
        assert_eq!(summary.unwrap(), "does a thing.\nHas a gotcha on the second line.");
    }

    #[test]
    fn parse_header_reply_with_no_summary_marker_returns_none() {
        let reply = "#1: first chunk header\n";
        let (headers, summary) = parse_header_reply(reply);
        assert_eq!(headers.get(&1).unwrap(), "first chunk header");
        assert!(summary.is_none());
    }

    #[test]
    fn parse_header_reply_an_empty_summary_marker_is_none() {
        let reply = "#1: h\nSUMMARY:\n";
        let (_, summary) = parse_header_reply(reply);
        assert!(summary.is_none(), "SUMMARY: alone, nothing after it anywhere, must be None");
    }

    #[test]
    fn parse_header_reply_ignores_garbage_lines_but_still_finds_the_summary() {
        let reply = "not a header line\n#nope: not a number\nSUMMARY: the real summary\n";
        let (headers, summary) = parse_header_reply(reply);
        assert!(headers.is_empty());
        assert_eq!(summary.unwrap(), "the real summary");
    }

    #[test]
    fn parse_header_reply_a_second_summary_looking_line_is_just_more_summary_text() {
        let reply = "SUMMARY: first line\nSUMMARY: not a new marker, just text\n";
        let (_, summary) = parse_header_reply(reply);
        assert_eq!(summary.unwrap(), "first line\nSUMMARY: not a new marker, just text");
    }

    #[test]
    fn parse_summary_reply_is_the_summary_half_of_the_same_grammar() {
        assert_eq!(parse_summary_reply("SUMMARY: only a summary, no chunks\n").unwrap(), "only a summary, no chunks");
        assert!(parse_summary_reply("no marker here at all\n").is_none());
    }

    #[test]
    fn ancestor_summary_paths_caps_at_depth_two_plus_the_repo_root() {
        assert_eq!(ancestor_summary_paths("a/b/c/x.cpp"), vec!["a/".to_string(), "a/b/".to_string(), "/".to_string()]);
        assert_eq!(ancestor_summary_paths("a/x.cpp"), vec!["a/".to_string(), "/".to_string()]);
        assert_eq!(ancestor_summary_paths("x.cpp"), vec!["/".to_string()]);
    }

    #[test]
    fn embedding_input_is_header_newline_first_line_newline_text_truncated() {
        let input = embedding_input(Some("a header"), "fn foo() {\n  body\n}");
        assert_eq!(input, "a header\nfn foo() {\nfn foo() {\n  body\n}", "header + \\n + first line + \\n + full text");

        let long_text = "x".repeat(crate::transcripts::CHUNK_CHARS * 2);
        let input = embedding_input(None, &long_text);
        assert_eq!(input.chars().count(), crate::transcripts::CHUNK_CHARS);
    }

    #[test]
    fn build_header_prompt_lists_chunks_with_dash_for_no_symbol() {
        let chunks = vec![
            ChunkMeta { id: 1, symbol: Some("foo".to_string()), kind: "function".to_string(), start_line: 1, end_line: 3, text: String::new() },
            ChunkMeta { id: 2, symbol: None, kind: "preamble".to_string(), start_line: 4, end_line: 5, text: String::new() },
        ];
        let prompt = build_header_prompt("proj", "src/a.rs", "fn foo() {}\n", &chunks);
        assert!(prompt.contains("Project: proj\nFile: src/a.rs"));
        assert!(prompt.contains("#1 function foo lines 1-3"));
        assert!(prompt.contains("#2 preamble - lines 4-5"));
        assert!(prompt.contains("Then a line \"SUMMARY:\""), "the combined prompt must ask for a summary too");
    }

    #[test]
    fn build_summary_prompt_has_no_chunk_list() {
        let prompt = build_summary_prompt("proj", "src/a.rs", "fn foo() {}\n");
        assert!(prompt.contains("Project: proj\nFile: src/a.rs"));
        assert!(!prompt.contains("Chunks:"), "a summary-only prompt must never carry a chunk list");
        assert!(prompt.contains("SUMMARY:"));
    }

    // -- combined call: file summary + ancestor staleness -------------------

    #[test]
    fn a_combined_call_writes_the_file_summary_and_the_module_stage_regenerates_its_ancestors_the_same_run() {
        // Was originally written against phase 2's ancestor-staling alone
        // (before the module stage existed): pre-seeded placeholder
        // module/repo rows and asserted they came out of the run merely
        // `stale`. Now that `summary::run_module_summary_pass` runs right
        // after the file stage in the very same `index_project` call, a
        // module whose only child is this one freshly-summarised file is
        // trivially ready the moment it's staled -- so it gets regenerated
        // (not left stale) within this same run. Rewritten to assert that
        // real end-to-end behavior instead.
        let fx = FixtureRepo::new("combined-summary");
        fx.write("a/b/c/x.cpp", b"int f() { return 1; }\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);

        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        let summary = store::code_summary_get(&conn, project.id, "a/b/c/x.cpp").unwrap().unwrap();
        assert_eq!(summary.level, "file");
        assert!(!summary.text.is_empty());
        let file_row = store::code_file_get(&conn, project.id, "a/b/c/x.cpp").unwrap().unwrap();
        assert_eq!(summary.source_digest, file_row.blob);
        assert!(!summary.stale, "the file's own fresh summary must not itself be stale");

        // "a/" (depth 1) and "a/b/" (depth 2) are ancestors per the depth
        // cap; "/" (the repo) always is. All three had exactly one thing to
        // wait on (this file, directly or transitively) and it's already
        // fresh, so all three are regenerated in this same run.
        for path in ["a/", "a/b/", "/"] {
            let row = store::code_summary_get(&conn, project.id, path).unwrap().unwrap_or_else(|| panic!("{} must have a summary row", path));
            assert!(!row.stale, "{} is ready in this same run and must be regenerated, not left stale", path);
            assert!(!row.text.is_empty());
        }
        // "a/b/c/" is one level past the depth-2 cap -- never a module, so
        // no row is ever created there (ancestor staling never reaches it,
        // and the module stage never iterates it either).
        assert!(store::code_summary_get(&conn, project.id, "a/b/c/").unwrap().is_none(), "depth-3 is never a module");
    }

    // -- summary-only fallback path ------------------------------------------

    #[test]
    fn a_missing_summary_from_the_combined_call_is_finished_by_a_summary_only_call_the_same_run() {
        let fx = FixtureRepo::new("missing-summary");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let llm = HeaderOnlyThenSummaryLlm::default();
        let outcome = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);

        assert!(outcome.head_advanced);
        assert_eq!(outcome.summaries_attempted, 1);
        assert_eq!(llm.header_calls.borrow().as_slice(), &["src/a.rs".to_string()]);
        assert_eq!(
            llm.summary_calls.borrow().as_slice(),
            &["src/a.rs".to_string()],
            "the combined call's headers-only reply must queue exactly one summary-only call, same run"
        );
        let summary = store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(summary.text, "fallback summary for src/a.rs");
        assert_eq!(summary.level, "file");

        // Re-run: the summary now matches the (unchanged) blob, so nothing
        // further is needed -- PanicLlm proves it.
        let report2 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report2.calls_used, 0);
    }

    #[test]
    fn files_indexed_before_the_summary_feature_existed_get_a_summary_only_call() {
        // No run_index call sets this up (that would fill the summary in via
        // the mechanism under test) -- the code_files/code_chunks rows are
        // poked directly, simulating a file fully indexed and already at
        // code_indexed_head under a pre-Task-2 binary, with zero
        // code_summaries rows anywhere.
        let fx = FixtureRepo::new("pre-phase-2");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let head = fx.repo().head().unwrap();
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let now = store::now_rfc3339();
        store::code_scope_set(&conn, project.id, "src", "product", "user", &now).unwrap();
        let blob = fx.repo().tracked_files_at(&head).unwrap().into_iter().find(|f| f.path == "src/a.rs").unwrap().blob;
        store::replace_code_chunks(
            &conn,
            project.id,
            "src/a.rs",
            &[store::NewCodeChunk {
                kind: "function".into(),
                symbol: Some("a".into()),
                start_line: 1,
                end_line: 1,
                text: "fn a() {}".into(),
                ..Default::default()
            }],
        )
        .unwrap();
        store::code_file_upsert(&conn, project.id, "src/a.rs", &blob, Some("Rust"), 1, "indexed", &now).unwrap();
        store::set_code_indexed_head(&conn, project.id, &head, &now).unwrap();
        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_none());

        let llm = HeaderOnlyThenSummaryLlm::default();
        run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();

        assert!(llm.header_calls.borrow().is_empty(), "the file's blob is unchanged -- no combined call needed");
        assert_eq!(llm.summary_calls.borrow().as_slice(), &["src/a.rs".to_string()]);
        let summary = store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(summary.source_digest, blob);
    }

    #[test]
    fn a_blob_change_that_leaves_the_summary_digest_stale_is_resummarised() {
        let fx = FixtureRepo::new("digest-mismatch");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &HeaderOnlyThenSummaryLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let first_digest = store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().unwrap().source_digest;

        fx.write("src/a.rs", b"fn a() { changed(); }\n");
        fx.commit("change a");
        let llm2 = HeaderOnlyThenSummaryLlm::default();
        run_index(&conn, &llm2, &FakeEmbedder, &opts()).unwrap();

        assert_eq!(llm2.header_calls.borrow().as_slice(), &["src/a.rs".to_string()], "the changed blob is re-chunked and re-headered");
        assert_eq!(
            llm2.summary_calls.borrow().as_slice(),
            &["src/a.rs".to_string()],
            "the stale digest (old summary vs new blob) must trigger exactly one resummarise call"
        );
        let second = store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_ne!(second.source_digest, first_digest);
        let file_row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(second.source_digest, file_row.blob);
    }

    #[test]
    fn budget_accounts_for_summary_only_calls_too() {
        let fx = FixtureRepo::new("budget-summary");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        new_project(&conn, "proj", &fx.path);

        // scope(1) + 2 header calls = 3, exactly enough for scope+headers,
        // nothing left for either file's summary-only call this run.
        let llm1 = HeaderOnlyThenSummaryLlm::default();
        let mut o = opts();
        o.budget = 3;
        let report = run_index(&conn, &llm1, &FakeEmbedder, &o).unwrap();
        assert!(report.budget_reached);
        assert_eq!(llm1.summary_calls.borrow().len(), 0, "budget spent on scope+headers leaves nothing for summaries this run");

        // Resume: both summary-only calls happen now, and count against
        // budget just like header calls do. With both files freshly
        // summarised, "src/" (their shared depth-1 module, both direct
        // children of it) and "/" become ready in this same run too --
        // two more `sonnet` calls, same budget counter -- plus the
        // (also budgeted) history call for the current month.
        let llm2 = HeaderOnlyThenSummaryLlm::default();
        let report2 = run_index(&conn, &llm2, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report2.calls_used, 5, "two summary-only calls, src/'s and the repo's module summaries, plus one history call");
        assert_eq!(llm2.summary_calls.borrow().len(), 2);
        assert_eq!(report2.projects[0].modules_attempted, 2);
        assert_eq!(report2.projects[0].months_summarized, 1);
    }

    #[test]
    fn a_file_that_never_produces_a_parseable_summary_stops_being_queued_after_the_give_up_cap() {
        let fx = FixtureRepo::new("give-up");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);

        let llm = NeverSummarizesLlm::default();
        let outcome = run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert!(outcome.head_advanced, "headers still finalize the file even with no summary ever produced");
        assert_eq!(llm.header_calls.borrow().len(), 1);
        assert_eq!(
            llm.summary_calls.borrow().len(),
            1,
            "the combined call's missing summary is picked up once more by the summary-only pass this same run"
        );

        let row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(row.summary_attempts, 2, "both the combined and the summary-only failures count toward the cap");
        assert_eq!(row.status, "indexed", "status stays indexed even though it never got a summary");
        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_none());

        // Second run: nothing about the file changed, so it's not dirty --
        // and the give-up cap must stop run_summary_pass from calling for
        // it ever again.
        run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(llm.summary_calls.borrow().len(), 1, "the give-up cap stops further summary-only calls on later runs");

        let statuses = all_project_statuses(&conn, None).unwrap();
        assert_eq!(statuses[0].summaries_given_up, 1, "`index status` surfaces the given-up file");
    }

    #[test]
    fn a_blob_change_after_giving_up_resets_the_attempt_count_and_tries_again() {
        let fx = FixtureRepo::new("give-up-reset");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);

        let llm = NeverSummarizesLlm::default();
        run_index(&conn, &llm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap().summary_attempts, 2);

        // The file's content changes: a fresh blob gets a fresh two tries.
        fx.write("src/a.rs", b"fn a() { 1 + 1; }\n");
        fx.commit("change a");
        let llm2 = FakeLlm::default();
        run_index(&conn, &llm2, &FakeEmbedder, &opts()).unwrap();
        let row = store::code_file_get(&conn, project.id, "src/a.rs").unwrap().unwrap();
        assert_eq!(row.summary_attempts, 0, "a new blob resets the give-up count");
        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_some(), "and can succeed again");
    }

    // -- final-review fixes ------------------------------------------------

    struct FailAllLlm;
    impl ReflectLlm for FailAllLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            Err("offline".to_string())
        }
    }

    struct PickyEmbedder;
    impl Embedder for PickyEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            if text.contains("unembeddable") {
                return Err(KbError::Embed("this one input fails".to_string()));
            }
            Ok(text.bytes().take(4).map(|b| b as f32).collect())
        }
    }

    #[test]
    fn summary_backfill_skips_a_single_failing_row_and_continues() {
        // Final-review item 3: one bad row used to `break` the whole backfill.
        let conn = mem_conn();
        store::upsert_project(&conn, "fp-a", "proj-a", "/nonexistent-a", "2026-09-24T00:00:00Z").unwrap();
        let p = store::get_project_by_name(&conn, "proj-a").unwrap().unwrap();
        store::code_summary_upsert(&conn, p.id, "a.cpp", "file", "unembeddable text", "d", "2026-09-24T00:00:00Z").unwrap();
        store::code_summary_upsert(&conn, p.id, "b.cpp", "file", "fine text", "d", "2026-09-24T00:00:00Z").unwrap();
        let done = backfill_summary_embeddings(&conn, &PickyEmbedder, None, 10).unwrap();
        assert_eq!(done, 1);
        assert!(store::code_summary_get(&conn, p.id, "b.cpp").unwrap().unwrap().embedding.is_some());
    }

    #[test]
    fn giving_up_on_a_changed_file_deletes_its_stale_summary_and_clears_its_module() {
        // Final-review item 11: at give-up the old-blob summary must not
        // linger (it describes code that no longer exists) and the
        // ancestors must be re-derived without it.
        let fx = FixtureRepo::new("give-up-delete");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let old_mirror = store::code_summary_get(&conn, project.id, "src/").unwrap().unwrap().memory_id.unwrap();

        fx.write("src/a.rs", b"fn a() { 2 + 2; }\n");
        fx.commit("change a");
        run_index(&conn, &NeverSummarizesLlm::default(), &FakeEmbedder, &opts()).unwrap();

        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_none(), "stale file summary deleted at give-up");
        let module = store::code_summary_get(&conn, project.id, "src/").unwrap().unwrap();
        assert_eq!(module.text, "", "src/ has no child with text left -> cleared placeholder, no call from nothing");
        assert!(store::get(&conn, old_mirror).unwrap().unwrap().invalidated_at.is_some(), "its mirror is gone too");
    }

    #[test]
    fn secret_skipping_a_previously_summarised_file_clears_its_ancestors_and_their_mirrors_immediately() {
        // Final-review item 13: even with no budget left for the module
        // stage, text derived from the now-secret file must stop being
        // served right away.
        let fx = FixtureRepo::new("secret-ancestors");
        fx.write("src/sub/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let mirrors: Vec<i64> = ["src/sub/", "src/", "/"]
            .iter()
            .map(|p| store::code_summary_get(&conn, project.id, p).unwrap().unwrap().memory_id.unwrap())
            .collect();

        let token = format!("{}{}", "gh".to_string() + "p_", "Q".repeat(36));
        fx.write("src/sub/a.rs", format!("const T: &str = \"{}\";\n", token).as_bytes());
        fx.commit("oops");
        // Every call fails, so the module stage cannot regenerate anything
        // this run: whatever is served afterwards is what mark_skipped left.
        run_index(&conn, &FailAllLlm, &FakeEmbedder, &opts()).unwrap();

        assert_eq!(store::code_file_get(&conn, project.id, "src/sub/a.rs").unwrap().unwrap().status, "skipped");
        assert!(store::code_summary_get(&conn, project.id, "src/sub/").unwrap().is_none(), "src/sub/ has no indexed file left: pruned");
        for p in ["src/", "/"] {
            let row = store::code_summary_get(&conn, project.id, p).unwrap().unwrap();
            assert_eq!(row.text, "", "{} placeholder-ized", p);
            assert!(row.stale, "{} will regenerate", p);
        }
        for id in mirrors {
            assert!(store::get(&conn, id).unwrap().unwrap().invalidated_at.is_some(), "mirror #{} invalidated", id);
        }
    }

    #[test]
    fn parse_header_reply_tolerates_markdown_summary_markers_and_a_leading_summary_block() {
        // Final-review item 12.
        for marker in ["**SUMMARY:**", "## SUMMARY:", "**SUMMARY**:", "SUMMARY:"] {
            let (h, s) = parse_header_reply(&format!("#1: first\n{} does things\nmore", marker));
            assert_eq!(h.get(&1).map(|s| s.as_str()), Some("first"), "{}", marker);
            assert_eq!(s.as_deref(), Some("does things\nmore"), "{}", marker);
        }
        // SUMMARY block first, header lines after it: headers still parsed,
        // and they do not leak into the summary.
        let (h, s) = parse_header_reply("SUMMARY: the file does X.\nIt also does Y.\n\n#1: first header\n#2: second header\n");
        assert_eq!(h.get(&1).map(|s| s.as_str()), Some("first header"));
        assert_eq!(h.get(&2).map(|s| s.as_str()), Some("second header"));
        assert_eq!(s.as_deref(), Some("the file does X.\nIt also does Y."));
    }

    #[test]
    fn backfill_summary_embeddings_scoped_to_one_project_never_spends_its_limit_on_another() {
        // Fix-list item 3: a `--project`-scoped run's summary backfill must
        // not spend its (small) `limit` on another project's rows first.
        let conn = mem_conn();
        store::upsert_project(&conn, "fp-a", "proj-a", "/nonexistent-a", "2026-09-24T00:00:00Z").unwrap();
        let proj_a = store::get_project_by_name(&conn, "proj-a").unwrap().unwrap();
        store::upsert_project(&conn, "fp-b", "proj-b", "/nonexistent-b", "2026-09-24T00:00:00Z").unwrap();
        let proj_b = store::get_project_by_name(&conn, "proj-b").unwrap().unwrap();

        store::code_summary_upsert(&conn, proj_a.id, "src/a.cpp", "file", "a summary", "d", "2026-09-24T00:00:00Z").unwrap();
        store::code_summary_upsert(&conn, proj_b.id, "src/b.cpp", "file", "b summary", "d", "2026-09-24T00:00:00Z").unwrap();

        // limit=1: unscoped, whichever project sorts first (proj-a, lower
        // id) eats the only slot.
        let done = backfill_summary_embeddings(&conn, &FakeEmbedder, None, 1).unwrap();
        assert_eq!(done, 1);
        assert!(store::code_summary_get(&conn, proj_a.id, "src/a.cpp").unwrap().unwrap().embedding.is_some());
        assert!(store::code_summary_get(&conn, proj_b.id, "src/b.cpp").unwrap().unwrap().embedding.is_none());

        // Scoped to proj-b with the same tiny limit: proj-b's own row gets
        // the slot regardless of proj-a's still-unembedded backlog.
        let done2 = backfill_summary_embeddings(&conn, &FakeEmbedder, Some(proj_b.id), 1).unwrap();
        assert_eq!(done2, 1);
        assert!(store::code_summary_get(&conn, proj_b.id, "src/b.cpp").unwrap().unwrap().embedding.is_some());
    }

    #[test]
    fn a_project_scoped_index_run_backfills_only_that_projects_summary_embeddings() {
        // End-to-end through run_index: `--project` limits the whole run,
        // including the post-run summary backfill.
        let fx_a = FixtureRepo::new("scope-backfill-a");
        fx_a.write("src/a.rs", b"fn a() {}\n");
        fx_a.commit("first");
        let fx_b = FixtureRepo::new("scope-backfill-b");
        fx_b.write("src/b.rs", b"fn b() {}\n");
        fx_b.commit("first");

        let conn = mem_conn();
        let project_a = new_project(&conn, "proj-a", &fx_a.path);
        let project_b = new_project(&conn, "proj-b", &fx_b.path);

        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        // Simulate an embedder outage that happened during both projects'
        // summary generation, leaving both embeddings NULL for a later
        // backfill to close -- same technique the store.rs tests use
        // rather than orchestrating a real embedder-down run (which would
        // also skip the header/summary calls that write these rows at
        // all).
        conn.execute("UPDATE code_summaries SET embedding = NULL", []).unwrap();

        // --project-scoped backfill-only pass (budget 0: nothing new is
        // indexed or re-summarised, only the end-of-run backfill runs).
        let mut o = opts();
        o.project = Some("proj-a".to_string());
        o.budget = 0;
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &o).unwrap();

        assert!(
            store::code_summary_get(&conn, project_a.id, "src/a.rs").unwrap().unwrap().embedding.is_some(),
            "the scoped project's own summary embedding is backfilled"
        );
        assert!(
            store::code_summary_get(&conn, project_b.id, "src/b.rs").unwrap().unwrap().embedding.is_none(),
            "the other project's summary embedding is left alone by a --project-scoped run"
        );
    }

    #[test]
    fn secret_skipped_files_are_never_summarised() {
        let token = format!("{}{}", "gh".to_string() + "p_", "Q".repeat(36));
        let fx = FixtureRepo::new("secret-no-summary");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.write("src/.env", format!("TOKEN={}\n", token).as_bytes());
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        assert!(store::code_summary_get(&conn, project.id, "src/.env").unwrap().is_none());
        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_some());
    }

    // -- delete / rename ------------------------------------------------------

    #[test]
    fn deleting_a_file_deletes_its_summary_then_the_module_stage_prunes_or_regenerates_ancestors() {
        let fx = FixtureRepo::new("delete-summary");
        fx.write("src/sub/a.rs", b"fn a() {}\n");
        fx.write("src/b.rs", b"fn b() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(store::code_summary_get(&conn, project.id, "src/sub/a.rs").unwrap().is_some());
        // Overwrite "src/"'s real (module-stage-written) summary with a
        // placeholder, so the assertions below can tell whether it was
        // actually regenerated rather than just checking its `stale` bit.
        store::code_summary_upsert(&conn, project.id, "src/", "module", "old module summary", "irrelevant-digest", &store::now_rfc3339())
            .unwrap();

        fx.rm("src/sub/a.rs");
        fx.commit("delete a");
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        assert!(store::code_summary_get(&conn, project.id, "src/sub/a.rs").unwrap().is_none(), "the deleted file's own summary must be gone");
        // "src/sub/" loses its only file -- orphan-pruned outright, not
        // just staled.
        assert!(store::code_summary_get(&conn, project.id, "src/sub/").unwrap().is_none(), "src/sub/ has no indexed files left and is pruned");
        // "src/" is staled by the delete (ancestor_summary_paths reaches
        // it), but its one remaining child ("src/b.rs") is already fresh,
        // so it's ready immediately and the module stage regenerates it in
        // this same run -- not left sitting stale.
        let module = store::code_summary_get(&conn, project.id, "src/").unwrap().unwrap();
        assert!(!module.stale, "src/'s only remaining child is fresh, so it's regenerated immediately");
        assert_ne!(module.text, "old module summary", "regenerated with real content, not left as the placeholder");
    }

    #[test]
    fn renaming_a_file_with_unchanged_blob_moves_its_summary_row_with_no_llm_call() {
        let fx = FixtureRepo::new("rename-summary");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let before = store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().unwrap();

        fx.mv("src/a.rs", "src/renamed.rs");
        fx.commit("rename a");
        run_index(&conn, &PanicUnlessHistoryLlm, &FakeEmbedder, &opts()).unwrap();

        assert!(store::code_summary_get(&conn, project.id, "src/a.rs").unwrap().is_none());
        let after = store::code_summary_get(&conn, project.id, "src/renamed.rs").unwrap().unwrap();
        assert_eq!(after.text, before.text);
        assert_eq!(after.source_digest, before.source_digest);
    }

    #[test]
    fn a_cross_directory_rename_stales_both_the_old_and_new_ancestor_module_paths() {
        let fx = FixtureRepo::new("rename-cross-dir");
        fx.write("a/x.rs", b"fn x() {}\n");
        fx.write("a/z.rs", b"fn z() {}\n");
        fx.write("b/y.rs", b"fn y() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        let x_file_summary = store::code_summary_get(&conn, project.id, "a/x.rs").unwrap().unwrap();
        let a_before = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        let b_before = store::code_summary_get(&conn, project.id, "b/").unwrap().unwrap();
        // Overwrite both module summaries with placeholders so the
        // assertions below can tell whether they were actually
        // regenerated, not just left non-stale.
        let now = store::now_rfc3339();
        store::code_summary_upsert(&conn, project.id, "a/", "module", "old a module summary", "irrelevant-digest", &now).unwrap();
        store::code_summary_upsert(&conn, project.id, "b/", "module", "old b module summary", "irrelevant-digest", &now).unwrap();

        fx.mv("a/x.rs", "b/x.rs");
        fx.commit("cross-dir rename");
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        // The renamed file's own summary content and digest move with it,
        // untouched by the rename (same blob).
        let x_after = store::code_summary_get(&conn, project.id, "b/x.rs").unwrap().unwrap();
        assert!(store::code_summary_get(&conn, project.id, "a/x.rs").unwrap().is_none());
        assert_eq!(x_after.text, x_file_summary.text);
        assert_eq!(x_after.source_digest, x_file_summary.source_digest);

        // Both the OLD (a/, lost x.rs) and NEW (b/, gained x.rs) parent
        // directories' child sets changed -- both must be staled and (all
        // children still fresh) regenerated in this same run, not left as
        // the placeholder (fix-list item 2).
        let a_after = store::code_summary_get(&conn, project.id, "a/").unwrap().unwrap();
        let b_after = store::code_summary_get(&conn, project.id, "b/").unwrap().unwrap();
        assert!(!a_after.stale, "a/'s remaining child (z.rs) is fresh, so it regenerates immediately this same run");
        assert!(!b_after.stale);
        assert_ne!(a_after.text, "old a module summary", "a/ must actually regenerate, not be left as the placeholder");
        assert_ne!(b_after.text, "old b module summary");
        assert_ne!(a_after.source_digest, a_before.source_digest, "a/'s child set changed (lost x.rs)");
        assert_ne!(b_after.source_digest, b_before.source_digest, "b/'s child set changed (gained x.rs)");
    }

    #[test]
    fn a_same_directory_rename_does_not_stale_any_module_path() {
        // Contrast with the cross-directory case above: nothing about any
        // module's children changed, so no staleness and no extra LLM
        // call -- covered already by
        // `renaming_a_file_with_unchanged_blob_moves_its_summary_row_with_no_llm_call`
        // (PanicLlm on the second run), this just asserts the module-level
        // row directly.
        let fx = FixtureRepo::new("rename-same-dir");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let src_before = store::code_summary_get(&conn, project.id, "src/").unwrap().unwrap();

        fx.mv("src/a.rs", "src/renamed.rs");
        fx.commit("same-dir rename");
        run_index(&conn, &PanicUnlessHistoryLlm, &FakeEmbedder, &opts()).unwrap();

        let src_after = store::code_summary_get(&conn, project.id, "src/").unwrap().unwrap();
        assert!(!src_after.stale);
        assert_eq!(src_after.text, src_before.text, "unchanged -- no header/summary/scope call was even possible against PanicUnlessHistoryLlm");
    }

    // -----------------------------------------------------------------
    // Symbol graph (schema v35, `chunk::extract_refs` -- see the module
    // doc's phase 3 paragraph and this task's own header comment).
    // -----------------------------------------------------------------

    fn symbol_row(conn: &Connection, project_id: i64, path: &str, name: &str) -> store::SymbolRow {
        store::symbol_definitions(conn, project_id, name).unwrap().into_iter().find(|s| s.path == path).unwrap_or_else(|| {
            panic!("no symbol named {:?} in {:?}", name, path)
        })
    }

    /// Makes `path` look like a row chunked by a pre-symbol-graph binary:
    /// no symbols/edges and no `refs_blob`.
    fn simulate_pre_graph_row(conn: &Connection, project_id: i64, path: &str) {
        store::delete_file_symbols(conn, project_id, path).unwrap();
        conn.execute("UPDATE code_files SET refs_blob = NULL WHERE project_id = ?1 AND path = ?2", params![project_id, path]).unwrap();
    }

    fn count_symbols(conn: &Connection, project_id: i64) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1", params![project_id], |r| r.get(0)).unwrap()
    }

    fn count_edges(conn: &Connection, project_id: i64) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM code_edges WHERE project_id = ?1", params![project_id], |r| r.get(0)).unwrap()
    }

    #[test]
    fn first_run_populates_symbols_and_edges_and_resolves_a_forward_reference_in_one_pass() {
        // "a_caller.rs" sorts before "z_callee.rs" (git ls-tree / guard_files
        // order), so the caller is chunked BEFORE the file defining the
        // function it calls exists in `code_symbols` -- this only resolves
        // if `resolve_edges` runs once, after the whole file stage, not
        // per-file as each chunk is written.
        let fx = FixtureRepo::new("symbols-first-run");
        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target()\n}\n");
        fx.write("src/z_callee.rs", b"pub fn target() -> i32 {\n    42\n}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();

        let user_sym = symbol_row(&conn, project.id, "src/a_caller.rs", "user");
        let target_sym = symbol_row(&conn, project.id, "src/z_callee.rs", "target");
        assert_eq!(count_symbols(&conn, project.id), 2);
        assert_eq!(count_edges(&conn, project.id), 1);

        let edges = store::callees_of(&conn, project.id, user_sym.id, 10).unwrap();
        assert_eq!(edges.len(), 1, "{:?}", edges);
        assert_eq!(edges[0].kind, "calls");
        assert_eq!(edges[0].dst_name, "target");
        assert_eq!(edges[0].dst_symbol_id, Some(target_sym.id), "the forward reference must resolve in this same run");

        let status = all_project_statuses(&conn, Some("proj")).unwrap().remove(0);
        assert_eq!(status.symbols, 2);
        assert_eq!(status.edges, 1);
        assert_eq!(status.edges_resolved_pct, 100.0);

        // Nothing changed: a second run makes no LLM calls and touches
        // nothing in the graph.
        let symbols_before = count_symbols(&conn, project.id);
        let edges_before = count_edges(&conn, project.id);
        run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(count_symbols(&conn, project.id), symbols_before);
        assert_eq!(count_edges(&conn, project.id), edges_before);
    }

    #[test]
    fn modifying_one_file_only_reextracts_that_files_symbols() {
        let fx = FixtureRepo::new("symbols-modify-one");
        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target()\n}\n");
        fx.write("src/z_callee.rs", b"pub fn target() -> i32 {\n    42\n}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let target_before = symbol_row(&conn, project.id, "src/z_callee.rs", "target");

        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target() + 1\n}\n");
        fx.commit("modify caller");
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.files_chunked, 1, "{:?}", outcome);

        let target_after = symbol_row(&conn, project.id, "src/z_callee.rs", "target");
        assert_eq!(target_before.id, target_after.id, "z_callee.rs's own symbol must be untouched by a_caller.rs's change");

        let user_after = symbol_row(&conn, project.id, "src/a_caller.rs", "user");
        let edges = store::callees_of(&conn, project.id, user_after.id, 10).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_symbol_id, Some(target_after.id), "the re-extracted edge must still resolve");
    }

    #[test]
    fn deleting_a_file_removes_its_symbols_and_nulls_incoming_edges() {
        let fx = FixtureRepo::new("symbols-delete");
        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target()\n}\n");
        fx.write("src/z_callee.rs", b"pub fn target() -> i32 {\n    42\n}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let user_sym = symbol_row(&conn, project.id, "src/a_caller.rs", "user");
        assert!(store::callees_of(&conn, project.id, user_sym.id, 10).unwrap()[0].dst_symbol_id.is_some());

        fx.rm("src/z_callee.rs");
        fx.commit("delete callee");
        let outcome = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.files_deleted, 1);

        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'src/z_callee.rs'",
                params![project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);

        // a_caller.rs itself is untouched (unchanged blob, not re-chunked),
        // but the edge that used to point at target's now-deleted symbol
        // must be NULLed, not left dangling on a stale id -- and stays
        // unresolved (there is nothing left to resolve it to).
        let edges = store::callees_of(&conn, project.id, user_sym.id, 10).unwrap();
        assert_eq!(edges.len(), 1, "{:?}", edges);
        assert_eq!(edges[0].dst_name, "target", "dst_name is kept in case a same-named symbol reappears");
        assert_eq!(edges[0].dst_symbol_id, None);
    }

    #[test]
    fn renaming_a_file_with_unchanged_blob_moves_its_symbols_and_keeps_resolution() {
        let fx = FixtureRepo::new("symbols-rename");
        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target()\n}\n");
        fx.write("src/z_callee.rs", b"pub fn target() -> i32 {\n    42\n}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let target_before = symbol_row(&conn, project.id, "src/z_callee.rs", "target");
        let user_sym = symbol_row(&conn, project.id, "src/a_caller.rs", "user");

        fx.mv("src/z_callee.rs", "src/renamed_callee.rs");
        fx.commit("rename callee");
        let llm2 = FakeLlm::default();
        let outcome = run_index(&conn, &llm2, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert!(llm2.header_calls().is_empty(), "a pure content-unchanged rename needs no header call");
        assert!(outcome.head_advanced);

        let target_after = symbol_row(&conn, project.id, "src/renamed_callee.rs", "target");
        assert_eq!(target_before.id, target_after.id, "the same symbol row moved, it wasn't replaced");
        let old_path_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'src/z_callee.rs'",
                params![project.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_path_count, 0);

        let edges = store::callees_of(&conn, project.id, user_sym.id, 10).unwrap();
        assert_eq!(edges[0].dst_symbol_id, Some(target_after.id), "resolution survives the rename untouched");
    }

    #[test]
    fn secret_file_yields_no_symbols() {
        let token = format!("{}{}", "AK".to_string() + "IA", "Z".repeat(16));
        let fx = FixtureRepo::new("symbols-secret");
        fx.write("src/a.rs", format!("pub fn has_secret() {{ let key = \"{}\"; }}\n", token).as_bytes());
        fx.write("src/b.rs", b"pub fn clean() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let report = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert_eq!(report.projects[0].files_skipped_secret, 1, "{:?}", report.projects[0]);

        let secret_symbols: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'src/a.rs'", params![project.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(secret_symbols, 0, "a secret-skipped file must never contribute a symbol");
        assert!(symbol_row(&conn, project.id, "src/b.rs", "clean").id > 0, "the clean file is still extracted normally");
    }

    #[test]
    fn backfill_extracts_symbols_for_files_indexed_before_the_symbol_graph_existed_with_no_llm_call() {
        let fx = FixtureRepo::new("symbols-backfill");
        fx.write("src/a.rs", b"pub fn a() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert_eq!(count_symbols(&conn, project.id), 1);

        // Simulate a row chunked by a pre-phase-3 binary: it has chunks and
        // is `indexed`, but never got a `code_symbols` row.
        simulate_pre_graph_row(&conn, project.id, "src/a.rs");
        assert_eq!(count_symbols(&conn, project.id), 0);

        // The file's blob is unchanged, so nothing is dirty and no LLM call
        // is possible (PanicLlm would blow up the test) -- backfill must be
        // entirely free.
        let outcome = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.symbols_backfilled, 1, "{:?}", outcome);
        assert_eq!(outcome.files_chunked, 0, "the file itself was never re-chunked");
        assert_eq!(count_symbols(&conn, project.id), 1);
        assert_eq!(symbol_row(&conn, project.id, "src/a.rs", "a").kind, "function");

        // Converged: a second run finds nothing left to backfill.
        let outcome2 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome2.symbols_backfilled, 0);
    }

    #[test]
    fn backfill_never_touches_markdown_or_fallback_files() {
        let fx = FixtureRepo::new("symbols-backfill-skip");
        fx.write("README.md", b"# Title\n\nSome prose.\n");
        fx.write("src/weird.zig", b"this is not a recognised language\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        // Neither file ever gets a code_symbols row (Markdown/fallback are
        // permanent extract_refs no-ops), so backfill must not keep trying
        // them forever.
        simulate_pre_graph_row(&conn, project.id, "README.md");
        simulate_pre_graph_row(&conn, project.id, "src/weird.zig");

        let outcome = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(outcome.symbols_backfilled, 0, "{:?}", outcome);
    }

    #[test]
    fn backfill_respects_path_prefix() {
        let fx = FixtureRepo::new("symbols-backfill-prefix");
        fx.write("src/a.rs", b"pub fn a() {}\n");
        fx.write("lib/b.rs", b"pub fn b() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        simulate_pre_graph_row(&conn, project.id, "src/a.rs");
        simulate_pre_graph_row(&conn, project.id, "lib/b.rs");
        assert_eq!(count_symbols(&conn, project.id), 0);

        let mut o = opts();
        o.project = Some("proj".to_string());
        o.path_prefix = Some("src".to_string());
        let outcome = run_index(&conn, &PanicLlm, &FakeEmbedder, &o).unwrap().projects.remove(0);
        assert_eq!(outcome.symbols_backfilled, 1, "{:?}", outcome);

        let a_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'src/a.rs'", params![project.id], |r| {
                r.get(0)
            })
            .unwrap();
        let b_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_symbols WHERE project_id = ?1 AND path = 'lib/b.rs'", params![project.id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(a_count, 1, "src/a.rs is under the prefix, must be backfilled");
        assert_eq!(b_count, 0, "lib/b.rs is outside the prefix, must be left alone this run");
    }

    #[test]
    fn recategorizing_a_directory_out_of_scope_removes_its_symbols() {
        let fx = FixtureRepo::new("symbols-recategorize-out");
        fx.write("vendor/lib.c", b"int lib(void) { return 1; }\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert!(count_symbols(&conn, project.id) > 0, "vendor/lib.c starts out in scope (default product) and gets symbols");

        store::code_scope_set(&conn, project.id, "vendor", "vendored", "user", &store::now_rfc3339()).unwrap();
        run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap();
        assert_eq!(count_symbols(&conn, project.id), 0, "recategorized-out files must lose their symbols too");
    }

    // -- final-review fix wave ---------------------------------------------

    #[test]
    fn a_zero_symbol_file_is_backfilled_exactly_once() {
        // Item 5: keyed on "has no code_symbols rows", a file that
        // legitimately defines nothing was re-backfilled every run forever.
        let fx = FixtureRepo::new("zero-symbol-backfill");
        fx.write("src/consts.rs", b"// only a comment, no definitions\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        assert_eq!(count_symbols(&conn, project.id), 0);
        let status: String =
            conn.query_row("SELECT status FROM code_files WHERE path = 'src/consts.rs'", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "indexed");
        let refs_blob: Option<String> =
            conn.query_row("SELECT refs_blob FROM code_files WHERE path = 'src/consts.rs'", [], |r| r.get(0)).unwrap();
        assert!(refs_blob.is_some(), "the chunking path records refs_blob");

        conn.execute("UPDATE code_files SET refs_blob = NULL", []).unwrap(); // pre-graph row
        let o1 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(o1.symbols_backfilled, 1);
        let o2 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(o2.symbols_backfilled, 0, "backfilled once, never again");
        assert_eq!(count_symbols(&conn, project.id), 0);
    }

    #[test]
    fn an_unchanged_second_run_examines_zero_edges_and_a_one_file_change_only_its_own() {
        // Item 2: resolution is incremental after the first run.
        let fx = FixtureRepo::new("incremental-resolve");
        fx.write("src/a_caller.rs", b"pub fn user() -> i32 {\n    target()\n}\n");
        fx.write("src/z_callee.rs", b"pub fn target() -> i32 {\n    42\n}\n");
        fx.write("src/other.rs", b"pub fn other() {\n    external_thing();\n}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let o1 = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(o1.edges_examined, 2, "first run: full pass over every unresolved edge");

        let o2 = run_index(&conn, &PanicLlm, &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(o2.edges_examined, 0, "nothing changed: no edge examined");

        fx.write("src/other.rs", b"pub fn other() {\n    external_thing();\n    another_external();\n}\n");
        fx.commit("edit other");
        let o3 = run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap().projects.remove(0);
        assert_eq!(o3.edges_examined, 2, "only other.rs's own two (unresolvable) edges");
        let target = symbol_row(&conn, project.id, "src/z_callee.rs", "target");
        let user = symbol_row(&conn, project.id, "src/a_caller.rs", "user");
        assert_eq!(store::callees_of(&conn, project.id, user.id, 5).unwrap()[0].dst_symbol_id, Some(target.id), "untouched resolution kept");
    }

    #[test]
    fn dry_run_history_only_counts_changed_months_and_normal_dry_run_counts_stale_modules() {
        // Item 18.
        let fx = FixtureRepo::new("dry-run-history");
        fx.write("src/a.rs", b"fn a() {}\n");
        fx.commit("first");
        let conn = mem_conn();
        let project = new_project(&conn, "proj", &fx.path);
        let mut o = opts();
        o.dry_run = true;
        o.project = Some("proj".to_string());
        o.history_only = true;
        let plan = plan_index(&conn, &o).unwrap().remove(0);
        assert_eq!((plan.months_to_summarize, plan.calls_needed, plan.files_to_index), (1, 1, 0));

        run_index(&conn, &FakeLlm::default(), &FakeEmbedder, &opts()).unwrap();
        let plan = plan_index(&conn, &o).unwrap().remove(0);
        assert_eq!(plan.months_to_summarize, 0, "the month is summarized and unchanged");

        store::mark_code_summaries_stale(&conn, project.id, &["src/".to_string()], &store::now_rfc3339()).unwrap();
        let mut o2 = opts();
        o2.dry_run = true;
        let plan = plan_index(&conn, &o2).unwrap().remove(0);
        assert!(store::code_summary_get(&conn, project.id, "src/").unwrap().is_some(), "the first run summarized src/");
        assert_eq!(plan.modules_stale, 1);
        assert_eq!(plan.calls_needed, plan.modules_stale, "nothing else to do");
    }
}
