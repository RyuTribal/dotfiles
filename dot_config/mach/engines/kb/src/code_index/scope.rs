//! Code index: scope pass (see docs/superpowers/specs/2026-09-23-code-index-design.md,
//! "Scope pass").
//!
//! Decides, per repo directory, whether its code should be indexed
//! (`product`/`tests`/`docs`), remembered but not chunked (`vendored`), or
//! skipped entirely (`generated`/`assets`/`build-output`). Deterministic
//! guards run first ([`guard_files`]: drop gitignored and binary paths --
//! never send those to an LLM or a chunker). Then, when the top-level
//! directory set has moved since the last decision ([`needs_scope`]), one
//! sonnet call per repo ([`run_scope_pass`]) classifies every directory
//! `git::Repo::dir_stats` finds down to depth 3, using per-directory file/
//! line/commit/author history plus any LICENSE*/README* content as
//! evidence. Decisions land in `code_scope` via `store::code_scope_set`,
//! which already refuses to let an `"llm"` write clobber a `"user"` one --
//! this module leans on that rather than re-implementing the guard.
//! [`category_for`] is the read side: given the rows for a project, which
//! category governs a specific file path.

use std::collections::{BTreeSet, HashMap};

use rusqlite::{params, Connection, OptionalExtension};

use crate::code_index::git::{DirStat, Repo, TrackedFile};
use crate::code_index::secrets;
use crate::reflect::ReflectLlm;
use crate::store::{self, code_scope_get, code_scope_set, KbError, ScopeRow};

/// The seven categories a directory can be scoped to (spec, "Scope pass").
/// `Product`/`Tests`/`Docs` are indexed; `Vendored` gets one memory and no
/// chunks; `Generated`/`Assets`/`BuildOutput` are skipped outright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Product,
    Tests,
    Docs,
    Vendored,
    Generated,
    Assets,
    BuildOutput,
}

impl Category {
    /// The exact token this category is stored/replied as -- matches the
    /// spec's category names verbatim, including the hyphen in
    /// `build-output`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Product => "product",
            Category::Tests => "tests",
            Category::Docs => "docs",
            Category::Vendored => "vendored",
            Category::Generated => "generated",
            Category::Assets => "assets",
            Category::BuildOutput => "build-output",
        }
    }

    /// Parses one reply token (case-insensitive, surrounding whitespace
    /// trimmed) into a `Category`. `None` for anything else -- callers
    /// treat that as "ignore this line", per the parser's tolerance.
    pub fn parse(token: &str) -> Option<Category> {
        match token.trim().to_ascii_lowercase().as_str() {
            "product" => Some(Category::Product),
            "tests" => Some(Category::Tests),
            "docs" => Some(Category::Docs),
            "vendored" => Some(Category::Vendored),
            "generated" => Some(Category::Generated),
            "assets" => Some(Category::Assets),
            "build-output" | "build_output" | "buildoutput" => Some(Category::BuildOutput),
            _ => None,
        }
    }
}

/// Removes gitignored and binary paths from a repo's tracked-file list --
/// the deterministic guard that runs before any LLM call or chunking, so
/// generated `.gitignore`d build output and binary blobs never reach
/// either. `ignored` is one batched `git check-ignore` call; `is_binary`
/// is one `git cat-file` per remaining candidate (mirrors the cost
/// `Repo::dir_stats` already pays per tracked file).
pub fn guard_files(repo: &Repo, tracked: Vec<TrackedFile>) -> Result<Vec<TrackedFile>, KbError> {
    let paths: Vec<&str> = tracked.iter().map(|f| f.path.as_str()).collect();
    let ignored = repo.ignored(paths)?;
    let mut kept = Vec::with_capacity(tracked.len());
    for f in tracked {
        if ignored.contains(&f.path) {
            continue;
        }
        if repo.is_binary(&f.blob)? {
            continue;
        }
        kept.push(f);
    }
    Ok(kept)
}

/// True when the scope pass should (re-)run for this project: either no
/// scope decisions exist yet, or `top_dirs` (each tracked path's first
/// component, as the caller currently sees it) contains a top-level
/// directory no stored row covers. Only a NEW top-level directory costs a
/// sonnet call: one that merely disappeared needs no decision at all (its
/// stale `llm` rows are dropped by [`prune_stale_scope_rows`] instead), and
/// files changing inside already-classified directories never re-scope.
pub fn needs_scope(conn: &Connection, project_id: i64, top_dirs: &BTreeSet<String>) -> Result<bool, KbError> {
    let rows = code_scope_get(conn, project_id)?;
    if rows.is_empty() {
        return Ok(true);
    }
    let stored: BTreeSet<String> = rows.iter().map(|r| r.dir.split('/').next().unwrap_or("").to_string()).collect();
    Ok(top_dirs.iter().any(|d| !stored.contains(d)))
}

/// Every directory (at any depth) that is an ancestor of some tracked file.
fn tracked_dirs(tracked: &[TrackedFile]) -> std::collections::HashSet<String> {
    let mut dirs = std::collections::HashSet::new();
    for f in tracked {
        let comps: Vec<&str> = f.path.split('/').collect();
        for i in 1..comps.len() {
            dirs.insert(comps[..i].join("/"));
        }
    }
    dirs
}

/// Deletes this project's `llm`-sourced scope rows whose directory no
/// longer exists in `tracked` (the committed tree at the head being
/// indexed). `user` rows are kept: a human's override is a standing
/// decision, and it costs nothing if the directory comes back. Returns the
/// number of rows removed.
pub fn prune_stale_scope_rows(conn: &Connection, project_id: i64, tracked: &[TrackedFile]) -> Result<usize, KbError> {
    let present = tracked_dirs(tracked);
    let mut removed = 0usize;
    for row in code_scope_get(conn, project_id)? {
        if row.source == "llm" && !present.contains(&row.dir) {
            removed += conn.execute(
                "DELETE FROM code_scope WHERE project_id = ?1 AND dir = ?2 AND source = 'llm'",
                params![project_id, row.dir],
            )?;
        }
    }
    Ok(removed)
}

/// Validates one `mach kb index scope <project> --set <dir>=<category>`
/// pair against the committed tree: `dir` (a trailing `/` is ignored) must
/// be a directory some tracked file lives under, and `category` must be one
/// of the seven known tokens (case-insensitive). Returns the normalized
/// `(dir, category)`.
pub fn validate_user_scope_set(tracked: &[TrackedFile], dir: &str, category: &str) -> Result<(String, Category), KbError> {
    let dir = dir.trim().trim_end_matches('/');
    let Some(cat) = Category::parse(category) else {
        return Err(KbError::Other(format!(
            "unknown category '{}' (expected one of: product, tests, docs, vendored, generated, assets, build-output)",
            category
        )));
    };
    if dir.is_empty() || !tracked_dirs(tracked).contains(dir) {
        return Err(KbError::Other(format!("'{}' is not a directory with tracked files at HEAD", dir)));
    }
    Ok((dir.to_string(), cat))
}

/// The category governing `path`: the category of the longest `dir` in
/// `scope_rows` that is either equal to `path` or a path-component-aligned
/// ancestor of it (so `src/vendor` matches `src/vendor/curl.c` but not
/// `src/vendored/other.c`). `Category::Product` when nothing matches --
/// the spec's default for anything the scope pass never decided on.
pub fn category_for(scope_rows: &[ScopeRow], path: &str) -> Category {
    let mut best: Option<(&str, &str)> = None;
    for row in scope_rows {
        let dir = row.dir.as_str();
        let is_match = path == dir || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/');
        if !is_match {
            continue;
        }
        if best.map(|(best_dir, _)| dir.len() > best_dir.len()).unwrap_or(true) {
            best = Some((dir, row.category.as_str()));
        }
    }
    best.and_then(|(_, category)| Category::parse(category)).unwrap_or(Category::Product)
}

/// Builds the one-call-per-repo scope prompt: the category list, then one
/// line per directory with its `dir_stats` evidence, then up to 40 lines
/// of each LICENSE*/README* file's content found anywhere in the tree.
fn build_scope_prompt(stats: &[DirStat], docs: &[(String, String)]) -> String {
    let mut s = String::new();
    s.push_str(
        "You are scoping a code repository so a personal code index knows what to index. \
         For each directory listed below, classify it into exactly one category:\n\
         - product: application/library source code\n\
         - tests: test code\n\
         - docs: documentation\n\
         - vendored: third-party code checked into the repo\n\
         - generated: generated/derived code (codegen output, lockfiles, protobuf stubs)\n\
         - assets: binary or media/data assets, not source\n\
         - build-output: build artifacts, caches, compiled output\n\n\
         Directories (path -- files, lines, commits, authors, first added):\n",
    );
    for stat in stats {
        s.push_str(&format!(
            "{}: files={} lines={} commits={} authors={} first_added={}\n",
            stat.dir,
            stat.files,
            stat.lines,
            stat.commits,
            stat.authors.join(","),
            stat.first_added
        ));
    }
    if !docs.is_empty() {
        s.push_str("\nLICENSE/README excerpts (first 40 lines each):\n");
        for (path, excerpt) in docs {
            s.push_str(&format!("\n--- {} ---\n{}\n", path, excerpt));
        }
    }
    s.push_str(
        "\nReply with exactly one line per directory, nothing else, in this exact format:\n\
         <dir>: <category>\n\
         For a vendored directory only, you may append a short description of what it vendors:\n\
         <dir>: vendored \u{2014} <what>\n",
    );
    s
}

/// Parses a scope-pass reply into `dir -> (category, vendored description)`.
/// Tolerant: a line with no `:`, an empty directory, or an unrecognized
/// category token is skipped rather than erroring -- directories the reply
/// never resolves default to `Category::Product` at the call site
/// ([`run_scope_pass`]), per the spec. The optional ` \u{2014} <what>` suffix
/// (used only for `vendored`) is captured verbatim when present.
fn parse_scope_reply(reply: &str) -> HashMap<String, (Category, Option<String>)> {
    let mut out = HashMap::new();
    for line in reply.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((dir, rest)) = line.split_once(':') else { continue };
        let dir = dir.trim();
        if dir.is_empty() {
            continue;
        }
        let rest = rest.trim();
        let (cat_token, what) = match rest.split_once(" \u{2014} ") {
            Some((cat, what)) => {
                let what = what.trim();
                (cat.trim(), if what.is_empty() { None } else { Some(what.to_string()) })
            }
            None => (rest, None),
        };
        let Some(category) = Category::parse(cat_token) else { continue };
        out.insert(dir.to_string(), (category, what));
    }
    out
}

/// `path`'s basename starts with LICENSE or README (case-insensitive, any
/// extension/suffix -- `LICENSE.md`, `LICENCE`, `readme.rst`, ...).
fn is_license_or_readme(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_uppercase();
    base.starts_with("LICENSE") || base.starts_with("LICENCE") || base.starts_with("README")
}

/// The first 40 lines of every LICENSE*/README* file tracked anywhere in
/// the repo, read at `head` via `Repo::show`.
fn collect_readme_license_excerpts(repo: &Repo, head: &str) -> Result<Vec<(String, String)>, KbError> {
    let mut paths: Vec<String> =
        repo.tracked_files()?.into_iter().map(|f| f.path).filter(|p| is_license_or_readme(p)).collect();
    paths.sort();
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        // Never send a secret to the scope LLM: a secret-looking path, or a
        // README/LICENSE with a secret marker anywhere in it, is dropped
        // from the evidence outright (see `secrets.rs`).
        if secrets::path_reason(&path).is_some() {
            continue;
        }
        let content = repo.show(head, &path)?;
        if secrets::content_reason(&content).is_some() {
            continue;
        }
        let excerpt: String = content.lines().take(40).collect::<Vec<_>>().join("\n");
        out.push((path, excerpt));
    }
    Ok(out)
}

/// Runs the scope pass for one project: one sonnet call (via `llm`, the
/// caller's `reflect::LoggedLlm` tagged `index_scope`) classifying every
/// directory `repo.dir_stats(3)` finds, then records each decision with
/// `store::code_scope_set(.., "llm", now)` -- which itself refuses to
/// clobber a prior `"user"` override, so this function never needs to
/// check for one itself. `dir_stats` is called exactly once, per the
/// caller's note that it is expensive.
///
/// A `vendored` verdict also writes one memory (`store::insert`, source
/// `code-index:<project>:vendored:<dir>`), skipped if that source already
/// has a row -- so re-running the pass never duplicates it.
///
/// Returns `(dirs_written, llm_call_failed)`. On an LLM failure nothing is
/// written at all (not even a `product` default) so the pass is retried in
/// full next time, same as a chunking/header failure leaves a file `dirty`.
pub fn run_scope_pass<L: ReflectLlm>(
    conn: &Connection,
    llm: &L,
    repo: &Repo,
    project_id: i64,
    now: &str,
) -> Result<(usize, bool), KbError> {
    let stats = repo.dir_stats(3)?;
    let head = repo.head()?;
    let docs = collect_readme_license_excerpts(repo, &head)?;
    let prompt = build_scope_prompt(&stats, &docs);

    let reply = match llm.call("sonnet", &prompt, crate::code_index::job::TIMEOUT_INDEX_SONNET) {
        Ok(r) => r,
        Err(_) => return Ok((0, true)),
    };
    let parsed = parse_scope_reply(&reply);

    // Pre-write snapshot: `code_scope_set` silently no-ops on a directory
    // that already has a `"user"` row, so the category that actually ends
    // up in effect there is the *existing* one, not whatever this reply
    // just guessed. A `vendored` memory must key off the effective
    // category, or a user override that turned a dir back into `product`
    // would still get a stray "vendors ..." memory written for it below.
    let existing_rows = code_scope_get(conn, project_id)?;
    let existing_by_dir: HashMap<&str, &ScopeRow> = existing_rows.iter().map(|r| (r.dir.as_str(), r)).collect();

    let mut written = 0usize;
    let mut vendored_project_name: Option<String> = None;
    for stat in &stats {
        let (parsed_category, what) = parsed.get(stat.dir.as_str()).cloned().unwrap_or((Category::Product, None));
        code_scope_set(conn, project_id, &stat.dir, parsed_category.as_str(), "llm", now)?;
        written += 1;

        let effective_category = match existing_by_dir.get(stat.dir.as_str()) {
            Some(row) if row.source == "user" => Category::parse(&row.category).unwrap_or(Category::Product),
            _ => parsed_category,
        };

        if effective_category == Category::Vendored {
            let name = match &vendored_project_name {
                Some(n) => n.clone(),
                None => {
                    let n = project_name(conn, project_id)?;
                    vendored_project_name = Some(n.clone());
                    n
                }
            };
            write_vendored_memory(conn, &name, &stat.dir, what.as_deref())?;
        }
    }
    Ok((written, false))
}

/// The scope row (`dir`) that decides `path`'s effective category via the
/// same longest-matching-prefix rule [`category_for`] itself uses, if any
/// row matches at all -- `category_for` only returns the resolved
/// [`Category`], not which directory produced it. `pub(crate)` for
/// `job.rs`'s recategorization cleanup, which needs to know *which*
/// vendored directory a stale file falls under so it can write that
/// directory's vendored memory (the same one `run_scope_pass` writes when
/// the LLM itself classifies a directory vendored -- a `--set` override
/// never goes through that reply path, so this is the only place it gets
/// written for a user-set vendored directory).
pub(crate) fn owning_dir<'a>(scope_rows: &'a [ScopeRow], path: &str) -> Option<&'a str> {
    let mut best: Option<&str> = None;
    for row in scope_rows {
        let dir = row.dir.as_str();
        let is_match = path == dir || (path.len() > dir.len() && path.starts_with(dir) && path.as_bytes()[dir.len()] == b'/');
        if !is_match {
            continue;
        }
        if best.map(|b| dir.len() > b.len()).unwrap_or(true) {
            best = Some(dir);
        }
    }
    best
}

/// Writes the one memory a `vendored` directory earns ("`<project> vendors
/// <what> under <dir>`", source `code-index:<project>:vendored:<dir>`,
/// importance 5, `reviewed = true` -- it's a deterministic fact about the
/// repo's structure, not a subjective inference), unless a memory with
/// that exact source already exists. `what` falls back to "third-party
/// code" when the reply gave no ` \u{2014} <what>` suffix. `pub(crate)` so
/// `job.rs`'s recategorization cleanup can write it too (see
/// [`owning_dir`]'s doc comment for why that path needs it).
pub(crate) fn write_vendored_memory(conn: &Connection, project_name: &str, dir: &str, what: Option<&str>) -> Result<(), KbError> {
    let source = format!("{}{}:vendored:{}", store::INDEX_OWNED_SOURCE_PREFIX, project_name, dir);
    if memory_source_exists(conn, &source)? {
        return Ok(());
    }
    let what = what.unwrap_or("third-party code");
    let content = format!("{} vendors {} under {}", project_name, what, dir);
    store::insert(conn, &content, Some(&source), Some(project_name), true, None, 5)?;
    Ok(())
}

// -- crate-private SQL helpers ---------------------------------------------
//
// Neither of these exists in `store.rs` today (no `get_project_by_id`, no
// "does a memory with this source exist" lookup) and task 4's brief asks
// for scope.rs-only changes, so they live here as narrow, private queries
// rather than growing store.rs's public surface. Both are small enough
// that promoting them to store.rs later (if another call site needs them)
// should be a plain cut-and-paste.

/// `projects.name` for `project_id`. Errors (including "no such project")
/// propagate as `KbError` -- a scope pass for a project id that doesn't
/// exist is a caller bug, not a recoverable condition.
fn project_name(conn: &Connection, project_id: i64) -> Result<String, KbError> {
    Ok(conn.query_row("SELECT name FROM projects WHERE id = ?1", params![project_id], |r| r.get(0))?)
}

/// Whether any memory already has this exact `source` value.
fn memory_source_exists(conn: &Connection, source: &str) -> Result<bool, KbError> {
    let id: Option<i64> =
        conn.query_row("SELECT id FROM memories WHERE source = ?1 LIMIT 1", params![source], |r| r.get(0)).optional()?;
    Ok(id.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    // -- fixture repo (replicated minimally from git.rs's private
    // `FixtureRepo` -- not `pub(crate)` there, and constraints.md says to
    // replicate rather than touch git.rs) --------------------------------

    struct FixtureRepo {
        path: PathBuf,
    }

    impl FixtureRepo {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mach-kb-scope-test-{}-{}-{}", std::process::id(), tag, n));
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

    fn new_project(conn: &Connection, name: &str) -> i64 {
        store::upsert_project(conn, &format!("fp-{name}"), name, &format!("/repo/{name}"), "2026-09-23T00:00:00Z").unwrap().0
    }

    fn scope_row(project_id: i64, dir: &str, category: &str, source: &str) -> ScopeRow {
        ScopeRow { project_id, dir: dir.to_string(), category: category.to_string(), source: source.to_string(), decided_at: "2026-09-23T00:00:00Z".to_string() }
    }

    /// A `ReflectLlm` test double that always returns a fixed reply, or
    /// always fails -- never spawns a real process. Mirrors the
    /// `FixedReflectLlm` pattern in cli.rs's tests.
    struct FixedReflectLlm {
        reply: Result<&'static str, &'static str>,
    }

    impl ReflectLlm for FixedReflectLlm {
        fn call(&self, _model: &str, _prompt: &str, _timeout: Duration) -> Result<String, String> {
            self.reply.map(|s| s.to_string()).map_err(|e| e.to_string())
        }
    }

    // -- guard_files -------------------------------------------------------

    #[test]
    fn guard_files_removes_gitignored_and_binary_paths() {
        let fx = FixtureRepo::new("guard");
        fx.write(".gitignore", b"*.log\n");
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.write("assets/logo.png", &[0x89, b'P', b'N', b'G', 0u8, 1, 2, 3]);
        fx.commit("first");

        let repo = fx.repo();
        let mut tracked = repo.tracked_files().unwrap();
        // `Repo::ignored` (`git check-ignore`, no `--no-index`) only
        // flags a path git's index does not already know about -- it
        // never reports an actually-committed path as ignored even when
        // it matches a pattern (verified empirically; see this task's
        // report). `guard_files` takes an arbitrary `Vec<TrackedFile>`,
        // not necessarily a live `tracked_files()` snapshot, so this adds
        // one such row by hand to exercise the removal path the way a
        // caller who filters before fully re-syncing the index would.
        tracked.push(TrackedFile { path: "build.log".to_string(), blob: "0".repeat(40) });

        let guarded = guard_files(&repo, tracked).unwrap();
        let mut paths: Vec<&str> = guarded.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec![".gitignore", "src/main.rs"], "gitignored *.log and the binary png must both be dropped");
    }

    #[test]
    fn guard_files_of_an_all_clean_tree_keeps_everything() {
        let fx = FixtureRepo::new("guard-clean");
        fx.write("a.txt", b"hello\n");
        fx.write("dir/b.txt", b"world\n");
        fx.commit("first");

        let repo = fx.repo();
        let tracked = repo.tracked_files().unwrap();
        let guarded = guard_files(&repo, tracked).unwrap();
        assert_eq!(guarded.len(), 2);
    }

    // -- needs_scope ---------------------------------------------------------

    #[test]
    fn needs_scope_is_true_when_no_scope_rows_exist_yet() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let top_dirs: BTreeSet<String> = ["src".to_string()].into_iter().collect();
        assert!(needs_scope(&conn, pid, &top_dirs).unwrap());
    }

    #[test]
    fn needs_scope_is_true_when_the_top_level_directory_set_changed() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_scope_set(&conn, pid, "src", "product", "llm", "2026-09-23T00:00:00Z").unwrap();
        code_scope_set(&conn, pid, "src/sub", "product", "llm", "2026-09-23T00:00:00Z").unwrap();

        let unchanged: BTreeSet<String> = ["src".to_string()].into_iter().collect();
        assert!(!needs_scope(&conn, pid, &unchanged).unwrap(), "same top-level set -- no re-scope needed");

        let grown: BTreeSet<String> = ["src".to_string(), "docs".to_string()].into_iter().collect();
        assert!(needs_scope(&conn, pid, &grown).unwrap(), "a new top-level directory must trigger a re-scope");
    }

    // -- category_for --------------------------------------------------------

    #[test]
    fn category_for_uses_the_longest_matching_directory_prefix() {
        let rows = vec![
            scope_row(1, "src", "product", "llm"),
            scope_row(1, "src/vendor", "vendored", "llm"),
        ];
        assert_eq!(category_for(&rows, "src/vendor/curl.c"), Category::Vendored);
        assert_eq!(category_for(&rows, "src/main.rs"), Category::Product);
    }

    #[test]
    fn category_for_does_not_treat_a_sibling_with_a_shared_prefix_as_a_match() {
        let rows = vec![scope_row(1, "src/vendor", "vendored", "llm")];
        // "src/vendored" shares the literal prefix "src/vendor" but is a
        // different directory -- must not be misclassified as vendored.
        assert_eq!(category_for(&rows, "src/vendored/other.c"), Category::Product);
        assert_eq!(category_for(&rows, "src/vendor"), Category::Vendored, "an exact dir match must also work");
    }

    #[test]
    fn category_for_defaults_to_product_when_nothing_matches() {
        let rows = vec![scope_row(1, "assets", "assets", "llm")];
        assert_eq!(category_for(&rows, "src/main.rs"), Category::Product);
        assert_eq!(category_for(&[], "anything.rs"), Category::Product);
    }

    // -- parse_scope_reply -----------------------------------------------

    #[test]
    fn parse_scope_reply_reads_one_category_per_line() {
        let reply = "src: product\ntests: tests\ndocs: docs\n";
        let parsed = parse_scope_reply(reply);
        assert_eq!(parsed.get("src").unwrap().0, Category::Product);
        assert_eq!(parsed.get("tests").unwrap().0, Category::Tests);
        assert_eq!(parsed.get("docs").unwrap().0, Category::Docs);
    }

    #[test]
    fn parse_scope_reply_captures_the_optional_vendored_description() {
        let reply = "third_party/curl: vendored \u{2014} the curl HTTP client\n";
        let parsed = parse_scope_reply(reply);
        let (category, what) = parsed.get("third_party/curl").unwrap();
        assert_eq!(*category, Category::Vendored);
        assert_eq!(what.as_deref(), Some("the curl HTTP client"));
    }

    #[test]
    fn parse_scope_reply_ignores_garbage_lines() {
        let reply = "not a valid line at all\n\
                     src: product\n\
                     : missing dir\n\
                     weird: not_a_real_category\n\
                     \n\
                     docs: docs\n";
        let parsed = parse_scope_reply(reply);
        assert_eq!(parsed.len(), 2, "{:?}", parsed);
        assert_eq!(parsed.get("src").unwrap().0, Category::Product);
        assert_eq!(parsed.get("docs").unwrap().0, Category::Docs);
        assert!(!parsed.contains_key("weird"), "an unrecognized category token drops the whole line");
    }

    #[test]
    fn parse_scope_reply_is_case_insensitive_and_trims_whitespace() {
        let reply = "  src  :   PRODUCT   \n";
        let parsed = parse_scope_reply(reply);
        assert_eq!(parsed.get("src").unwrap().0, Category::Product);
    }

    // -- run_scope_pass ----------------------------------------------------

    #[test]
    fn run_scope_pass_stores_llm_categories_and_defaults_unmentioned_dirs_to_product() {
        let fx = FixtureRepo::new("run-basic");
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.write("tests/it.rs", b"#[test] fn it() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let llm = FixedReflectLlm { reply: Ok("src: product\n") }; // "tests" left unmentioned on purpose

        let (written, failed) = run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-23T00:00:00Z").unwrap();
        assert!(!failed);
        assert_eq!(written, 2);

        let rows = code_scope_get(&conn, pid).unwrap();
        let by_dir: HashMap<&str, &ScopeRow> = rows.iter().map(|r| (r.dir.as_str(), r)).collect();
        assert_eq!(by_dir.get("src").unwrap().category, "product");
        assert_eq!(by_dir.get("tests").unwrap().category, "product", "an unmentioned directory defaults to product");
        assert!(rows.iter().all(|r| r.source == "llm"));
    }

    #[test]
    fn run_scope_pass_writes_nothing_when_the_llm_call_fails() {
        let fx = FixtureRepo::new("run-failed");
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.commit("first");

        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let llm = FixedReflectLlm { reply: Err("offline") };

        let (written, failed) = run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-23T00:00:00Z").unwrap();
        assert!(failed);
        assert_eq!(written, 0);
        assert!(code_scope_get(&conn, pid).unwrap().is_empty());
    }

    #[test]
    fn run_scope_pass_never_lets_a_user_override_be_undone_by_a_re_run() {
        let fx = FixtureRepo::new("run-user-override");
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.write("third_party/curl.c", b"/* vendored */\n");
        fx.commit("first");

        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        // A human decided third_party is actually product (say, a vendored
        // copy the user maintains and wants indexed as first-party).
        code_scope_set(&conn, pid, "third_party", "product", "user", "2026-09-20T00:00:00Z").unwrap();

        let llm = FixedReflectLlm { reply: Ok("src: product\nthird_party: vendored \u{2014} curl\n") };
        run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-23T00:00:00Z").unwrap();

        let rows = code_scope_get(&conn, pid).unwrap();
        let third_party = rows.iter().find(|r| r.dir == "third_party").unwrap();
        assert_eq!(third_party.category, "product", "the llm's vendored verdict must not clobber the user's override");
        assert_eq!(third_party.source, "user");

        // No vendored memory should have been written either, since the
        // directory was never actually scoped as vendored.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE source = ?1", params!["code-index:helios:vendored:third_party"], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn run_scope_pass_writes_one_vendored_memory_and_never_duplicates_it_on_a_re_run() {
        let fx = FixtureRepo::new("run-vendored");
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.write("third_party/curl.c", b"/* vendored */\n");
        fx.commit("first");

        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let llm = FixedReflectLlm { reply: Ok("src: product\nthird_party: vendored \u{2014} the curl HTTP client\n") };

        run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-23T00:00:00Z").unwrap();
        run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-24T00:00:00Z").unwrap();

        let source = "code-index:helios:vendored:third_party";
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "a second run must not duplicate the vendored memory");

        let content: String =
            conn.query_row("SELECT content FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(content, "helios vendors the curl HTTP client under third_party");
    }

    #[test]
    fn run_scope_pass_falls_back_to_a_generic_description_when_the_reply_gives_none() {
        let fx = FixtureRepo::new("run-vendored-no-desc");
        fx.write("third_party/curl.c", b"/* vendored */\n");
        fx.commit("first");

        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let llm = FixedReflectLlm { reply: Ok("third_party: vendored\n") };
        run_scope_pass(&conn, &llm, &fx.repo(), pid, "2026-09-23T00:00:00Z").unwrap();

        let source = "code-index:helios:vendored:third_party";
        let content: String =
            conn.query_row("SELECT content FROM memories WHERE source = ?1", params![source], |r| r.get(0)).unwrap();
        assert_eq!(content, "helios vendors third-party code under third_party");
    }

    // -- README/LICENSE excerpt collection (build_scope_prompt inputs) ------

    #[test]
    fn collect_readme_license_excerpts_finds_files_anywhere_and_caps_at_40_lines() {
        let fx = FixtureRepo::new("readmes");
        fx.write("LICENSE", b"MIT License\nCopyright...\n");
        let many_lines: String = (1..=50).map(|n| format!("line {n}\n")).collect();
        fx.write("docs/README.md", many_lines.as_bytes());
        fx.write("src/main.rs", b"fn main() {}\n");
        fx.commit("first");

        let repo = fx.repo();
        let head = repo.head().unwrap();
        let docs = collect_readme_license_excerpts(&repo, &head).unwrap();
        let by_path: HashMap<&str, &str> = docs.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();

        assert!(by_path.contains_key("LICENSE"));
        let readme = by_path.get("docs/README.md").expect("README below root must be found");
        assert_eq!(readme.lines().count(), 40, "excerpt must be capped at 40 lines");
        assert_eq!(readme.lines().next().unwrap(), "line 1");
    }

    #[test]
    fn collect_readme_license_excerpts_drops_a_readme_that_carries_a_secret() {
        // Item 5: the scope prompt must never carry a secret either.
        let fx = FixtureRepo::new("readme-secret");
        let token = format!("{}{}", "gh".to_string() + "p_", "a".repeat(36));
        fx.write("README.md", format!("setup\nexport TOKEN={}\n", token).as_bytes());
        fx.write("docs/README.md", b"clean docs\n");
        fx.commit("first");
        let repo = fx.repo();
        let head = repo.head().unwrap();
        let docs = collect_readme_license_excerpts(&repo, &head).unwrap();
        let paths: Vec<&str> = docs.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["docs/README.md"]);
        assert!(docs.iter().all(|(_, c)| !c.contains(&token)));

        let llm = RecordingLlm::default();
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        run_scope_pass(&conn, &llm, &repo, pid, "2026-09-23T00:00:00Z").unwrap();
        assert!(!llm.prompts.borrow().iter().any(|p| p.contains(&token)), "the token must never reach the prompt");
    }

    #[derive(Default)]
    struct RecordingLlm {
        prompts: std::cell::RefCell<Vec<String>>,
    }
    impl ReflectLlm for RecordingLlm {
        fn call(&self, _model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            self.prompts.borrow_mut().push(prompt.to_string());
            Ok(String::new())
        }
    }

    // -- item 7: re-scope only on a NEW top-level dir; stale rows pruned ---

    #[test]
    fn needs_scope_is_false_when_a_top_level_directory_merely_disappeared() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        code_scope_set(&conn, pid, "src", "product", "llm", "2026-09-23T00:00:00Z").unwrap();
        code_scope_set(&conn, pid, "old", "product", "llm", "2026-09-23T00:00:00Z").unwrap();
        let shrunk: BTreeSet<String> = ["src".to_string()].into_iter().collect();
        assert!(!needs_scope(&conn, pid, &shrunk).unwrap(), "losing a directory is not a reason to spend a sonnet call");
    }

    #[test]
    fn prune_stale_scope_rows_deletes_llm_rows_for_vanished_dirs_but_keeps_user_rows() {
        let conn = mem_conn();
        let pid = new_project(&conn, "helios");
        let now = "2026-09-23T00:00:00Z";
        code_scope_set(&conn, pid, "src", "product", "llm", now).unwrap();
        code_scope_set(&conn, pid, "src/sub", "product", "llm", now).unwrap();
        code_scope_set(&conn, pid, "src/gone", "generated", "llm", now).unwrap();
        code_scope_set(&conn, pid, "old", "product", "llm", now).unwrap();
        code_scope_set(&conn, pid, "vendor", "vendored", "user", now).unwrap();
        let tracked = vec![
            TrackedFile { path: "src/a.rs".to_string(), blob: "1".repeat(40) },
            TrackedFile { path: "src/sub/b.rs".to_string(), blob: "2".repeat(40) },
        ];
        let removed = prune_stale_scope_rows(&conn, pid, &tracked).unwrap();
        assert_eq!(removed, 2);
        let mut dirs: Vec<String> = code_scope_get(&conn, pid).unwrap().into_iter().map(|r| r.dir).collect();
        dirs.sort();
        assert_eq!(dirs, vec!["src".to_string(), "src/sub".to_string(), "vendor".to_string()]);
    }

    #[test]
    fn validate_user_scope_set_requires_a_tracked_dir_and_a_known_category() {
        let tracked = vec![TrackedFile { path: "src/sub/a.rs".to_string(), blob: "1".repeat(40) }];
        assert_eq!(validate_user_scope_set(&tracked, "src", "vendored").unwrap(), ("src".to_string(), Category::Vendored));
        assert_eq!(validate_user_scope_set(&tracked, "src/sub/", "Build-Output").unwrap().0, "src/sub");
        assert!(validate_user_scope_set(&tracked, "nope", "product").unwrap_err().to_string().contains("nope"));
        assert!(validate_user_scope_set(&tracked, "src/sub/a.rs", "product").is_err(), "a file is not a directory");
        assert!(validate_user_scope_set(&tracked, "sr", "product").is_err(), "a string prefix is not a directory");
        assert!(validate_user_scope_set(&tracked, "src", "vendor").unwrap_err().to_string().contains("vendor"));
    }
}
