//! Code index: git (see docs/superpowers/specs/2026-09-23-code-index-design.md).
//!
//! The only place the code index talks to git. Every command below is
//! read-only (`rev-parse`, `ls-tree`, `diff`, `cat-file`,
//! `check-ignore`, `log`) and always runs with `-C <root>`, so this module
//! never writes to a repo on disk -- no checkout, commit, stash, fetch,
//! reset, or config write. `GIT_CONFIG_GLOBAL=/dev/null` and `LC_ALL=C` are
//! set on every invocation so neither the caller's global gitconfig nor
//! locale can change output formatting.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::store::KbError;

/// One entry from `git ls-tree -r <rev>`: a committed path and its blob sha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedFile {
    pub path: String,
    pub blob: String,
}

/// One line of `git diff --name-status -M -z <from> <to>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(String),
    Modified(String),
    Deleted(String),
    Renamed { from: String, to: String },
}

/// One file's line-change counts from a commit's `git log --numstat`
/// output. A binary file (git prints `-` for both counts, since there is
/// no meaningful line count) is reported as `added = removed = 0` -- the
/// file still touched, there is simply nothing to count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatLine {
    pub path: String,
    pub added: i64,
    pub removed: i64,
}

/// One commit's metadata plus its per-file line-change stats -- the
/// `code_index::history` (Task 3) month-text raw material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    /// Committer date, `%cI` (ISO 8601, the committer's own timezone
    /// offset -- never converted to UTC). "Distinct `YYYY-MM` of committer
    /// dates" (the month grouping this feeds) is read straight off this
    /// string's first 7 characters.
    pub date: String,
    pub author: String,
    pub subject: String,
    /// Raw commit message body (everything after the subject line),
    /// trimmed of the trailing blank line(s) git's own message
    /// normalization and `--numstat`'s block separator add. May be empty.
    /// Truncating to a preview length is the caller's job, not this
    /// module's -- this is the whole body exactly as committed.
    pub body: String,
    /// Per-file line-change counts, in the order `git log --numstat`
    /// reports them (git's own path order within the commit, not sorted by
    /// churn -- picking e.g. "top 15 by churn" is the caller's job).
    pub stat_lines: Vec<StatLine>,
}

/// One commit from [`Repo::log_path`]: just enough to list a path's recent
/// history and group it by committer-date month -- `sha`/`date`/`author`/
/// `subject` plus the files it touched under the queried path, no body or
/// numstat (unlike [`CommitInfo`], which `log_month`'s month-narrative
/// text needs both of).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathLogEntry {
    pub sha: String,
    /// Committer date, `%cI` (ISO 8601), same convention as
    /// [`CommitInfo::date`] -- its first 7 characters are the commit's
    /// `YYYY-MM` month.
    pub date: String,
    pub author: String,
    pub subject: String,
    /// Files this commit touched under the queried path (`--name-only`,
    /// limited to the pathspec), git's order, capped at
    /// [`PATH_LOG_FILES_PER_COMMIT`].
    pub files: Vec<String>,
}

/// Cap on [`PathLogEntry::files`] per commit.
pub const PATH_LOG_FILES_PER_COMMIT: usize = 10;

/// Every commit reachable from `HEAD`, grouped by committer-date month --
/// the output of ONE cheap `git log --format=%H%x1f%cI` pass
/// ([`Repo::month_index`]). `months` is newest first; each month's shas are
/// in `git log` order (newest first). Enough to compute every month's
/// digest without any numstat/body work.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonthIndex {
    pub months: Vec<(String, Vec<String>)>,
}

/// Aggregated stats for one directory, up to `dir_stats`'s `depth` bound --
/// the shape the scope pass ranks directories by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirStat {
    pub dir: String,
    pub files: usize,
    pub lines: usize,
    pub commits: usize,
    pub authors: Vec<String>,
    /// Author date (`%aI`, RFC 3339) of the oldest commit that touched this
    /// directory, i.e. the last line of `git log -- <dir>` (git lists
    /// newest-first). Empty when `git log` returned nothing for the dir,
    /// which should not happen for a directory derived from tracked files.
    pub first_added: String,
}

/// A git working tree, accessed read-only, rooted at `root`.
pub struct Repo {
    root: PathBuf,
    /// How many body+numstat reads ([`Repo::log_commits`]) this handle has
    /// spawned -- lets the history stage's tests assert that unchanged
    /// months cost no numstat pass at all.
    numstat_calls: std::cell::Cell<usize>,
}

impl Repo {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Repo { root: root.into(), numstat_calls: std::cell::Cell::new(0) }
    }

    /// See the `numstat_calls` field.
    pub fn numstat_calls(&self) -> usize {
        self.numstat_calls.get()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// True when `path` is inside a git working tree. Used to decide
    /// whether this module should touch a project at all -- callers must
    /// not construct a `Repo` (or call any other method) for a path this
    /// returns `false` for.
    pub fn is_repo(path: &Path) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(path)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args(["rev-parse", "--is-inside-work-tree"])
            .output()
            .map(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "true")
            .unwrap_or(false)
    }

    /// The current `HEAD` commit sha.
    pub fn head(&self) -> Result<String, KbError> {
        let out = self.run(&["rev-parse", "HEAD"])?;
        self.check(&["rev-parse", "HEAD"], &out)?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Every file committed at `HEAD`, with its blob sha -- see
    /// [`Repo::tracked_files_at`].
    pub fn tracked_files(&self) -> Result<Vec<TrackedFile>, KbError> {
        self.tracked_files_at("HEAD")
    }

    /// Every file committed at `rev` (`git ls-tree -r -z <rev>`), with its
    /// blob sha. Committed content, not git's staging index: a staged but
    /// uncommitted change must never make the blob recorded for a path
    /// disagree with what `show(rev, path)` returns (that mismatch re-chunks
    /// the same file on every run). Submodule entries (`commit` objects) are
    /// skipped -- there is no blob to read.
    pub fn tracked_files_at(&self, rev: &str) -> Result<Vec<TrackedFile>, KbError> {
        let args = ["ls-tree", "-r", "-z", "--full-tree", rev];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut files = Vec::new();
        for entry in text.split('\0').filter(|s| !s.is_empty()) {
            // "<mode> SP <type> SP <object> TAB <path>"
            let (meta, path) = entry
                .split_once('\t')
                .ok_or_else(|| KbError::Other(format!("git ls-tree: malformed entry {:?}", entry)))?;
            let mut parts = meta.split_whitespace();
            let kind = parts.nth(1).unwrap_or("");
            let blob =
                parts.next().ok_or_else(|| KbError::Other(format!("git ls-tree: malformed entry {:?}", entry)))?;
            if kind != "blob" {
                continue;
            }
            files.push(TrackedFile { path: path.to_string(), blob: blob.to_string() });
        }
        Ok(files)
    }

    /// True iff `sha` names a commit object in this repo
    /// (`git cat-file -e <sha>^{commit}`). A stored `code_indexed_head` that
    /// was garbage-collected away (rebase + gc, a re-cloned repo) is not one,
    /// and must not be handed to `git diff`.
    pub fn is_commit(&self, sha: &str) -> bool {
        if sha.is_empty() || sha.starts_with('-') {
            return false;
        }
        let spec = format!("{}^{{commit}}", sha);
        self.run(&["cat-file", "-e", spec.as_str()]).map(|o| o.status.success()).unwrap_or(false)
    }

    /// File-level changes between two commit-ish revisions
    /// (`git diff --name-status -M -z`), rename-aware at git's default
    /// similarity threshold (>=50%).
    pub fn diff(&self, from: &str, to: &str) -> Result<Vec<Change>, KbError> {
        let args = ["diff", "--name-status", "-M", "-z", from, to];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut tokens: std::collections::VecDeque<&str> = text.split('\0').filter(|s| !s.is_empty()).collect();
        let mut changes = Vec::new();
        while let Some(status) = tokens.pop_front() {
            let code = status.as_bytes().first().copied().unwrap_or(0) as char;
            match code {
                'A' => {
                    let path = tokens
                        .pop_front()
                        .ok_or_else(|| KbError::Other("git diff: missing path after A".to_string()))?;
                    changes.push(Change::Added(path.to_string()));
                }
                'M' => {
                    let path = tokens
                        .pop_front()
                        .ok_or_else(|| KbError::Other("git diff: missing path after M".to_string()))?;
                    changes.push(Change::Modified(path.to_string()));
                }
                'D' => {
                    let path = tokens
                        .pop_front()
                        .ok_or_else(|| KbError::Other("git diff: missing path after D".to_string()))?;
                    changes.push(Change::Deleted(path.to_string()));
                }
                'R' => {
                    let from_path = tokens
                        .pop_front()
                        .ok_or_else(|| KbError::Other("git diff: missing old path after R".to_string()))?;
                    let to_path = tokens
                        .pop_front()
                        .ok_or_else(|| KbError::Other("git diff: missing new path after R".to_string()))?;
                    changes.push(Change::Renamed { from: from_path.to_string(), to: to_path.to_string() });
                }
                other => {
                    return Err(KbError::Other(format!("git diff: unexpected status {:?} ({})", status, other)));
                }
            }
        }
        Ok(changes)
    }

    /// True when the first 8000 bytes of `blob`'s content contain a NUL
    /// byte -- the same heuristic git itself uses to decide "binary".
    pub fn is_binary(&self, blob: &str) -> Result<bool, KbError> {
        let bytes = self.blob_bytes(blob)?;
        Ok(is_binary_content(&bytes))
    }

    /// `path`'s blob as it existed at `sha`, decoded as lossy UTF-8, via
    /// `git cat-file blob <sha>:<path>`. Deliberately not `git show`: `show`
    /// parses its argument as a revision *range*, so a path like
    /// `..stash@{0}` or `..some-branch` turned `<sha>:..stash@{0}` into a
    /// range and printed another ref's content. `cat-file blob` accepts
    /// exactly one object name and fails on anything that isn't a blob.
    pub fn show(&self, sha: &str, path: &str) -> Result<String, KbError> {
        let spec = format!("{}:{}", sha, path);
        let args = ["cat-file", "blob", spec.as_str()];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Of `paths`, the ones git would ignore (`.gitignore`, `.git/info/exclude`,
    /// etc.), via `git check-ignore --no-index --stdin -z`. Exit code 1
    /// ("none of the given paths are ignored") is not an error -- only a
    /// hard failure (exit 128) is.
    ///
    /// `--no-index` matters: plain `check-ignore` refuses to report a path
    /// as ignored while it is still present in git's index, which every
    /// already-committed file always is -- so a directory added to
    /// `.gitignore` *after* its files were committed (`build/` added to
    /// `.gitignore` after `build/out.o` was already checked in) would
    /// never be flagged, forever. `--no-index` evaluates purely by
    /// pathname pattern against the ignore rules, regardless of whether
    /// the path is tracked, which is what a caller checking "should this
    /// still be indexed" actually needs.
    pub fn ignored<I, S>(&self, paths: I) -> Result<HashSet<String>, KbError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let args = ["check-ignore", "--no-index", "--stdin", "-z"];
        let mut child = self
            .command(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| KbError::Other(format!("failed to spawn git {}: {}", args.join(" "), e)))?;
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| KbError::Other("git check-ignore: no stdin handle".to_string()))?;
            for p in paths {
                stdin
                    .write_all(p.as_ref().as_bytes())
                    .and_then(|_| stdin.write_all(b"\0"))
                    .map_err(|e| KbError::Other(format!("git check-ignore: failed writing stdin: {}", e)))?;
            }
            // stdin dropped here, closing the pipe so git sees EOF.
        }
        let out = child
            .wait_with_output()
            .map_err(|e| KbError::Other(format!("git check-ignore: failed to wait: {}", e)))?;
        // 0 = some paths ignored, 1 = none ignored -- both are a normal
        // result, not a failure. Anything else (128 = fatal error) is.
        match out.status.code() {
            Some(0) | Some(1) => {}
            _ => {
                return Err(KbError::Other(format!(
                    "git {} failed (exit {:?}): {}",
                    args.join(" "),
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
        }
        let text = String::from_utf8_lossy(&out.stdout);
        Ok(text.split('\0').filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
    }

    /// Per-directory stats used by the scope pass to decide which
    /// directories to index: file/line counts from the tracked tree at
    /// `HEAD`, plus commit/author history from `git log -- <dir>`.
    ///
    /// Only directories within `depth` path components of `root` are
    /// considered (`depth` bounds how many `git log` invocations this
    /// makes -- one per qualifying directory). `depth == 0` yields no
    /// directories (every tracked file lives at depth >= 1).
    pub fn dir_stats(&self, depth: usize) -> Result<Vec<DirStat>, KbError> {
        let files = self.tracked_files()?;

        let mut dirs: BTreeSet<String> = BTreeSet::new();
        let mut file_count: HashMap<String, usize> = HashMap::new();
        let mut line_count: HashMap<String, usize> = HashMap::new();

        for f in &files {
            let comps: Vec<&str> = f.path.split('/').collect();
            // The last component is the filename, not a directory; an
            // ancestor directory list only goes up to comps.len() - 1.
            let max_ancestor = comps.len().saturating_sub(1).min(depth);
            if max_ancestor == 0 {
                continue;
            }
            let bytes = self.blob_bytes(&f.blob)?;
            let lines = if is_binary_content(&bytes) { 0 } else { count_lines(&bytes) };
            for i in 1..=max_ancestor {
                let dir = comps[..i].join("/");
                dirs.insert(dir.clone());
                *file_count.entry(dir.clone()).or_insert(0) += 1;
                *line_count.entry(dir).or_insert(0) += lines;
            }
        }

        let mut stats = Vec::with_capacity(dirs.len());
        for dir in dirs {
            let args = ["log", "--format=%H%x1f%an%x1f%aI", "--", dir.as_str()];
            let out = self.run(&args)?;
            self.check(&args, &out)?;
            let text = String::from_utf8_lossy(&out.stdout);

            let mut commit_shas: HashSet<String> = HashSet::new();
            let mut authors: HashSet<String> = HashSet::new();
            let mut last_date = String::new();
            for line in text.lines().filter(|l| !l.is_empty()) {
                let mut parts = line.splitn(3, '\u{1f}');
                let sha = parts.next().unwrap_or("");
                let author = parts.next().unwrap_or("");
                let date = parts.next().unwrap_or("");
                if !sha.is_empty() {
                    commit_shas.insert(sha.to_string());
                }
                if !author.is_empty() {
                    authors.insert(author.to_string());
                }
                if !date.is_empty() {
                    // `git log` lists newest-first, so the last non-empty
                    // date seen is the oldest commit touching this dir.
                    last_date = date.to_string();
                }
            }
            let mut authors: Vec<String> = authors.into_iter().collect();
            authors.sort();

            stats.push(DirStat {
                files: *file_count.get(&dir).unwrap_or(&0),
                lines: *line_count.get(&dir).unwrap_or(&0),
                commits: commit_shas.len(),
                authors,
                first_added: last_date,
                dir,
            });
        }
        Ok(stats)
    }

    /// Every distinct `YYYY-MM` committer-date month reachable from `HEAD`
    /// (`git log --format=%cI`, grouped by each line's first 7 characters),
    /// newest first -- `code_index::history`'s month list.
    pub fn months(&self) -> Result<Vec<String>, KbError> {
        let args = ["log", "--format=%cI"];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut months: BTreeSet<String> = BTreeSet::new();
        for line in text.lines().filter(|l| !l.is_empty()) {
            if line.len() >= 7 {
                months.insert(line[..7].to_string());
            }
        }
        // BTreeSet iterates ascending; reversed for "newest first".
        Ok(months.into_iter().rev().collect())
    }

    /// One `git log --format=%H%x1f%cI` pass over everything reachable from
    /// `HEAD`, grouped by month -- see [`MonthIndex`]. This is all the
    /// history stage needs to decide which months changed; the expensive
    /// body+numstat read ([`Repo::log_commits`]) only runs for those.
    pub fn month_index(&self) -> Result<MonthIndex, KbError> {
        let args = ["log", "--format=%H%x1f%cI"];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut by_month: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for line in text.lines().filter(|l| !l.is_empty()) {
            let Some((sha, date)) = line.split_once('\u{1f}') else { continue };
            if date.len() >= 7 {
                by_month.entry(date[..7].to_string()).or_default().push(sha.to_string());
            }
        }
        Ok(MonthIndex { months: by_month.into_iter().rev().collect() })
    }

    /// Body + numstat for exactly `shas` (`git log --no-walk=unsorted
    /// --stdin --numstat`, the shas fed on stdin -- never argv, so a
    /// 5,000-commit month can't hit an argument-length limit), returned
    /// oldest first by committer date. Only hex object names are accepted;
    /// anything else is an error before git is spawned.
    pub fn log_commits(&self, shas: &[String]) -> Result<Vec<CommitInfo>, KbError> {
        if shas.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(bad) = shas.iter().find(|s| s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(KbError::Other(format!("git log: refusing non-sha revision {:?}", bad)));
        }
        const RS: char = '\u{1e}';
        const FS: char = '\u{1f}';
        let fmt = format!("--format={RS}%H{FS}%cI{FS}%an{FS}%s{FS}%b");
        let args = ["log", "--no-walk=unsorted", "--stdin", fmt.as_str(), "--numstat"];
        self.numstat_calls.set(self.numstat_calls.get() + 1);
        let mut child = self
            .command(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| KbError::Other(format!("failed to spawn git {}: {}", args.join(" "), e)))?;
        {
            let mut stdin = child.stdin.take().ok_or_else(|| KbError::Other("git log: no stdin handle".to_string()))?;
            let mut input = shas.join("\n");
            input.push('\n');
            stdin.write_all(input.as_bytes()).map_err(|e| KbError::Other(format!("git log: failed writing stdin: {}", e)))?;
        }
        let out = child.wait_with_output().map_err(|e| KbError::Other(format!("git log: failed to wait: {}", e)))?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits = parse_log_records(&text, None);
        commits.sort_by(|a, b| a.date.cmp(&b.date));
        Ok(commits)
    }

    /// Every commit reachable from `HEAD` whose committer-date month
    /// (`%cI`'s first 7 characters) equals `month` (`"YYYY-MM"`), oldest
    /// first within the month (`git log` itself lists newest first --
    /// reversed here since a month's own narrative reads chronologically).
    ///
    /// Implementation: one `git log --numstat` call with a custom format
    /// that opens each commit record with `\x1e` (record separator) and
    /// separates `%H`/`%cI`/`%an`/`%s`/`%b` with `\x1f` (unit separator),
    /// so the whole history can be split into per-commit chunks even
    /// though a commit body may itself contain blank lines and arbitrary
    /// text. Within a chunk, the numstat rows (`<added>\t<removed>\t<path>`,
    /// each field either all-digits or `-` for a binary file) are peeled
    /// off from the END backwards until a line doesn't match that shape,
    /// since a body can contain its own blank lines and git additionally
    /// appends a message-normalization blank line before the numstat
    /// block -- counting blank lines is not reliable, but the numstat
    /// row's own three-tab-separated-field shape is.
    pub fn log_month(&self, month: &str) -> Result<Vec<CommitInfo>, KbError> {
        const RS: char = '\u{1e}';
        const FS: char = '\u{1f}';
        let fmt = format!("--format={RS}%H{FS}%cI{FS}%an{FS}%s{FS}%b");
        let args = ["log", fmt.as_str(), "--numstat"];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits = parse_log_records(&text, Some(month));
        commits.reverse();
        Ok(commits)
    }

    /// The `n` most recent commits touching `path` (`git log -n <n>
    /// --format=... -- <path>`), newest first (git's own order) --
    /// `code_index::ask`'s `history {"path": ...}` tool. Unlike
    /// [`Repo::log_month`], this needs no body or numstat (that tool only
    /// wants a short per-commit line plus each commit's committer-date
    /// month, to look up that month's `code_history` summary) so the
    /// format is a single-line-per-commit `%H`/`%cI`/`%an`/`%s`, `\x1f`
    /// (unit separator) joined -- no record separator needed since `%s`
    /// (subject only, never the body) never contains a raw newline. `path`
    /// reaches git as a plain positional argument after `--`, never
    /// interpolated into the format string or a shell, so it cannot be
    /// misread as a revision or option; validating that `path` is a safe,
    /// index-exposed repo-relative path is the caller's job (`ask.rs`
    /// reuses `read`'s own refusal rules), not this method's.
    ///
    /// `--literal-pathspecs`: `path` is matched literally, never as a glob or
    /// a `:(magic)` pathspec -- `*` or `:(exclude)` in a model-supplied path
    /// can't widen what the log reveals beyond the one path asked about.
    /// `--name-only` adds each commit's touched files under `path` (capped
    /// at [`PATH_LOG_FILES_PER_COMMIT`]), so each record opens with `\x1e`.
    pub fn log_path(&self, path: &str, n: usize) -> Result<Vec<PathLogEntry>, KbError> {
        const RS: char = '\u{1e}';
        const FS: char = '\u{1f}';
        let fmt = format!("--format={RS}%H{FS}%cI{FS}%an{FS}%s");
        let n_str = n.to_string();
        let args = ["--literal-pathspecs", "log", "-n", n_str.as_str(), fmt.as_str(), "--name-only", "--", path];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits = Vec::new();
        for record in text.split(RS).filter(|r| !r.trim().is_empty()) {
            let mut lines = record.lines();
            let header = lines.next().unwrap_or("");
            let mut parts = header.splitn(4, FS);
            let sha = parts.next().unwrap_or("").to_string();
            let date = parts.next().unwrap_or("").to_string();
            let author = parts.next().unwrap_or("").to_string();
            let subject = parts.next().unwrap_or("").to_string();
            let files: Vec<String> =
                lines.map(str::trim).filter(|l| !l.is_empty()).take(PATH_LOG_FILES_PER_COMMIT).map(str::to_string).collect();
            commits.push(PathLogEntry { sha, date, author, subject, files });
        }
        Ok(commits)
    }

    // -- internal helpers --------------------------------------------------

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("LC_ALL", "C")
            .args(args);
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<Output, KbError> {
        self.command(args)
            .output()
            .map_err(|e| KbError::Other(format!("failed to spawn git {}: {}", args.join(" "), e)))
    }

    fn check(&self, args: &[&str], out: &Output) -> Result<(), KbError> {
        if out.status.success() {
            Ok(())
        } else {
            Err(KbError::Other(format!(
                "git {} failed (exit {:?}): {}",
                args.join(" "),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }

    fn blob_bytes(&self, blob: &str) -> Result<Vec<u8>, KbError> {
        let args = ["cat-file", "-p", blob];
        let out = self.run(&args)?;
        self.check(&args, &out)?;
        Ok(out.stdout)
    }
}

fn is_binary_content(bytes: &[u8]) -> bool {
    let n = bytes.len().min(8000);
    bytes[..n].contains(&0u8)
}

fn count_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Parses `\x1e`-opened, `\x1f`-separated `%H %cI %an %s %b` + numstat
/// records (the shared format of [`Repo::log_month`] and
/// [`Repo::log_commits`]), keeping only `month`'s commits when given, in the
/// order git printed them.
fn parse_log_records(text: &str, month: Option<&str>) -> Vec<CommitInfo> {
    let mut commits = Vec::new();
    for chunk in text.split('\u{1e}').filter(|c| !c.is_empty()) {
        let mut parts = chunk.splitn(5, '\u{1f}');
        let sha = parts.next().unwrap_or("").trim().to_string();
        let date = parts.next().unwrap_or("").to_string();
        if let Some(m) = month {
            if date.len() < 7 || date[..7] != *m {
                continue;
            }
        }
        if sha.is_empty() {
            continue;
        }
        let author = parts.next().unwrap_or("").to_string();
        let subject = parts.next().unwrap_or("").to_string();
        let rest = parts.next().unwrap_or("");
        let (body, stat_lines) = split_body_and_numstat(rest);
        commits.push(CommitInfo { sha, date, author, subject, body, stat_lines });
    }
    commits
}

/// Splits `rest` (everything in a `log_month` chunk after the `%s` field --
/// the raw `%b` body, a blank-line separator, then the `--numstat` rows) into
/// `(body, stat_lines)`. Peels numstat rows off the end while they match;
/// whatever remains (with trailing blank lines trimmed) is the body. See
/// [`Repo::log_month`]'s own doc comment for why this can't just count
/// blank lines.
fn split_body_and_numstat(rest: &str) -> (String, Vec<StatLine>) {
    let mut lines: Vec<&str> = rest.split('\n').collect();
    // `rest` always ends with a real `\n` (either the last numstat row's own
    // newline, or -- for a commit that changed no files -- the blank-line
    // separator git still emits before an empty numstat block), so
    // splitting on '\n' always leaves one trailing empty string that is
    // pure split artifact, never content. Drop it before peeling numstat
    // rows off the end, or it would immediately (and wrongly) look like
    // "the last line isn't a numstat row, stop".
    while lines.last() == Some(&"") {
        lines.pop();
    }
    let mut stats = Vec::new();
    while let Some(&last) = lines.last() {
        match parse_numstat_line(last) {
            Some(row) => {
                stats.push(row);
                lines.pop();
            }
            None => break,
        }
    }
    stats.reverse();
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    (lines.join("\n"), stats)
}

/// Parses one `--numstat` line (`"<added>\t<removed>\t<path>"`). `added`/
/// `removed` are either all-ASCII-digits or exactly `"-"` (binary file, no
/// line count -- reported as `0`); anything else (not a numstat line at
/// all -- e.g. a body line that happens to start a paragraph) is `None`.
fn parse_numstat_line(line: &str) -> Option<StatLine> {
    let mut parts = line.splitn(3, '\t');
    let added = parts.next()?;
    let removed = parts.next()?;
    let path = parts.next()?;
    if path.is_empty() {
        return None;
    }
    let parse_count = |s: &str| -> Option<i64> {
        if s == "-" {
            Some(0)
        } else if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let added = parse_count(added)?;
    let removed = parse_count(removed)?;
    Some(StatLine { path: path.to_string(), added, removed })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch git repo under the system temp dir, unique per test
    /// (mirrors `cli.rs`'s `ScratchDir`: no `tempfile` dependency, cleaned
    /// up on drop). `-c user.name=t -c user.email=t@t -c commit.gpgsign=false`
    /// plus `GIT_CONFIG_GLOBAL=/dev/null` keep fixture commits independent
    /// of whatever the host's real git config says.
    struct FixtureRepo {
        path: PathBuf,
    }

    impl FixtureRepo {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mach-kb-git-test-{}-{}-{}", std::process::id(), tag, n));
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

        /// Like `commit`, but with an explicit author and a controlled
        /// author/committer date (`GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE`,
        /// ISO 8601 e.g. `"2026-08-05T10:00:00+00:00"`) -- `log_month`'s
        /// grouping and `months`'s distinct-month listing both need commits
        /// deterministically placed in a chosen month, not whatever moment
        /// the test happened to run at.
        fn commit_dated(&self, msg: &str, author: &str, date: &str) {
            self.commit_dated_with_body(msg, None, author, date);
        }

        fn commit_dated_with_body(&self, subject: &str, body: Option<&str>, author: &str, date: &str) {
            self.git(&["add", "-A"]);
            let mut cmd = Command::new("git");
            cmd.arg("-C")
                .arg(&self.path)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .args(["-c", &format!("user.name={}", author), "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(["commit", "--quiet", "--no-gpg-sign", "-m", subject]);
            if let Some(b) = body {
                cmd.args(["-m", b]);
            }
            let status = cmd.status().unwrap();
            assert!(status.success(), "git commit --date failed");
        }

        fn repo(&self) -> Repo {
            Repo::new(self.path.clone())
        }

        fn rev_parse(&self, rev: &str) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(["rev-parse", rev])
                .output()
                .unwrap();
            assert!(out.status.success(), "git rev-parse {} failed", rev);
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
    }

    impl Drop for FixtureRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn head_returns_the_current_commit_sha() {
        let fx = FixtureRepo::new("head");
        fx.write("a.txt", b"one\n");
        fx.commit("first");
        let expected = fx.rev_parse("HEAD");
        assert_eq!(fx.repo().head().unwrap(), expected);
        assert_eq!(expected.len(), 40, "HEAD should resolve to a full sha: {:?}", expected);
    }

    #[test]
    fn head_errors_when_there_is_no_commit_yet() {
        let fx = FixtureRepo::new("head-empty");
        let err = fx.repo().head().unwrap_err();
        assert!(matches!(err, KbError::Other(_)));
    }

    #[test]
    fn tracked_files_lists_paths_with_their_blob_shas() {
        let fx = FixtureRepo::new("tracked");
        fx.write("a.txt", b"hello\n");
        fx.write("dir/b.txt", b"world\n");
        fx.commit("first");

        let files = fx.repo().tracked_files().unwrap();
        let mut paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        paths.sort();
        assert_eq!(paths, vec!["a.txt", "dir/b.txt"]);

        // Blob sha must be the real git blob hash for that content, not a
        // placeholder -- `git hash-object` gives us the independently
        // computed value to compare against.
        let a = files.iter().find(|f| f.path == "a.txt").unwrap();
        let out = Command::new("git")
            .arg("-C")
            .arg(&fx.path)
            .args(["hash-object", "a.txt"])
            .output()
            .unwrap();
        let expected_blob = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(a.blob, expected_blob);
    }

    #[test]
    fn diff_reports_added_modified_deleted_and_renamed_files() {
        let fx = FixtureRepo::new("diff");
        fx.write("keep.txt", b"unchanged\n");
        fx.write("modify.txt", b"before\n");
        fx.write("delete.txt", b"gone soon\n");
        // Rename detection needs enough shared content to clear git's
        // default 50% similarity threshold.
        fx.write("rename_from.txt", b"line one\nline two\nline three\nline four\nline five\n");
        fx.commit("base");
        let from = fx.rev_parse("HEAD");

        fx.write("modify.txt", b"after\n");
        std::fs::remove_file(fx.path.join("delete.txt")).unwrap();
        std::fs::rename(fx.path.join("rename_from.txt"), fx.path.join("rename_to.txt")).unwrap();
        fx.write("added.txt", b"new file\n");
        fx.commit("changes");
        let to = fx.rev_parse("HEAD");

        let mut changes = fx.repo().diff(&from, &to).unwrap();
        changes.sort_by_key(|c| format!("{:?}", c));

        assert!(changes.contains(&Change::Added("added.txt".to_string())), "{:?}", changes);
        assert!(changes.contains(&Change::Modified("modify.txt".to_string())), "{:?}", changes);
        assert!(changes.contains(&Change::Deleted("delete.txt".to_string())), "{:?}", changes);
        assert!(
            changes.contains(&Change::Renamed { from: "rename_from.txt".to_string(), to: "rename_to.txt".to_string() }),
            "{:?}",
            changes
        );
        assert!(!changes.iter().any(|c| matches!(c, Change::Modified(p) if p == "keep.txt")), "{:?}", changes);
        assert_eq!(changes.len(), 4, "{:?}", changes);
    }

    #[test]
    fn is_binary_is_true_for_a_blob_with_an_early_nul_and_false_for_text() {
        let fx = FixtureRepo::new("binary");
        fx.write("text.txt", b"just plain text\n");
        fx.write("blob.bin", &[b'X', b'Y', 0u8, b'Z']);
        fx.commit("first");

        let repo = fx.repo();
        let files = repo.tracked_files().unwrap();
        let text_blob = &files.iter().find(|f| f.path == "text.txt").unwrap().blob;
        let bin_blob = &files.iter().find(|f| f.path == "blob.bin").unwrap().blob;

        assert!(!repo.is_binary(text_blob).unwrap());
        assert!(repo.is_binary(bin_blob).unwrap());
    }

    #[test]
    fn show_at_an_older_sha_returns_the_old_content() {
        let fx = FixtureRepo::new("show");
        fx.write("f.txt", b"version one\n");
        fx.commit("first");
        let old_sha = fx.rev_parse("HEAD");

        fx.write("f.txt", b"version two\n");
        fx.commit("second");

        let repo = fx.repo();
        assert_eq!(repo.show(&old_sha, "f.txt").unwrap(), "version one\n");
        assert_eq!(repo.show("HEAD", "f.txt").unwrap(), "version two\n");
    }

    #[test]
    fn show_never_resolves_a_range_or_reflog_spec_hidden_in_the_path() {
        // Regression (final review): `git show <sha>:<path>` parsed a path of
        // `..stash@{0}` as the range `<sha>:..stash@{0}` and printed the
        // stash commit -- a read of a ref the index never chose to expose.
        // `cat-file blob` takes exactly one object name, never a range.
        let fx = FixtureRepo::new("show-escape");
        fx.write("f.txt", b"tracked\n");
        fx.commit("first");
        fx.git(&["branch", "engine-rewrite"]);
        fx.write("f.txt", b"uncommitted stash content\n");
        fx.git(&["stash", "--quiet"]);
        let head = fx.rev_parse("HEAD");
        let repo = fx.repo();
        for evil in ["..stash@{0}", "..engine-rewrite", "../f.txt"] {
            match repo.show(&head, evil) {
                Ok(text) => panic!("{:?} must not resolve to anything, got {:?}", evil, text),
                Err(e) => assert!(!e.to_string().contains("uncommitted stash content"), "{}", e),
            }
        }
        assert_eq!(repo.show(&head, "f.txt").unwrap(), "tracked\n");
    }

    #[test]
    fn is_commit_is_true_for_a_real_commit_and_false_for_garbage_or_a_blob() {
        let fx = FixtureRepo::new("is-commit");
        fx.write("f.txt", b"x\n");
        fx.commit("first");
        let repo = fx.repo();
        let head = fx.rev_parse("HEAD");
        assert!(repo.is_commit(&head));
        assert!(!repo.is_commit(&"d".repeat(40)));
        assert!(!repo.is_commit("not-a-sha"));
        let blob = repo.tracked_files().unwrap().remove(0).blob;
        assert!(!repo.is_commit(&blob), "a blob sha is an object, but not a commit");
    }

    #[test]
    fn tracked_files_reflect_committed_content_not_the_staging_index() {
        // Item 8: the file list and blob shas come from `ls-tree -r <head>`,
        // so a staged-but-uncommitted change can't make the blob recorded
        // for a file disagree with the content `show(head, ..)` returns.
        let fx = FixtureRepo::new("tracked-committed");
        fx.write("a.txt", b"committed\n");
        fx.commit("first");
        let committed = fx.repo().tracked_files().unwrap();
        fx.write("a.txt", b"staged only\n");
        fx.write("new.txt", b"staged new file\n");
        fx.git(&["add", "-A"]);
        let after = fx.repo().tracked_files().unwrap();
        assert_eq!(committed, after, "staged changes must not show up");
        let head = fx.rev_parse("HEAD");
        assert_eq!(fx.repo().tracked_files_at(&head).unwrap(), committed);
    }

    #[test]
    fn is_repo_is_false_for_a_plain_directory_and_true_for_a_git_repo() {
        let fx = FixtureRepo::new("is-repo");
        assert!(Repo::is_repo(&fx.path));

        let plain = std::env::temp_dir().join(format!("mach-kb-git-test-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        assert!(!Repo::is_repo(&plain));
        std::fs::remove_dir_all(&plain).ok();
    }

    #[test]
    fn ignored_returns_only_the_paths_that_match_gitignore_rules() {
        let fx = FixtureRepo::new("ignored");
        fx.write(".gitignore", b"*.log\nbuild/\n");
        fx.write("keep.txt", b"kept\n");
        fx.commit("first");
        // build/ and *.log needn't exist on disk for check-ignore to match
        // -- it evaluates the pathname against the patterns, not the tree.

        let repo = fx.repo();
        let ignored = repo.ignored(["keep.txt", "debug.log", "build/out.o", "src/main.rs"]).unwrap();
        assert_eq!(ignored, HashSet::from(["debug.log".to_string(), "build/out.o".to_string()]));
    }

    #[test]
    fn ignored_flags_a_tracked_file_that_gitignore_later_came_to_match() {
        // Regression: plain `git check-ignore` (no --no-index) refuses to
        // report a path as ignored while it is still in the git index --
        // which every already-committed file always is. That made
        // guard_files's "removes gitignored files" functionally inert for
        // real repos: a directory added to .gitignore *after* its files
        // were committed (e.g. `build/` after `build/out.o` was checked
        // in) would keep being reported as not-ignored forever.
        // `--no-index` evaluates purely by pathname pattern, regardless of
        // tracked state, which is the only thing that actually helps here.
        let fx = FixtureRepo::new("ignored-tracked-later");
        fx.write("build/out.o", b"stale artifact\n");
        fx.commit("first, before .gitignore exists");
        fx.write(".gitignore", b"build/\n");
        fx.commit("add .gitignore after the fact -- out.o stays tracked");

        let repo = fx.repo();
        let ignored = repo.ignored(["build/out.o", "keep.txt"]).unwrap();
        assert!(ignored.contains("build/out.o"), "a tracked file must still be flagged once its directory matches .gitignore: {:?}", ignored);
    }

    #[test]
    fn ignored_of_an_empty_list_is_an_empty_set_not_an_error() {
        let fx = FixtureRepo::new("ignored-empty");
        fx.write("a.txt", b"x\n");
        fx.commit("first");
        let empty: Vec<&str> = Vec::new();
        assert_eq!(fx.repo().ignored(empty).unwrap(), HashSet::new());
    }

    #[test]
    fn dir_stats_aggregates_files_lines_commits_and_authors_per_directory() {
        let fx = FixtureRepo::new("dirstats");
        fx.write("src/a.rs", b"line1\nline2\nline3\n");
        fx.commit("add a");
        fx.write("src/sub/b.rs", b"x\ny\n");
        fx.commit("add b");
        fx.write("docs/readme.md", b"hello\n");
        fx.commit("add docs");

        let stats = fx.repo().dir_stats(2).unwrap();
        let by_dir: HashMap<String, &DirStat> = stats.iter().map(|s| (s.dir.clone(), s)).collect();

        // depth 2 includes "src", "src/sub" and "docs", but nothing deeper.
        let src = by_dir.get("src").expect("src present");
        assert_eq!(src.files, 2, "src should count a.rs and src/sub/b.rs");
        assert_eq!(src.lines, 5);
        assert_eq!(src.commits, 2);
        assert_eq!(src.authors, vec!["t".to_string()]);
        assert!(!src.first_added.is_empty());

        let src_sub = by_dir.get("src/sub").expect("src/sub present");
        assert_eq!(src_sub.files, 1);
        assert_eq!(src_sub.lines, 2);
        assert_eq!(src_sub.commits, 1);

        let docs = by_dir.get("docs").expect("docs present");
        assert_eq!(docs.files, 1);
        assert_eq!(docs.lines, 1);
    }

    #[test]
    fn dir_stats_depth_zero_returns_no_directories() {
        let fx = FixtureRepo::new("dirstats-zero");
        fx.write("src/a.rs", b"x\n");
        fx.commit("first");
        assert_eq!(fx.repo().dir_stats(0).unwrap(), Vec::new());
    }

    #[test]
    fn months_lists_distinct_committer_date_months_newest_first() {
        let fx = FixtureRepo::new("months");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug one", "t", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("aug two", "t", "2026-08-20T10:00:00+00:00");
        fx.write("b.txt", b"1\n");
        fx.commit_dated("sep one", "t", "2026-09-03T10:00:00+00:00");
        assert_eq!(fx.repo().months().unwrap(), vec!["2026-09".to_string(), "2026-08".to_string()]);
    }

    #[test]
    fn log_month_returns_only_that_months_commits_oldest_first_with_author_subject_body() {
        let fx = FixtureRepo::new("log-month-basic");
        fx.write("a.txt", b"1\n");
        fx.commit_dated_with_body("aug one", Some("body line1\nbody line2"), "alice", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("aug two", "bob", "2026-08-20T10:00:00+00:00");
        fx.write("b.txt", b"1\n");
        fx.commit_dated("sep one", "carol", "2026-09-03T10:00:00+00:00");

        let commits = fx.repo().log_month("2026-08").unwrap();
        assert_eq!(commits.len(), 2, "{:?}", commits);
        assert_eq!(commits[0].subject, "aug one", "oldest of the month must come first");
        assert_eq!(commits[0].author, "alice");
        assert!(commits[0].date.starts_with("2026-08-05"));
        assert_eq!(commits[0].body, "body line1\nbody line2");
        assert_eq!(commits[1].subject, "aug two");
        assert_eq!(commits[1].author, "bob");
        assert_eq!(commits[1].body, "", "no -m body given, must be empty not a stray blank line");
        assert!(!commits.iter().any(|c| c.subject == "sep one"), "{:?}", commits);
    }

    #[test]
    fn log_month_reports_per_file_numstat_including_binary_files_as_zero() {
        let fx = FixtureRepo::new("log-month-numstat");
        fx.write("a.txt", b"one\ntwo\nthree\n");
        fx.write("bin.dat", &[0u8, 1, 2, 3]);
        fx.commit_dated("first", "t", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"one\ntwo\nthree\nfour\n");
        fx.commit_dated("second", "t", "2026-08-06T10:00:00+00:00");

        let commits = fx.repo().log_month("2026-08").unwrap();
        assert_eq!(commits.len(), 2, "{:?}", commits);
        let first_paths: std::collections::HashSet<&str> = commits[0].stat_lines.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(first_paths, std::collections::HashSet::from(["a.txt", "bin.dat"]));
        let a = commits[0].stat_lines.iter().find(|s| s.path == "a.txt").unwrap();
        assert_eq!((a.added, a.removed), (3, 0));
        let bin = commits[0].stat_lines.iter().find(|s| s.path == "bin.dat").unwrap();
        assert_eq!((bin.added, bin.removed), (0, 0), "a binary file's '-' counts must read as zero, not fail to parse");

        let second = &commits[1].stat_lines;
        assert_eq!(second.len(), 1);
        assert_eq!((second[0].path.as_str(), second[0].added, second[0].removed), ("a.txt", 1, 0));
    }

    #[test]
    fn log_month_returns_nothing_for_a_month_with_no_commits() {
        let fx = FixtureRepo::new("log-month-empty");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("only commit", "t", "2026-08-05T10:00:00+00:00");
        assert_eq!(fx.repo().log_month("2026-01").unwrap(), Vec::new());
    }

    #[test]
    fn log_month_preserves_a_multiline_body_untruncated_body_is_gits_job_not_this_ones() {
        // A body long enough to span many lines must come back whole --
        // truncating to a preview length is `code_index::history`'s job.
        let fx = FixtureRepo::new("log-month-long-body");
        fx.write("a.txt", b"1\n");
        let body: String = (1..=30).map(|n| format!("line {}", n)).collect::<Vec<_>>().join("\n");
        fx.commit_dated_with_body("subject", Some(&body), "t", "2026-08-05T10:00:00+00:00");
        let commits = fx.repo().log_month("2026-08").unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].body, body);
        assert_eq!(commits[0].body.lines().count(), 30);
    }

    #[test]
    fn log_path_returns_only_commits_touching_that_path_newest_first() {
        let fx = FixtureRepo::new("log-path-basic");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("touch a", "alice", "2026-08-05T10:00:00+00:00");
        fx.write("b.txt", b"1\n");
        fx.commit_dated("touch b", "bob", "2026-08-06T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("touch a again", "carol", "2026-09-01T10:00:00+00:00");

        let commits = fx.repo().log_path("a.txt", 15).unwrap();
        assert_eq!(commits.len(), 2, "{:?}", commits);
        assert_eq!(commits[0].subject, "touch a again", "newest first");
        assert_eq!(commits[0].author, "carol");
        assert!(commits[0].date.starts_with("2026-09-01"));
        assert_eq!(commits[1].subject, "touch a");
        assert_eq!(commits[1].author, "alice");
        assert_eq!(commits[1].sha.len(), 40);
    }

    #[test]
    fn log_path_returns_nothing_for_a_path_never_touched() {
        let fx = FixtureRepo::new("log-path-empty");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("touch a", "alice", "2026-08-05T10:00:00+00:00");
        assert_eq!(fx.repo().log_path("never/touched.txt", 15).unwrap(), Vec::new());
    }

    #[test]
    fn log_path_respects_the_n_cap() {
        let fx = FixtureRepo::new("log-path-cap");
        for i in 1..=5 {
            fx.write("a.txt", format!("{}\n", i).as_bytes());
            fx.commit_dated(&format!("edit {}", i), "t", &format!("2026-08-0{}T10:00:00+00:00", i));
        }
        let commits = fx.repo().log_path("a.txt", 2).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "edit 5");
        assert_eq!(commits[1].subject, "edit 4");
    }

    #[test]
    fn month_index_groups_every_sha_by_month_newest_first() {
        let fx = FixtureRepo::new("month-index");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug one", "t", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("aug two", "t", "2026-08-20T10:00:00+00:00");
        fx.write("b.txt", b"1\n");
        fx.commit_dated("sep one", "t", "2026-09-03T10:00:00+00:00");
        let idx = fx.repo().month_index().unwrap();
        let months: Vec<&str> = idx.months.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(months, vec!["2026-09", "2026-08"]);
        assert_eq!(idx.months[1].1.len(), 2);
        assert!(idx.months.iter().all(|(_, shas)| shas.iter().all(|s| s.len() == 40)));
    }

    #[test]
    fn log_commits_reads_exactly_the_given_shas_oldest_first_with_numstat() {
        let fx = FixtureRepo::new("log-commits");
        fx.write("a.txt", b"1\n");
        fx.commit_dated_with_body("aug one", Some("body"), "alice", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"1\n2\n");
        fx.commit_dated("aug two", "bob", "2026-08-20T10:00:00+00:00");
        fx.write("b.txt", b"1\n");
        fx.commit_dated("sep one", "carol", "2026-09-03T10:00:00+00:00");
        let repo = fx.repo();
        let idx = repo.month_index().unwrap();
        let aug = &idx.months.iter().find(|(m, _)| m == "2026-08").unwrap().1;
        let commits = repo.log_commits(aug).unwrap();
        assert_eq!(commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), vec!["aug one", "aug two"]);
        assert_eq!(commits[0].body, "body");
        assert_eq!(commits[1].stat_lines, vec![StatLine { path: "a.txt".to_string(), added: 1, removed: 0 }]);
        assert_eq!(commits, repo.log_month("2026-08").unwrap(), "same records as the full-history month read");
        assert_eq!(repo.numstat_calls(), 1);
        assert!(repo.log_commits(&["HEAD".to_string()]).is_err(), "only hex shas are accepted");
    }

    #[test]
    fn log_path_lists_touched_files_and_matches_the_path_literally() {
        let fx = FixtureRepo::new("log-path-files");
        fx.write("src/a.txt", b"1\n");
        fx.write("src/b.txt", b"1\n");
        fx.write("other.txt", b"1\n");
        fx.commit_dated("touch src and other", "alice", "2026-08-05T10:00:00+00:00");
        let repo = fx.repo();
        let commits = repo.log_path("src", 15).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].files, vec!["src/a.txt".to_string(), "src/b.txt".to_string()], "only files under the path");
        // A glob is matched literally: no file is literally named "*.txt".
        assert!(repo.log_path("*.txt", 15).unwrap().is_empty());
        assert!(repo.log_path(":(glob)**", 15).unwrap().is_empty());
    }
}
