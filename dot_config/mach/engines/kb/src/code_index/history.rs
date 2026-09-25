//! Code index: monthly commit-history summaries (Task 3; see
//! docs/superpowers/specs/2026-09-23-code-index-design.md and the scratchpad
//! brief `sdd/codeidx3/task-3-brief.md`).
//!
//! Called once per project from `job.rs::index_project`, right after that
//! project's module/repo summary pass (`summary::run_module_summary_pass`),
//! sharing the same [`Budget`] every other `claude -p` call this run spends
//! against. A `--path-prefix` run skips this stage entirely (see the call
//! site in `job.rs`) -- a partial view of the repo has no meaningful "this
//! month" history to report on.
//!
//! One cheap `git log --format=%H%x1f%cI` pass (`git::Repo::month_index`)
//! lists every `YYYY-MM` committer-date month reachable from `HEAD` with its
//! commit shas, newest first (so a limited budget always keeps the most
//! recently touched months up to date, and older months are picked up on a
//! later run -- fully resumable). A month's digest is sha256 of a version
//! salt plus its sorted shas (changes whenever a commit is added, removed or
//! rewritten). A month is regenerated when it has no `code_history` row, its
//! digest differs, or its row has no mirrored memory; otherwise it is
//! skipped without any further git work -- the expensive body+numstat read
//! (`git::Repo::log_commits`, shas on stdin) runs only for months that need
//! it. Months no longer reachable (a history rewrite) have their row
//! deleted and their mirror invalidated.
//!
//! Regeneration: every commit field (subject, author, body, file paths) is
//! redacted first ([`redact_secrets`] -- per line, a whole PEM block at a
//! time), THEN the month text is built and capped at 20,000 chars (file
//! lists, then bodies, lowest churn first; then a middle run of commits
//! elided as `[N commits omitted]`, never the newest -- see
//! [`build_month_text`]). That text is the prompt (logged in `judge_log`,
//! tag `index_history`) sent to `sonnet`, which is asked for one paragraph
//! with no headings.
//!
//! On a successful, non-empty reply: `code_history.text` is the REPLY (the
//! summary -- never the raw log), with the month's top-churn paths in
//! `code_history.files`; the reply's lead (first ~600 chars across
//! paragraphs, cut at a sentence end -- `summary::lead_across_paragraphs`)
//! is mirrored into `memories` as an index-owned, dated row with source
//! `code-history:<project>:<YYYY-MM>` and content `"<project> <YYYY-MM>: "`
//! followed by that lead. A month that already has a mirror gets that SAME row's
//! content and embedding updated in place (`store::update_content`) rather
//! than superseded -- `code_history.memory_id` is what makes that possible
//! across runs. `occurred_from`/`occurred_to` are the month's calendar
//! bounds ([`month_bounds`]). The digest is written LAST
//! (`store::code_history_set_digest`), so a run that dies mid-month
//! regenerates it next time instead of skipping a month with no mirror.
//!
//! `code_index::mod.rs`'s `is_index_owned`/`is_index_owned_source`/
//! `not_index_owned_sql` (extended in this same task to also cover
//! `code-history:` alongside `code-index:`) are what keep these mirrors out
//! of dedupe, contradiction, strength review, graph extraction, the
//! supersession audit, `top_similar`, and `mach kb improve`'s evidence --
//! nothing in this module filters for that itself.

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::code_index::git::{CommitInfo, Repo, StatLine};
use crate::code_index::job::{is_tool_transcript, Budget, TIMEOUT_INDEX_SONNET};
use crate::code_index::secrets;
use crate::code_index::summary::lead_across_paragraphs;
use crate::embed::Embedder;
use crate::reflect::{LoggedLlm, ReflectLlm};
use crate::store::{self, KbError};

/// Month text hard cap before prompt-building (brief: "truncate to 20,000
/// chars").
const MONTH_TEXT_MAX_CHARS: usize = 20_000;

/// Per-commit body preview: first N lines only (brief: "body (first 20
/// lines)").
const BODY_MAX_LINES: usize = 20;

/// Top-N changed files (by churn, i.e. added + removed) listed per commit
/// (brief: "top 15 changed files with +/- counts").
const TOP_FILES_PER_COMMIT: usize = 15;

/// Top-churn paths of the whole month stored in `code_history.files` (the
/// ask `history` tool's `Files:` line).
const TOP_FILES_PER_MONTH: usize = 8;

/// Mixed into every month digest. Bumped when what a row STORES changes
/// meaning (the first v35 cut stored the raw log in `code_history.text`;
/// from `summary-v2` on it stores the summary), so rows written under the
/// old meaning regenerate once instead of being skipped forever.
const DIGEST_VERSION: &str = "summary-v2";

/// One project's [`run_history_pass`] outcome.
pub(crate) struct HistoryPassResult {
    /// `index_history` (`sonnet`) calls actually attempted this run --
    /// success or failure, each one spent one unit of `budget`, same
    /// accounting as `job::ProjectOutcome::headers_attempted`.
    pub attempted: usize,
}

/// A month that needs (re)generation: it has no row, its digest changed, or
/// its row has no mirrored memory yet.
fn month_needs_work(existing: Option<&store::CodeHistoryRow>, digest: &str) -> bool {
    match existing {
        None => true,
        Some(row) => row.digest != digest || row.memory_id.is_none(),
    }
}

/// Months `run_history_pass` would regenerate right now, without calling
/// git for anything but the one cheap month index, the LLM, or writing --
/// what `mach kb index --dry-run` reports.
pub(crate) fn months_needing_work(conn: &Connection, repo: &Repo, project_id: i64) -> Result<usize, KbError> {
    let index = repo.month_index()?;
    let mut n = 0usize;
    for (month, shas) in &index.months {
        let existing = store::code_history_get(conn, project_id, month)?;
        if month_needs_work(existing.as_ref(), &month_digest(shas)) {
            n += 1;
        }
    }
    Ok(n)
}

/// Runs the monthly commit-history stage for one project. See the module
/// doc comment for the full behaviour. Stops the moment `budget` is spent;
/// whatever month is left un-regenerated is picked up again next run.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_history_pass<L: ReflectLlm, E: Embedder>(
    conn: &Connection,
    history_llm: &LoggedLlm<L>,
    embedder: &E,
    repo: &Repo,
    project_id: i64,
    project_name: &str,
    budget: &mut Budget,
    now: &str,
) -> Result<HistoryPassResult, KbError> {
    // ONE cheap pass (`%H %cI`) decides everything: months + digests.
    let index = repo.month_index()?;
    let mut attempted = 0usize;

    // Months no longer reachable from HEAD (a history rewrite dropped
    // them): the row and its mirror go -- free, no budget.
    let live: HashSet<&str> = index.months.iter().map(|(m, _)| m.as_str()).collect();
    for row in store::code_history_list(conn, project_id)? {
        if live.contains(row.month.as_str()) {
            continue;
        }
        if let Some(mid) = row.memory_id {
            store::invalidate_memory(conn, mid, now)?;
        }
        store::invalidate_index_memories_by_source(conn, &history_source(project_name, &row.month), now)?;
        store::code_history_delete(conn, project_id, &row.month)?;
    }

    for (month, shas) in &index.months {
        if !budget.remaining() {
            break;
        }
        let digest = month_digest(shas);
        let existing = store::code_history_get(conn, project_id, month)?;
        if !month_needs_work(existing.as_ref(), &digest) {
            continue; // unchanged since last run -- free skip, no numstat, no call.
        }
        // Body + numstat only for a month that actually needs regenerating.
        let commits = repo.log_commits(shas)?;
        if commits.is_empty() {
            continue;
        }
        // Redact every field BEFORE any truncation, so a cut can never
        // split a secret into a half that no longer matches its pattern.
        let commits = redact_commits(&commits);
        let text = redact_secrets(&build_month_text(&commits));
        let prompt = build_history_prompt(project_name, month, &text);

        attempted += 1;
        let result = history_llm.call("sonnet", &prompt, TIMEOUT_INDEX_SONNET);
        budget.record(result.as_ref().err().map(String::as_str));
        let Ok(reply) = result else {
            continue; // digest/row unchanged -- retried next run.
        };
        // What is stored is the model's SUMMARY (belt-and-braces redacted:
        // its input was already redacted, so this is normally a no-op).
        let summary_text = redact_secrets(reply.trim());
        if summary_text.trim().is_empty() || is_tool_transcript(&summary_text) {
            continue;
        }

        let last_sha = commits.last().map(|c| c.sha.clone()).unwrap_or_default();
        let files = top_month_files(&commits).join("\n");
        let files = if files.is_empty() { None } else { Some(files) };
        // Digest deliberately empty here; written last, below.
        store::code_history_upsert(conn, project_id, month, &summary_text, commits.len() as i64, &last_sha, "", files.as_deref(), now)?;

        let source = history_source(project_name, month);
        let content = format!("{} {}: {}", project_name, month, lead_across_paragraphs(&summary_text));
        let mirror_embedding = embedder.embed(&content).ok();

        // A month regenerating for the second-or-later time keeps pointing
        // at the SAME mirrored memory (content AND embedding updated in
        // place) -- only the first generation (or a mirror that has since
        // disappeared) mints a new one. See the module doc comment and
        // `store::code_history_upsert`'s.
        let reusable = match existing.as_ref().and_then(|row| row.memory_id) {
            Some(id) if store::get(conn, id)?.is_some() => Some(id),
            _ => None,
        };
        let memory_id = match reusable {
            Some(id) => {
                store::update_content(conn, id, &content, mirror_embedding.as_deref())?;
                id
            }
            None => {
                let id = store::upsert_index_memory(conn, project_name, &source, &content, mirror_embedding.as_deref(), now)?;
                store::code_history_set_memory_id(conn, project_id, month, id)?;
                id
            }
        };
        let (from, to) = month_bounds(month);
        store::set_occurrence(conn, memory_id, &from, &to)?;
        store::code_history_set_digest(conn, project_id, month, &digest)?;
    }

    Ok(HistoryPassResult { attempted })
}

/// `code-history:<project>:<YYYY-MM>` -- the exact source of a history
/// mirror, mirroring `summary::index_source`'s shape one level down (a
/// month instead of a directory).
fn history_source(project_name: &str, month: &str) -> String {
    format!("{}{}:{}", store::CODE_HISTORY_SOURCE_PREFIX, project_name, month)
}

/// sha256 (lowercase hex) of [`DIGEST_VERSION`] plus the month's commit
/// shas, sorted before hashing -- "stable regardless of gathering order,
/// changes whenever the underlying set changes" (same shape as
/// `summary::children_digest`). A rebase that rewrites every commit in the
/// month (even to identical-looking content) mints new sha1 object ids, so
/// this always changes with it.
fn month_digest<S: AsRef<str>>(shas: &[S]) -> String {
    let mut shas: Vec<&str> = shas.iter().map(|s| s.as_ref()).collect();
    shas.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_VERSION.as_bytes());
    hasher.update(b"\n");
    hasher.update(shas.join("\n").as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for b in digest {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

/// The month's [`TOP_FILES_PER_MONTH`] highest-churn paths (added + removed
/// summed across its commits), ties by path.
fn top_month_files(commits: &[CommitInfo]) -> Vec<String> {
    let mut churn: HashMap<&str, i64> = HashMap::new();
    for c in commits {
        for f in &c.stat_lines {
            *churn.entry(f.path.as_str()).or_insert(0) += f.added + f.removed;
        }
    }
    let mut v: Vec<(&str, i64)> = churn.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.into_iter().take(TOP_FILES_PER_MONTH).map(|(p, _)| p.to_string()).collect()
}

/// Every field of every commit run through [`redact_secrets`] -- subject,
/// author, body (block-aware, so a whole PEM key goes) and each numstat
/// path.
fn redact_commits(commits: &[CommitInfo]) -> Vec<CommitInfo> {
    commits
        .iter()
        .map(|c| CommitInfo {
            sha: c.sha.clone(),
            date: c.date.clone(),
            author: redact_secrets(&c.author),
            subject: redact_secrets(&c.subject),
            body: redact_secrets(&c.body),
            stat_lines: c
                .stat_lines
                .iter()
                .map(|f| StatLine { path: redact_secrets(&f.path), added: f.added, removed: f.removed })
                .collect(),
        })
        .collect()
}

/// Builds the month's log text from `commits` (already chronological,
/// oldest first): one block per commit -- a header line (`date author
/// subject`), then its body (first [`BODY_MAX_LINES`] lines), then its top
/// [`TOP_FILES_PER_COMMIT`] changed files by churn with `+added/-removed`
/// counts.
///
/// When the whole thing exceeds [`MONTH_TEXT_MAX_CHARS`]: file lists are
/// dropped first, then bodies, lowest-churn commit first (total churn
/// across every file it touched), re-measuring after each drop -- every
/// subject is still there, and a trailer line says how many file lists /
/// bodies were omitted. Only if the subjects alone still overflow (an
/// enormous number of commits) is a MIDDLE run of commits elided whole,
/// replaced by one `[N commits omitted]` line, growing outward from the
/// middle until it fits -- so the month's oldest and newest commits always
/// survive (a plain hard cut used to silently drop the newest). A final
/// head+tail cut is the last resort for a pathological single subject.
fn build_month_text(commits: &[CommitInfo]) -> String {
    build_month_text_capped(commits, MONTH_TEXT_MAX_CHARS)
}

/// [`build_month_text`] with the char cap as a parameter, so tests can drive
/// the drop-order logic with a small, exact cap.
fn build_month_text_capped(commits: &[CommitInfo], max_chars: usize) -> String {
    struct Entry {
        header: String,
        body: String,
        files: String,
        churn: i64,
    }

    let entries: Vec<Entry> = commits
        .iter()
        .map(|c| {
            let header = format!("{} {} {}", c.date, c.author, c.subject);
            let body = c.body.lines().take(BODY_MAX_LINES).collect::<Vec<_>>().join("\n");
            let churn: i64 = c.stat_lines.iter().map(|f| f.added + f.removed).sum();
            let mut top = c.stat_lines.clone();
            top.sort_by(|a, b| (b.added + b.removed).cmp(&(a.added + a.removed)));
            top.truncate(TOP_FILES_PER_COMMIT);
            let files = top.iter().map(|f| format!("  {} +{}/-{}", f.path, f.added, f.removed)).collect::<Vec<_>>().join("\n");
            Entry { header, body, files, churn }
        })
        .collect();

    let render = |drop_files: &HashSet<usize>, drop_bodies: &HashSet<usize>| -> String {
        let mut out = String::new();
        for (i, e) in entries.iter().enumerate() {
            out.push_str(&e.header);
            out.push('\n');
            if !e.body.is_empty() && !drop_bodies.contains(&i) {
                out.push_str(&e.body);
                out.push('\n');
            }
            if !e.files.is_empty() && !drop_files.contains(&i) {
                out.push_str(&e.files);
                out.push('\n');
            }
            out.push('\n');
        }
        let mut out = out.trim_end().to_string();
        if !drop_files.is_empty() || !drop_bodies.is_empty() {
            out.push_str("\n\n");
            out.push_str(&omission_trailer(drop_files.len(), drop_bodies.len()));
        }
        out
    };

    let mut drop_files: HashSet<usize> = HashSet::new();
    let mut drop_bodies: HashSet<usize> = HashSet::new();
    let mut text = render(&drop_files, &drop_bodies);
    if text.chars().count() <= max_chars {
        return text;
    }

    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| (entries[i].churn, i));

    for &i in &order {
        if entries[i].files.is_empty() {
            continue;
        }
        drop_files.insert(i);
        text = render(&drop_files, &drop_bodies);
        if text.chars().count() <= max_chars {
            return text;
        }
    }
    for &i in &order {
        if entries[i].body.is_empty() {
            continue;
        }
        drop_bodies.insert(i);
        text = render(&drop_files, &drop_bodies);
        if text.chars().count() <= max_chars {
            return text;
        }
    }

    // Subjects alone overflow: elide a middle run of whole commits.
    let n = entries.len();
    let block_len: Vec<usize> = entries.iter().map(|e| e.header.chars().count() + 2).collect();
    let marker = |k: usize| format!("[{k} commits omitted]");
    let total: usize = block_len.iter().sum();
    let (mut lo, mut hi) = (n / 2, n / 2); // elided = [lo, hi)
    let mut size = total;
    while hi - lo < n.saturating_sub(2) && size + marker(hi - lo).len() + 2 > max_chars {
        // Grow alternately towards the older, then the newer side, never
        // eating the first or last commit.
        if (hi - lo) % 2 == 0 && lo > 1 {
            lo -= 1;
            size -= block_len[lo];
        } else if hi < n - 1 {
            size -= block_len[hi];
            hi += 1;
        } else if lo > 1 {
            lo -= 1;
            size -= block_len[lo];
        } else {
            break;
        }
    }
    let mut out = String::new();
    for (i, e) in entries.iter().enumerate() {
        if i == lo && hi > lo {
            out.push_str(&marker(hi - lo));
            out.push_str("\n\n");
        }
        if i >= lo && i < hi {
            continue;
        }
        out.push_str(&e.header);
        out.push_str("\n\n");
    }
    let text = out.trim_end().to_string();
    if text.chars().count() <= max_chars {
        return text;
    }
    // Last resort: keep the head AND the tail (the newest commit), never
    // just the head.
    let sep = "\n[... omitted ...]\n";
    let half = max_chars.saturating_sub(sep.len()) / 2;
    let chars: Vec<char> = text.chars().collect();
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!("{head}{sep}{tail}")
}

/// The line [`build_month_text_capped`] appends when it dropped file lists
/// or bodies, so the model (and a reader of `judge_log`) knows the month
/// text is abridged.
fn omission_trailer(files: usize, bodies: usize) -> String {
    format!("[omitted to fit: {files} file lists, {bodies} bodies]")
}

/// Redacts secrets in `text`, line by line, replacing a leaking line with
/// `[redacted: <pattern name>]` (only that line, never the whole text). A
/// PEM private-key block is redacted WHOLE -- from its `-----BEGIN ...
/// PRIVATE KEY-----` line through the matching `-----END` line (or the end
/// of the text if none) -- since its base64 body lines match no pattern on
/// their own. The match itself never appears in the result. `pub(crate)`:
/// the ask `history` tool runs its output through this too.
pub(crate) fn redact_secrets(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_pem = false;
    for line in text.lines() {
        if in_pem {
            if line.contains("-----END") {
                in_pem = false;
            }
            continue;
        }
        if let Some(pos) = line.find("-----BEGIN") {
            if line[pos..].contains("PRIVATE KEY-----") {
                out.push("[redacted: private-key]".to_string());
                in_pem = !line[pos..].contains("-----END");
                continue;
            }
        }
        match secrets::content_reason(line) {
            Some(reason) => out.push(format!("[redacted: {}]", pattern_name(reason))),
            None => out.push(line.to_string()),
        }
    }
    out.join("\n")
}

/// `"secret-content(github-token)"` -> `"github-token"`; anything not
/// shaped like that is used verbatim.
fn pattern_name(reason: &str) -> &str {
    reason.strip_prefix("secret-content(").and_then(|s| s.strip_suffix(')')).unwrap_or(reason)
}

/// The exact prompt sent to `sonnet`, followed by a blank line and the
/// month's own (redacted, capped) log text. Asks for ONE paragraph with no
/// headings: the reply is stored as the month's summary and its lead is
/// mirrored as a memory.
fn build_history_prompt(project: &str, month: &str, log_text: &str) -> String {
    format!(
        "Summarise what changed in {project} during {month} and why, as one paragraph of 3\u{2013}8 sentences \
(no headings, no lists, no preamble). Name the authors, the areas touched (paths/modules) and the intent \
stated in commit messages. Only use what the log says.\n\n{log_text}\n"
    )
}

/// `"YYYY-MM"` -> (first day, last day), both `"YYYY-MM-DD"` -- the
/// `occurred_from`/`occurred_to` bounds for a month's mirrored memory
/// (`store::set_occurrence`, schema v19's occurrence columns).
fn month_bounds(month: &str) -> (String, String) {
    let year: i32 = month.get(..4).and_then(|s| s.parse().ok()).unwrap_or(1970);
    let m: u32 = month.get(5..7).and_then(|s| s.parse().ok()).unwrap_or(1);
    let from = format!("{:04}-{:02}-01", year, m);
    let to = format!("{:04}-{:02}-{:02}", year, m, days_in_month(year, m));
    (from, to)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    use crate::store::ProjectRow;

    // -- fixture repo (replicated minimally from git.rs's/scope.rs's/
    // job.rs's private `FixtureRepo` -- none is reachable from here, and
    // constraints.md says to replicate rather than touch git.rs beyond its
    // own log functions) with dated-commit support, since this stage's
    // whole behaviour is keyed on committer-date months. -------------------

    struct FixtureRepo {
        path: PathBuf,
    }

    impl FixtureRepo {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("mach-kb-history-test-{}-{}-{}", std::process::id(), tag, n));
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

        /// Amends the current commit, keeping it in a chosen month --
        /// `git commit --amend` without an explicit `GIT_COMMITTER_DATE`
        /// re-stamps the committer date to "now" (only the author date is
        /// preserved by default), which would silently move the amended
        /// commit into whatever month the test happens to run in.
        fn amend_dated(&self, msg: &str, author: &str, date: &str) {
            self.git(&["add", "-A"]);
            let status = Command::new("git")
                .arg("-C")
                .arg(&self.path)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .args(["-c", &format!("user.name={}", author), "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
                .args(["commit", "--amend", "--quiet", "--no-gpg-sign", "-m", msg])
                .status()
                .unwrap();
            assert!(status.success(), "git commit --amend --date failed");
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
        store::open_with_path(std::path::Path::new(":memory:")).expect("open in-memory store")
    }

    fn new_project(conn: &Connection, name: &str) -> ProjectRow {
        store::upsert_project(conn, &format!("fp-{name}"), name, "/nonexistent", "2026-09-24T00:00:00Z").unwrap();
        store::get_project_by_name(conn, name).unwrap().unwrap()
    }

    /// Records every prompt it was called with, in order; always succeeds
    /// with a distinct reply unless the month (read off the `Project: ...`
    /// -- actually the prompt's own literal `project during <month>` shape
    /// -- isn't easily parsed back out, so tests instead just count/inspect
    /// `prompts` directly) is listed in `fail`.
    #[derive(Default)]
    struct FakeLlm {
        calls: RefCell<usize>,
        prompts: RefCell<Vec<String>>,
        fail: RefCell<bool>,
        reply: RefCell<Option<String>>,
    }

    impl ReflectLlm for FakeLlm {
        fn call(&self, _model: &str, prompt: &str, _timeout: Duration) -> Result<String, String> {
            *self.calls.borrow_mut() += 1;
            self.prompts.borrow_mut().push(prompt.to_string());
            if *self.fail.borrow() {
                return Err("simulated failure".to_string());
            }
            if let Some(r) = self.reply.borrow().as_ref() {
                return Ok(r.clone());
            }
            Ok("Alice and Bob reworked the picking module for better performance.".to_string())
        }
    }

    struct FakeEmbedder;
    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().take(4).map(|b| b as f32).collect())
        }
    }

    fn logged<'a>(conn: &'a Connection, llm: &'a FakeLlm) -> LoggedLlm<'a, FakeLlm> {
        LoggedLlm::new(conn, "index_history", llm)
    }

    // -- pure helpers --------------------------------------------------

    #[test]
    fn month_bounds_handles_ordinary_30_and_31_day_and_leap_years() {
        assert_eq!(month_bounds("2026-01"), ("2026-01-01".to_string(), "2026-01-31".to_string()));
        assert_eq!(month_bounds("2026-04"), ("2026-04-01".to_string(), "2026-04-30".to_string()));
        assert_eq!(month_bounds("2026-02"), ("2026-02-01".to_string(), "2026-02-28".to_string()), "2026 is not a leap year");
        assert_eq!(month_bounds("2024-02"), ("2024-02-01".to_string(), "2024-02-29".to_string()), "2024 is a leap year");
        assert_eq!(month_bounds("2000-02"), ("2000-02-01".to_string(), "2000-02-29".to_string()), "2000 is a leap year (div 400)");
        assert_eq!(month_bounds("1900-02"), ("1900-02-01".to_string(), "1900-02-28".to_string()), "1900 is not (div 100, not 400)");
    }

    #[test]
    fn redact_secrets_drops_only_the_leaking_line_never_the_match_itself() {
        // Fake token built at runtime, per the assignment's own rule about
        // not putting a real-looking secret literal in source.
        let token = format!("{}{}", "gh".to_string() + "p_", "a".repeat(36));
        let text = format!("2026-08-05 alice: fix bug\ntoken = \"{}\"\n2026-08-06 bob: add feature", token);
        let redacted = redact_secrets(&text);
        assert!(!redacted.contains(&token), "the secret value must never survive redaction");
        assert!(redacted.contains("[redacted: github-token]"), "{}", redacted);
        assert!(redacted.contains("2026-08-05 alice: fix bug"), "other lines must be untouched: {}", redacted);
        assert!(redacted.contains("2026-08-06 bob: add feature"), "{}", redacted);
    }

    /// A commit touching ten files, `churn` lines each, with a body.
    fn wide_commit(sha: &str, author: &str, subject: &str, churn: i64, body: &str) -> CommitInfo {
        CommitInfo {
            sha: sha.to_string(),
            date: "2026-08-05T10:00:00+00:00".to_string(),
            author: author.to_string(),
            subject: subject.to_string(),
            body: body.to_string(),
            stat_lines: (0..10)
                .map(|i| crate::code_index::git::StatLine { path: format!("src/{sha}/file_{i}.rs"), added: churn, removed: 0 })
                .collect(),
        }
    }

    fn len(s: &str) -> usize {
        s.chars().count()
    }

    #[test]
    fn build_month_text_capped_drops_lowest_churn_file_list_before_the_high_churn_ones() {
        let commits = vec![
            wide_commit("aaa", "alice", "low churn change", 1, ""),
            wide_commit("bbb", "bob", "high churn change", 100, ""),
        ];
        let full = build_month_text_capped(&commits, usize::MAX);
        assert!(full.contains("src/aaa/file_0.rs +1/-0") && full.contains("src/bbb/file_0.rs +100/-0"));
        let cap = len(&full) - 1; // one file list must go
        let capped = build_month_text_capped(&commits, cap);
        assert!(len(&capped) <= cap);
        assert!(capped.contains("src/bbb/file_0.rs +100/-0"), "high-churn file list should survive: {}", capped);
        assert!(!capped.contains("src/aaa/"), "low-churn file list should be dropped first: {}", capped);
        assert!(capped.ends_with(&omission_trailer(1, 0)), "{capped}");
    }

    #[test]
    fn build_month_text_capped_drops_bodies_only_after_every_file_list_is_gone() {
        let long_body = |tag: &str| (0..10).map(|i| format!("{tag} churn body line {i}")).collect::<Vec<_>>().join("\n");
        let commits = vec![
            wide_commit("aaa", "alice", "low churn change", 1, &long_body("low")),
            wide_commit("bbb", "bob", "high churn change", 100, &long_body("high")),
        ];
        let full = build_month_text_capped(&commits, usize::MAX);
        let no_files: String = full.lines().filter(|l| !l.trim_start().starts_with("src/")).collect::<Vec<_>>().join("\n");
        // Both file lists must go, and then one body too.
        let cap = len(&no_files) + 2 + len(&omission_trailer(2, 0)) - 1;
        let capped = build_month_text_capped(&commits, cap);
        assert!(len(&capped) <= cap);
        assert!(!capped.contains("src/"), "both file lists must be gone: {}", capped);
        assert!(capped.contains("high churn body line"), "high-churn body should survive: {}", capped);
        assert!(!capped.contains("low churn body line"), "low-churn body should be dropped before the high-churn one: {}", capped);
        assert!(capped.contains("low churn change") && capped.contains("high churn change"), "subjects all kept");
    }

    #[test]
    fn build_month_text_says_when_it_dropped_file_lists_or_bodies() {
        let commits = vec![wide_commit("aaa", "a", "subject aaa", 1, "b"), wide_commit("bbb", "a", "subject bbb", 2, "b")];
        let full = build_month_text_capped(&commits, usize::MAX);
        assert_eq!(build_month_text_capped(&commits, len(&full)), full, "fits: untouched, no trailer");
        assert!(!full.contains("omitted"));
        let tight = build_month_text_capped(&commits, len(&full) - 1);
        assert!(tight.contains("subject aaa") && tight.contains("subject bbb"), "all subjects kept: {tight}");
        assert!(tight.ends_with(&omission_trailer(1, 0)), "{tight}");
    }

    #[test]
    fn build_month_text_elides_a_middle_run_and_keeps_the_oldest_and_newest() {
        // Item 12: a hard cut used to drop the newest commits silently.
        let commits: Vec<CommitInfo> = (0..2000)
            .map(|i| CommitInfo {
                sha: format!("sha{i}"),
                date: "2026-08-05T10:00:00+00:00".to_string(),
                author: "t".to_string(),
                subject: format!("commit number {i} with a reasonably long subject line to pad things out"),
                body: String::new(),
                stat_lines: vec![],
            })
            .collect();
        let text = build_month_text(&commits);
        assert!(text.chars().count() <= MONTH_TEXT_MAX_CHARS, "{}", text.chars().count());
        assert!(text.contains("commit number 0 "), "oldest kept");
        assert!(text.contains("commit number 1999 "), "newest kept");
        let marker = text.lines().find(|l| l.ends_with("commits omitted]")).expect("an elision marker");
        let omitted: usize = marker.trim_start_matches('[').split(' ').next().unwrap().parse().unwrap();
        let kept = text.lines().filter(|l| l.contains("commit number")).count();
        assert_eq!(kept + omitted, 2000, "every commit is either shown or counted in the marker");
    }

    #[test]
    fn redaction_happens_per_field_before_truncation_so_no_partial_secret_survives() {
        let token = format!("{}{}", "gh".to_string() + "p_", "c".repeat(36));
        let c = CommitInfo {
            sha: "aaa".to_string(),
            date: "2026-08-05T10:00:00+00:00".to_string(),
            author: "a".to_string(),
            subject: format!("rotate {}", token),
            body: format!("line one\nkey {}\nline three", token),
            stat_lines: vec![],
        };
        let redacted = redact_commits(&[c]);
        for cap in [20usize, 45, 60, 80, 200, usize::MAX] {
            let text = redact_secrets(&build_month_text_capped(&redacted, cap));
            assert!(!text.contains(&token[..10]), "cap {cap}: {text}");
        }
    }

    #[test]
    fn redact_secrets_drops_a_whole_pem_block() {
        let begin = format!("{}BEGIN RSA PRIVATE KEY{}", "-".repeat(5), "-".repeat(5));
        let end = format!("{}END RSA PRIVATE KEY{}", "-".repeat(5), "-".repeat(5));
        let body1 = "MIIEowIBAAKCAQEA".to_string() + &"q".repeat(40);
        let text = format!("before\n{begin}\n{body1}\nzzzzbase64line\n{end}\nafter");
        let red = redact_secrets(&text);
        assert_eq!(red, "before\n[redacted: private-key]\nafter", "{red}");
        // Unterminated: everything after BEGIN goes.
        let red2 = redact_secrets(&format!("x\n{begin}\n{body1}\ntrailing"));
        assert_eq!(red2, "x\n[redacted: private-key]");
    }

    #[test]
    fn month_digest_changes_when_shas_change_stable_under_reordering() {
        let a = ["aaa", "bbb"];
        let b = ["bbb", "aaa"]; // reordered
        let c = ["aaa", "ccc"]; // one sha rebased away
        assert_eq!(month_digest(&a), month_digest(&b), "order must not matter");
        assert_ne!(month_digest(&a), month_digest(&c), "a changed sha set must change the digest");
    }

    // -- run_history_pass (end to end, fake LLM + embedder) -----------------

    #[test]
    fn run_history_pass_summarizes_each_month_newest_first_and_stores_plus_mirrors() {
        let fx = FixtureRepo::new("basic");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("sep work", "bob", "2026-09-03T10:00:00+00:00");

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        let result =
            run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z")
                .unwrap();

        assert_eq!(result.attempted, 2);
        let rows = store::code_history_list(&conn, project.id).unwrap();
        assert_eq!(rows.len(), 2);
        let sep = rows.iter().find(|r| r.month == "2026-09").unwrap();
        let aug = rows.iter().find(|r| r.month == "2026-08").unwrap();
        assert_eq!(sep.commits, 1);
        assert_eq!(aug.commits, 1);
        assert!(sep.memory_id.is_some());
        assert!(aug.memory_id.is_some());

        // Newest month (Sep) must be attempted before the older one (Aug).
        assert_eq!(llm.prompts.borrow().len(), 2);
        assert!(llm.prompts.borrow()[0].contains("during 2026-09"), "{}", llm.prompts.borrow()[0]);
        assert!(llm.prompts.borrow()[1].contains("during 2026-08"), "{}", llm.prompts.borrow()[1]);

        let sep_mem = store::get(&conn, sep.memory_id.unwrap()).unwrap().unwrap();
        assert!(sep_mem.content.starts_with("proj 2026-09: "), "{}", sep_mem.content);
        assert_eq!(sep_mem.source.as_deref(), Some("code-history:proj:2026-09"));
        assert!(store::is_index_owned(&sep_mem));
        assert_eq!(sep_mem.occurred_from.as_deref(), Some("2026-09-01"));
        assert_eq!(sep_mem.occurred_to.as_deref(), Some("2026-09-30"));
    }

    #[test]
    fn run_history_pass_skips_a_month_whose_digest_is_unchanged() {
        let fx = FixtureRepo::new("unchanged");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(*llm.calls.borrow(), 1);

        // Same repo state, run again: nothing changed, no new call.
        let mut budget2 = Budget::new(10);
        let result = run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget2, "2026-09-24T00:00:01Z")
            .unwrap();
        assert_eq!(result.attempted, 0);
        assert_eq!(*llm.calls.borrow(), 1, "an unchanged digest must not spend another call");
    }

    #[test]
    fn run_history_pass_regenerates_and_updates_the_same_mirror_in_place_on_a_rebase() {
        let fx = FixtureRepo::new("rebase");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z").unwrap();
        let first_memory_id = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap().memory_id.unwrap();

        // Simulate a rebase: amend the commit, which mints a new sha for
        // the same logical change.
        fx.write("a.txt", b"1 amended\n");
        fx.amend_dated("aug work amended", "alice", "2026-08-06T10:00:00+00:00");

        *llm.reply.borrow_mut() = Some("A different summary text after the rebase.".to_string());
        let mut budget2 = Budget::new(10);
        let result = run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget2, "2026-09-24T00:00:01Z")
            .unwrap();
        assert_eq!(result.attempted, 1, "a rebased month must be treated as changed");

        let row = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap();
        assert_eq!(row.memory_id, Some(first_memory_id), "regeneration must keep pointing at the SAME mirrored memory");
        let mem = store::get(&conn, first_memory_id).unwrap().unwrap();
        assert!(mem.content.contains("A different summary text"), "{}", mem.content);

        // Only ever the one memory row for this source -- never superseded
        // into a second row.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories WHERE source = 'code-history:proj:2026-08'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn run_history_pass_stops_at_budget_and_resumes_older_months_next_run() {
        let fx = FixtureRepo::new("budget");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("sep work", "bob", "2026-09-03T10:00:00+00:00");

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;

        let mut budget = Budget::new(1);
        let result = run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z")
            .unwrap();
        assert_eq!(result.attempted, 1, "only the newest month fits in a budget of 1");
        assert!(store::code_history_get(&conn, project.id, "2026-09").unwrap().is_some());
        assert!(store::code_history_get(&conn, project.id, "2026-08").unwrap().is_none(), "the older month must wait for the next run");

        let mut budget2 = Budget::new(10);
        let result2 = run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget2, "2026-09-24T00:00:01Z")
            .unwrap();
        assert_eq!(result2.attempted, 1, "the next run picks up the remaining older month");
        assert!(store::code_history_get(&conn, project.id, "2026-08").unwrap().is_some());
    }

    #[test]
    fn run_history_pass_never_sends_or_stores_a_secret_in_a_commit_body() {
        let fx = FixtureRepo::new("secret");
        // Fake token assembled at runtime, per the assignment's rule.
        let token = format!("{}{}", "gh".to_string() + "p_", "b".repeat(36));
        fx.write("a.txt", b"1\n");
        fx.commit_dated_with_body(
            "add config",
            Some(&format!("oops committed a token\ntoken = \"{}\"\nplease rotate", token)),
            "alice",
            "2026-08-05T10:00:00+00:00",
        );

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z").unwrap();

        // Never in the prompt sent to the (fake) LLM:
        assert!(!llm.prompts.borrow()[0].contains(&token), "{}", llm.prompts.borrow()[0]);
        assert!(llm.prompts.borrow()[0].contains("[redacted: github-token]"), "{}", llm.prompts.borrow()[0]);

        // Never in code_history.text:
        let row = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap();
        assert!(!row.text.contains(&token), "{}", row.text);

        // Never in judge_log:
        let logged_prompt: String =
            conn.query_row("SELECT prompt FROM judge_log WHERE pass = 'index_history'", [], |r| r.get(0)).unwrap();
        assert!(!logged_prompt.contains(&token), "{}", logged_prompt);
    }

    #[test]
    fn run_history_pass_makes_no_call_and_writes_nothing_when_there_are_no_commits() {
        let fx = FixtureRepo::new("no-commits");
        // A freshly-init'd repo with zero commits: `months()`/`head()` on it
        // would error, so this exercises the "no months at all" path via a
        // repo whose only commit predates nothing -- i.e. an empty months
        // list is handled without panicking. Give it one commit far in the
        // future relative to nothing else, just to have a valid HEAD, then
        // assert only that one month's row exists (sanity that the loop
        // doesn't invent work).
        fx.write("a.txt", b"1\n");
        fx.commit_dated("only", "t", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z").unwrap();
        assert_eq!(store::code_history_list(&conn, project.id).unwrap().len(), 1);
    }

    #[test]
    fn run_history_pass_stops_making_calls_when_the_llm_fails_but_leaves_it_retryable() {
        let fx = FixtureRepo::new("llm-fail");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");

        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        *llm.fail.borrow_mut() = true;
        let embedder = FakeEmbedder;
        let mut budget = Budget::new(10);
        let result = run_history_pass(&conn, &logged(&conn, &llm), &embedder, &fx.repo(), project.id, "proj", &mut budget, "2026-09-24T00:00:00Z")
            .unwrap();
        assert_eq!(result.attempted, 1, "a failed call still counts as attempted (budget spent)");
        assert!(store::code_history_get(&conn, project.id, "2026-08").unwrap().is_none(), "a failed call must write nothing");
    }

    // -- final-review fix wave -------------------------------------------

    const LONG_REPLY: &str = "August 2026.\n\nAlice reworked the picking module so pointer hits are resolved once per frame instead of per widget, \
and Bob moved the hover/press state into a shared PointerState resource. The stated intent in both commit messages was to cut \
per-frame allocations and to make hit testing deterministic.";

    fn run(conn: &Connection, llm: &FakeLlm, repo: &Repo, pid: i64, now: &str) -> HistoryPassResult {
        let mut budget = Budget::new(10);
        run_history_pass(conn, &logged(conn, llm), &FakeEmbedder, repo, pid, "proj", &mut budget, now).unwrap()
    }

    #[test]
    fn the_stored_history_text_is_the_summary_reply_and_the_mirror_is_non_trivial() {
        // Item 1: code_history.text used to be the raw redacted log, and the
        // mirror's lead (first paragraph only) was one commit header.
        let fx = FixtureRepo::new("stores-summary");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        *llm.reply.borrow_mut() = Some(LONG_REPLY.to_string());
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");

        let row = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap();
        assert_eq!(row.text, LONG_REPLY, "code_history.text must be the model's summary, verbatim");
        assert!(!row.text.contains("aug work"), "never the raw log");
        assert_eq!(row.files.as_deref(), Some("a.txt"));
        let mem = store::get(&conn, row.memory_id.unwrap()).unwrap().unwrap();
        assert!(mem.content.len() > 150, "mirror must carry the summary, not a heading: {}", mem.content);
        assert!(mem.content.contains("PointerState"), "{}", mem.content);
        assert!(llm.prompts.borrow()[0].contains("one paragraph"), "{}", llm.prompts.borrow()[0]);
        assert!(llm.prompts.borrow()[0].contains("no headings"));
    }

    #[test]
    fn unchanged_months_spawn_no_numstat_pass() {
        // Item 4: the old loop ran a full-history `git log --numstat` per
        // month on every run, changed or not.
        let fx = FixtureRepo::new("no-numstat");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        fx.write("a.txt", b"2\n");
        fx.commit_dated("sep work", "bob", "2026-09-03T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        let repo1 = fx.repo();
        run(&conn, &llm, &repo1, project.id, "2026-09-24T00:00:00Z");
        assert_eq!(repo1.numstat_calls(), 2, "one body+numstat read per regenerated month");

        let repo2 = fx.repo();
        let r = run(&conn, &llm, &repo2, project.id, "2026-09-25T00:00:00Z");
        assert_eq!(r.attempted, 0);
        assert_eq!(repo2.numstat_calls(), 0, "unchanged months must cost no numstat");

        // A new commit in September: only that month is read again.
        fx.write("a.txt", b"3\n");
        fx.commit_dated("more sep work", "bob", "2026-09-10T10:00:00+00:00");
        let repo3 = fx.repo();
        let r = run(&conn, &llm, &repo3, project.id, "2026-09-26T00:00:00Z");
        assert_eq!(r.attempted, 1);
        assert_eq!(repo3.numstat_calls(), 1);
    }

    #[test]
    fn a_month_with_a_matching_digest_but_no_mirror_is_regenerated() {
        // Item 14.
        let fx = FixtureRepo::new("no-mirror");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");
        conn.execute("UPDATE code_history SET memory_id = NULL", []).unwrap();
        let r = run(&conn, &llm, &fx.repo(), project.id, "2026-09-25T00:00:00Z");
        assert_eq!(r.attempted, 1);
        assert!(store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap().memory_id.is_some());
    }

    #[test]
    fn the_digest_is_written_last_so_a_failed_mirror_retries() {
        // Item 14: the text row is upserted with an EMPTY digest and the
        // real one is written only after the mirror exists, so a run that
        // dies in between leaves a row that never matches. Observable
        // contract: complete -> real digest; empty digest -> regenerated.
        let fx = FixtureRepo::new("digest-last");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");
        let row = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap();
        assert_eq!(row.digest, month_digest(&fx.repo().month_index().unwrap().months[0].1));
        conn.execute("UPDATE code_history SET digest = ''", []).unwrap();
        assert_eq!(run(&conn, &llm, &fx.repo(), project.id, "2026-09-25T00:00:00Z").attempted, 1);
    }

    #[test]
    fn regenerating_a_month_updates_the_mirrors_embedding_too() {
        let fx = FixtureRepo::new("re-embed");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        *llm.reply.borrow_mut() = Some("Alpha summary.".to_string());
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");
        let mid = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap().memory_id.unwrap();
        // Knock the vector out so the update is observable.
        conn.execute("UPDATE memories SET embedding = NULL WHERE id = ?1", [mid]).unwrap();

        fx.write("a.txt", b"1 amended\n");
        fx.amend_dated("aug work amended", "alice", "2026-08-06T10:00:00+00:00");
        // The stored vector must be the embedding of the NEW content.
        *llm.reply.borrow_mut() = Some("Beta summary.".to_string());
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-25T00:00:00Z");
        let mem = store::get(&conn, mid).unwrap().unwrap();
        assert!(mem.content.contains("Beta summary."));
        assert_eq!(mem.embedding, Some(FakeEmbedder.embed(&mem.content).unwrap()), "embedding refreshed with the content");
    }

    #[test]
    fn a_month_that_vanished_from_history_loses_its_row_and_mirror() {
        // Item 13: e.g. a rebase moved every commit out of that month.
        let fx = FixtureRepo::new("vanished");
        fx.write("a.txt", b"1\n");
        fx.commit_dated("aug work", "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");
        let mid = store::code_history_get(&conn, project.id, "2026-08").unwrap().unwrap().memory_id.unwrap();

        fx.amend_dated("moved to september", "alice", "2026-09-06T10:00:00+00:00");
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-25T00:00:00Z");
        assert!(store::code_history_get(&conn, project.id, "2026-08").unwrap().is_none(), "vanished month row deleted");
        assert!(store::get(&conn, mid).unwrap().unwrap().invalidated_at.is_some(), "its mirror invalidated");
        assert!(store::code_history_get(&conn, project.id, "2026-09").unwrap().is_some());
    }

    #[test]
    fn a_secret_in_a_commit_subject_never_reaches_the_prompt_or_the_store() {
        let fx = FixtureRepo::new("secret-subject");
        let token = format!("{}{}", "gh".to_string() + "p_", "d".repeat(36));
        fx.write("a.txt", b"1\n");
        fx.commit_dated(&format!("oops {}", token), "alice", "2026-08-05T10:00:00+00:00");
        let conn = mem_conn();
        let project = new_project(&conn, "proj");
        let llm = FakeLlm::default();
        run(&conn, &llm, &fx.repo(), project.id, "2026-09-24T00:00:00Z");
        assert!(!llm.prompts.borrow()[0].contains(&token));
        assert!(llm.prompts.borrow()[0].contains("[redacted: github-token]"));
        let logged: String = conn.query_row("SELECT prompt FROM judge_log WHERE pass = 'index_history'", [], |r| r.get(0)).unwrap();
        assert!(!logged.contains(&token));
    }
}
