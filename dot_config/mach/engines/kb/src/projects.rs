//! Project identity, drift arithmetic, and the derivable project card.
//!
//! Project identity used to be `basename(cwd)` verbatim, which broke two
//! ways. A rename orphaned every `project-index:<old>` memory, and letter
//! case alone already orphaned three projects on the live bank:
//! `~/programming/Expedite` tagged sessions "Expedite" while 28 index rows
//! sat under "expedite". Identity is therefore a fingerprint, and the name
//! is normalized.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Commits since the interpretive index ran before the map counts as stale.
///
/// PLACEHOLDER, chosen by intuition. Three similarity thresholds in this
/// codebase were set the same way and turned out to be unreachable, which
/// made whole passes dead code. Re-derive from observed per-project commit
/// rates once a few weeks of `projects` history exists.
pub const PROJECT_DRIFT_COMMITS: usize = 30;

/// Days since the interpretive index ran before the map counts as stale.
/// Same placeholder caveat as `PROJECT_DRIFT_COMMITS`.
pub const PROJECT_DRIFT_DAYS: f64 = 30.0;

/// What a project's identity is anchored to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fingerprint {
    /// Hash of the repository's first commit: survives renames, moves and
    /// remote changes, needs no file in the repo, works offline.
    GitRoot(String),
    /// No git available, so identity is the path and rename immunity is
    /// not possible. Stated rather than faked with a similarity heuristic.
    Path(PathBuf),
}

impl Fingerprint {
    /// The stored form, e.g. `git:1695fdb3...` or `path:/home/x/proj`.
    pub fn key(&self) -> String {
        match self {
            Fingerprint::GitRoot(h) => format!("git:{}", h),
            Fingerprint::Path(p) => format!("path:{}", p.display()),
        }
    }
}

/// The project key for a directory: its basename, lowercased.
pub fn normalize_name(root: &Path) -> String {
    root.file_name().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

/// Whether `root` is itself the toplevel of the git repository `git`
/// sees from it -- not merely somewhere inside one. Without this check, a
/// directory nested under a git repo (e.g. `~/programming` or `~/.config`
/// ever placed under git) reports the SAME root commit as its superproject,
/// so two unrelated sibling directories collapse onto one fingerprint and
/// `retag_project` can merge one project's whole index onto the other.
fn is_git_root(root: &Path) -> bool {
    let Some(toplevel) = git_output(root, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    let (Ok(root_canon), Ok(toplevel_canon)) = (std::fs::canonicalize(root), std::fs::canonicalize(&toplevel)) else {
        return false;
    };
    root_canon == toplevel_canon
}

/// Identity for a directory: the root commit when it is a git repository
/// rooted AT this directory, otherwise its path.
pub fn fingerprint_for(root: &Path) -> Fingerprint {
    match git_output(root, &["rev-list", "--max-parents=0", "HEAD"]) {
        // A repo with grafted or multiple roots prints several; the first
        // in reverse-chronological order is stable for a given history.
        Some(out) => match out.lines().next() {
            Some(h) if !h.is_empty() && is_git_root(root) => Fingerprint::GitRoot(h.to_string()),
            _ => Fingerprint::Path(root.to_path_buf()),
        },
        None => Fingerprint::Path(root.to_path_buf()),
    }
}

/// Current commit count on HEAD, when the directory is a git repository.
pub fn commit_count(root: &Path) -> Option<i64> {
    git_output(root, &["rev-list", "--count", "HEAD"])?.parse().ok()
}

/// How far the interpretive index has fallen behind the code.
#[derive(Debug, Clone, PartialEq)]
pub enum DriftState {
    /// No interpretive index has ever run for this project.
    NeverIndexed,
    /// Within both thresholds.
    Fresh,
    /// Past at least one threshold. `commits` is 0 for non-git projects.
    Drifted { commits: i64, days: f64 },
}

/// Drift from the recorded watermark. `commits` is `None` for projects with
/// no git, where age is the only available signal.
pub fn drift_state(
    indexed_commits: Option<i64>,
    current_commits: Option<i64>,
    indexed_at: Option<&str>,
    now: &str,
) -> DriftState {
    let Some(indexed_at) = indexed_at else {
        return DriftState::NeverIndexed;
    };
    // A malformed STORED timestamp is treated as NeverIndexed rather than
    // epoch-anchored (`unwrap_or(0)`): the old fallback silently meant
    // "drift forever" for a corrupted `indexed_at` with no visible sign
    // anything was wrong. NeverIndexed is loud (a distinct state in
    // `projects list`) and self-healing (the next `mark-indexed` overwrites
    // the bad timestamp with a good one). `now` is always freshly generated
    // by the caller via `store::now_rfc3339()`, never stored, so it keeps
    // the old epoch fallback rather than needing the same treatment.
    let Some(indexed_epoch) = crate::store::parse_rfc3339(indexed_at) else {
        return DriftState::NeverIndexed;
    };
    let days = crate::store::days_between(crate::store::parse_rfc3339(now).unwrap_or(0), indexed_epoch);
    let commits = match (indexed_commits, current_commits) {
        (Some(then), Some(now_c)) => (now_c - then).max(0),
        _ => 0,
    };
    if commits >= PROJECT_DRIFT_COMMITS as i64 || days >= PROJECT_DRIFT_DAYS {
        DriftState::Drifted { commits, days }
    } else {
        DriftState::Fresh
    }
}

/// Cap on a rendered card, so it competes fairly in `cli::pack_to_budget`
/// against entity cards rather than dominating an injection.
///
/// Set so a full-length card always fits the budget reservation that
/// `pack_to_budget` grants it: `est_tokens` is `chars/4`, so 520 chars is
/// at most 130 tokens, under the 133 that a third of the 400-token recall
/// budget allows. It was 1200 while the card was charged first against the
/// WHOLE budget — which is exactly how a long card came to evict three
/// ranked hits. Raising it above `4 * budget / 3` again would silently
/// start dropping whole cards instead of truncating them, so
/// `cli`'s `the_card_cap_and_the_budget_reservation_stay_consistent` test
/// pins the relationship.
pub const CARD_MAX_CHARS: usize = 520;

/// Manifest files worth naming, with the field to pull out of each.
const MANIFESTS: [&str; 6] =
    ["Cargo.toml", "package.json", "pyproject.toml", "go.mod", "Makefile", "CMakeLists.txt"];

/// Entry points worth naming, relative to the root.
const ENTRY_POINTS: [&str; 6] =
    ["src/main.rs", "src/lib.rs", "main.go", "src/index.ts", "src/index.tsx", "__main__.py"];

/// The derivable half of a project index: what can be read off the repo
/// with no LLM call, regenerated wholesale on every refresh.
///
/// Deliberately excludes anything interpretive (architecture, invariants,
/// workflows) — those stay memories, refreshed by an agentic diff, because
/// they cannot be recomputed and must not be silently rewritten.
pub fn build_card(root: &Path) -> String {
    let mut s = String::new();
    s.push_str(&format!("project {} at {}\n", normalize_name(root), root.display()));

    if let Some(remote) = git_output(root, &["remote", "get-url", "origin"]) {
        s.push_str(&format!("- remote: {}\n", remote));
    }
    if let Some(branch) = git_output(root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        s.push_str(&format!("- branch: {}\n", branch));
    }
    if let Fingerprint::GitRoot(h) = fingerprint_for(root) {
        s.push_str(&format!("- root commit: {}\n", &h[..h.len().min(8)]));
    }

    // Top-level layout, sorted so the card does not churn between runs —
    // `read_dir` order is not stable across runs or platforms.
    let mut dirs: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            if e.path().is_dir() {
                dirs.push(name);
            }
        }
    }
    dirs.sort();
    if !dirs.is_empty() {
        s.push_str(&format!("- layout: {}\n", dirs.join(", ")));
    }

    let present: Vec<&str> = MANIFESTS.iter().copied().filter(|m| root.join(m).is_file()).collect();
    if !present.is_empty() {
        s.push_str(&format!("- manifests: {}\n", present.join(", ")));
    }
    if let Some(name) = manifest_name(root) {
        s.push_str(&format!("- package name: {}\n", name));
    }

    let entries: Vec<&str> = ENTRY_POINTS.iter().copied().filter(|p| root.join(p).is_file()).collect();
    if !entries.is_empty() {
        s.push_str(&format!("- entry points: {}\n", entries.join(", ")));
    }
    if let Some(cmd) = test_command(root) {
        s.push_str(&format!("- test command: {}\n", cmd));
    }

    if s.len() > CARD_MAX_CHARS {
        // `truncate` panics if the cut point isn't a char boundary; a
        // non-ASCII path or manifest name could otherwise land mid-char.
        let mut cut = CARD_MAX_CHARS;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

/// The package name from whichever manifest is present, read with plain
/// string scanning rather than a parser per format — the card names things,
/// it does not need a full manifest model.
fn manifest_name(root: &Path) -> Option<String> {
    if let Ok(t) = std::fs::read_to_string(root.join("Cargo.toml")) {
        for line in t.lines() {
            if let Some(v) = line.strip_prefix("name") {
                return Some(v.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string());
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string(root.join("package.json")) {
        let v: serde_json::Value = serde_json::from_str(&t).ok()?;
        return v.get("name")?.as_str().map(|s| s.to_string());
    }
    if let Ok(t) = std::fs::read_to_string(root.join("go.mod")) {
        return t.lines().find_map(|l| l.strip_prefix("module ")).map(|m| m.trim().to_string());
    }
    None
}

/// Test command inferred from the manifest present.
fn test_command(root: &Path) -> Option<&'static str> {
    if root.join("Cargo.toml").is_file() {
        return Some("cargo test --workspace");
    }
    if root.join("package.json").is_file() {
        return Some("npm test");
    }
    if root.join("pyproject.toml").is_file() {
        return Some("pytest");
    }
    if root.join("go.mod").is_file() {
        return Some("go test ./...");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_name_lowercases_the_basename() {
        // The live bug this fixes: ~/programming/Expedite tagged sessions
        // "Expedite" while 28 index rows sat under "expedite".
        assert_eq!(normalize_name(Path::new("/home/x/programming/Expedite")), "expedite");
        assert_eq!(normalize_name(Path::new("/home/x/programming/LOD_Remake")), "lod_remake");
        assert_eq!(normalize_name(Path::new("/home/x/programming/umoja/")), "umoja");
    }

    #[test]
    fn a_non_git_directory_falls_back_to_a_path_fingerprint() {
        let dir = std::env::temp_dir().join(format!("mach-proj-nogit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fp = fingerprint_for(&dir);
        assert!(matches!(fp, Fingerprint::Path(_)), "no git means no root commit: {:?}", fp);
        assert!(fp.key().starts_with("path:"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_git_directory_fingerprints_to_its_head_and_counts_commits() {
        // Covers the git-success path for all three git-backed functions:
        // a typo turning `--count` into `--counts`, or a broken hash parse
        // in `fingerprint_for`, would compile fine and only show
        // up here — the non-git fallback test above never touches `git`.
        let dir = std::env::temp_dir().join(format!("mach-proj-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        git(&["init", "--quiet"]);
        // A fresh checkout has no identity configured; set one per-repo so
        // the commit doesn't fail on a machine with no global git config.
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        // --no-gpg-sign so a global commit.gpgsign=true can't break this.
        git(&["commit", "--allow-empty", "--no-gpg-sign", "-m", "one"]);

        // Ask for the root commit directly rather than through a helper:
        // with one commit HEAD and the root commit coincide, and the
        // assertion below is about the ROOT commit specifically.
        let root_commit = {
            let out = std::process::Command::new("git")
                .args(["rev-list", "--max-parents=0", "HEAD"])
                .current_dir(&dir)
                .output()
                .expect("git rev-list should run");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let fp = fingerprint_for(&dir);
        assert_eq!(fp, Fingerprint::GitRoot(root_commit.clone()));
        assert_eq!(commit_count(&dir), Some(1));

        git(&["commit", "--allow-empty", "--no-gpg-sign", "-m", "two"]);
        assert_eq!(commit_count(&dir), Some(2));
        // The root commit (what identity anchors to) doesn't move when a
        // second commit is added on top of it.
        assert_eq!(fingerprint_for(&dir), Fingerprint::GitRoot(root_commit));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_subdirectory_of_a_git_repo_fingerprints_as_its_own_path_not_the_repos_root_commit() {
        // Reproduces the live bug: repo, repo/sub_a and repo/sub_b all ran
        // `git` and got the SAME root commit back, because `fingerprint_for`
        // never checked that `root` itself is the repo's toplevel. Two
        // sibling non-git directories placed under one git superproject
        // would collapse onto one fingerprint and `retag_project` would then
        // merge one project's whole index onto the other.
        //
        // Unique per-call, not just per-process: a pid-only temp dir raced
        // with another test's fixture once already (see `fixture_tree`).
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let repo = std::env::temp_dir().join(format!("mach-proj-subdir-{}-{}", std::process::id(), n));
        let sub = repo.join("sub_a");
        std::fs::create_dir_all(&sub).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {:?} failed", args);
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        git(&["commit", "--allow-empty", "--no-gpg-sign", "-m", "one"]);

        // The repo root itself still fingerprints as GitRoot...
        assert!(matches!(fingerprint_for(&repo), Fingerprint::GitRoot(_)), "the repo's own root must still fingerprint as GitRoot");
        // ...but a subdirectory inside it, which is not itself a repo root,
        // must NOT inherit the superproject's root commit.
        let fp = fingerprint_for(&sub);
        assert_eq!(fp, Fingerprint::Path(sub.clone()), "a non-root subdirectory must fall back to a path fingerprint: {:?}", fp);

        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn drift_is_flagged_by_commits_or_by_days_whichever_trips() {
        let now = "2026-09-09T12:00:00Z";
        // Never indexed at all.
        assert_eq!(drift_state(None, Some(10), None, now), DriftState::NeverIndexed);
        // Fresh: few commits, recent.
        let fresh = drift_state(Some(100), Some(105), Some("2026-09-08T12:00:00Z"), now);
        assert_eq!(fresh, DriftState::Fresh);
        // Commit drift alone trips it.
        let by_commits = drift_state(Some(100), Some(100 + PROJECT_DRIFT_COMMITS as i64), Some("2026-09-08T12:00:00Z"), now);
        assert!(matches!(by_commits, DriftState::Drifted { commits, .. } if commits == PROJECT_DRIFT_COMMITS as i64));
        // Age alone trips it, with no commits at all (non-git projects).
        let by_age = drift_state(None, None, Some("2026-01-01T12:00:00Z"), now);
        assert!(matches!(by_age, DriftState::Drifted { .. }));
    }

    #[test]
    fn a_malformed_stored_timestamp_is_treated_as_never_indexed_rather_than_silently_drifting_forever() {
        // unwrap_or(0) on a malformed `indexed_at` used to epoch-anchor it,
        // producing a huge days figure -- effectively "drift forever" but
        // silently, with no sign anything was wrong. NeverIndexed is loud
        // (it shows up distinctly in `projects list`) and self-healing (the
        // next re-index rewrites the timestamp with a good one).
        let now = "2026-09-09T12:00:00Z";
        let malformed = drift_state(Some(100), Some(105), Some("not-a-timestamp"), now);
        assert_eq!(malformed, DriftState::NeverIndexed, "a malformed stored timestamp must not be treated as fresh, drifted, or epoch-anchored");
    }

    fn fixture_tree() -> PathBuf {
        // Per-call uniqueness, not just per-process: `cargo test` runs tests
        // in parallel threads sharing one pid, and two tests both call this
        // helper — a pid-only name let one test's cleanup delete the fixture
        // out from under the other mid-run.
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mach-card-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("tests")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.2.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("Makefile"), "build:\n\tcargo build\ntest:\n\tcargo test\n").unwrap();
        dir
    }

    #[test]
    fn the_card_states_layout_manifest_entry_point_and_test_command() {
        let dir = fixture_tree();
        let card = build_card(&dir);
        assert!(card.contains("src"), "layout: {}", card);
        assert!(card.contains("tests"));
        assert!(card.contains("Cargo.toml"));
        assert!(card.contains("fixture"), "manifest name");
        assert!(card.contains("src/main.rs"), "entry point");
        assert!(card.contains("cargo test"), "inferred test command");
        assert!(card.len() <= CARD_MAX_CHARS, "card must stay budget-sized: {}", card.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_root_yields_a_degenerate_card_the_caller_can_reject() {
        // `cmd_projects` refuses to overwrite a good card with this.
        let card = build_card(Path::new("/nonexistent/definitely-not-a-project-xyz"));
        assert!(card.lines().count() <= 1, "nothing to say about a directory that is not there: {:?}", card);
    }

    #[test]
    fn the_card_is_deterministic_across_rebuilds() {
        let dir = fixture_tree();
        assert_eq!(build_card(&dir), build_card(&dir), "unstable output would churn the card every run");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_layout_line_lists_directories_in_sorted_order_regardless_of_creation_order() {
        // Pins `dirs.sort()` directly. The determinism test above calls
        // `build_card` twice on unmodified state, so it would still pass if
        // `dirs.sort()` were deleted — `read_dir` order is stable across two
        // immediately-repeated calls, and that fixture's two directories
        // (`src`, `tests`) already happen to be alphabetical. This test
        // creates directories out of alphabetical order and asserts the
        // rendered order is sorted anyway.
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("mach-card-sort-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(dir.join("zebra")).unwrap();
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::create_dir_all(dir.join("middle")).unwrap();

        let card = build_card(&dir);
        let layout_line =
            card.lines().find(|l| l.starts_with("- layout:")).expect("layout line present");
        assert_eq!(
            layout_line, "- layout: alpha, middle, zebra",
            "layout must be sorted, not creation order: {}", card
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // Regression test for the `is_char_boundary` walk-back in
        // `build_card`. A naive `s.truncate(CARD_MAX_CHARS)` panics if the
        // cut lands mid-character; this builds a directory name long and
        // padded so that the raw (untruncated) first line straddles
        // CARD_MAX_CHARS in the middle of a 3-byte CJK character.
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // `pad` shifts where the multi-byte block starts by 1 byte at a
        // time. A 3-byte character has exactly 3 possible alignments
        // relative to a fixed cut point, so trying all three residues is
        // guaranteed to find one where CARD_MAX_CHARS lands mid-character.
        let mut chosen = None;
        for pad in 0..3 {
            let name = format!(
                "mach-card-utf8-{}-{}-{}{}",
                std::process::id(),
                n,
                "x".repeat(pad),
                "字".repeat(500),
            );
            let candidate = std::env::temp_dir().join(&name);
            let first_line = format!(
                "project {} at {}\n",
                normalize_name(&candidate),
                candidate.display()
            );
            if first_line.len() > CARD_MAX_CHARS && !first_line.is_char_boundary(CARD_MAX_CHARS) {
                chosen = Some(candidate);
                break;
            }
        }
        let dir = chosen.expect("one of the 3 byte alignments must straddle the cut point");
        // Deliberately not created on disk: the filename (~1500 bytes) would
        // exceed the filesystem's 255-byte name limit, and `build_card`'s
        // header line is pure string formatting that needs no real
        // directory — every filesystem call in the rest of the function
        // degrades to "absent" on a path that doesn't exist, same as the
        // `an_unreadable_root_...` test above.

        // The real assertion: this must not panic. A card is just a
        // String, so surviving to return one (within budget) is the proof
        // that the cut point was walked back to a char boundary.
        let card = build_card(&dir);
        assert!(card.len() <= CARD_MAX_CHARS, "card must stay budget-sized: {}", card.len());
        assert!(!card.is_empty());
    }

    #[test]
    fn a_package_json_project_names_itself_and_infers_npm_test() {
        let dir = std::env::temp_dir().join(format!("mach-card-js-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name": "fixture-js", "scripts": {"test": "vitest"}}"#,
        )
        .unwrap();
        let card = build_card(&dir);
        assert!(card.contains("fixture-js"), "manifest name: {}", card);
        assert!(card.contains("npm test"), "inferred test command: {}", card);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_go_mod_project_names_itself_and_infers_go_test() {
        let dir = std::env::temp_dir().join(format!("mach-card-go-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("go.mod"), "module github.com/example/fixture\n\ngo 1.22\n").unwrap();
        let card = build_card(&dir);
        assert!(card.contains("github.com/example/fixture"), "manifest name: {}", card);
        assert!(card.contains("go test ./..."), "inferred test command: {}", card);
        std::fs::remove_dir_all(&dir).ok();
    }
}
