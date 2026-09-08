
//! `mach kb health` — operational self-check + persistent-degradation
//! notification (`mach-health.timer` → `mach kb health --notify`).
//!
//! Split the same way `reflect.rs`/`ingest.rs` are: the pure aggregation
//! logic here (report formatting, pass/fail thresholds, the notification
//! streak decision) is unit-testable with no filesystem, network, or
//! subprocess involved; `cli::cmd_health` is the one impure caller that
//! actually pings ollama, opens the db, talks to the kb socket, shells out
//! to `df`, and writes the streak file.

/// One line of `mach kb health`'s report: a named check, whether it passed,
/// and a human-readable detail. `ok` decides both the report line's marker
/// and whether this check counts toward the overall nonzero exit code / the
/// `--notify` failing-set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// Plain-text report: one `[OK]`/`[FAIL]` line per check, in the order
/// given.
pub fn format_report(checks: &[Check]) -> String {
    let mut s = String::new();
    for c in checks {
        s.push_str(&format!("[{}] {}: {}\n", if c.ok { "OK" } else { "FAIL" }, c.name, c.detail));
    }
    s
}

/// Whether any check failed — decides `mach kb health`'s exit code.
pub fn any_failed(checks: &[Check]) -> bool {
    checks.iter().any(|c| !c.ok)
}

/// Names of every failing check, sorted — the `--notify` failing-set, and
/// the input to `streak_key`.
pub fn failing_names(checks: &[Check]) -> Vec<String> {
    let mut v: Vec<String> = checks.iter().filter(|c| !c.ok).map(|c| c.name.clone()).collect();
    v.sort();
    v
}

/// A stable, order-independent join of a failing-check-set into the key
/// persisted by the notification streak file — two runs with the exact same
/// set of failing checks (regardless of how `failing_names` happened to
/// enumerate them, which is already sorted) produce the same key.
pub fn streak_key(failing: &[String]) -> String {
    failing.join(",")
}

/// Persisted notification streak: the failing-check-set last notified about,
/// and when.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotifyStreak {
    pub key: String,
    pub last_notified_secs: u64,
}

/// Suppression window: a persistently-failing check-set is re-alerted at
/// most this often (failure-only philosophy — no success spam, but a
/// standing failure still surfaces daily rather than going silent forever
/// once the first notification has fired).
pub const NOTIFY_SUPPRESS_WINDOW_SECS: u64 = 24 * 60 * 60;

/// Whether `mach kb health --notify` should actually fire a desktop
/// notification this run, given the current failing-check set and the
/// streak persisted from the last time one fired: never when nothing is
/// failing; otherwise when the failing set differs from what was last
/// notified, or when `NOTIFY_SUPPRESS_WINDOW_SECS` has elapsed since it was.
pub fn should_notify(failing: &[String], last: Option<&NotifyStreak>, now_secs: u64) -> bool {
    if failing.is_empty() {
        return false;
    }
    let key = streak_key(failing);
    match last {
        None => true,
        Some(s) if s.key != key => true,
        Some(s) => now_secs.saturating_sub(s.last_notified_secs) >= NOTIFY_SUPPRESS_WINDOW_SECS,
    }
}

/// Reflect-completion staleness warning floor: `mach kb reflect` should
/// complete (see `store::ReflectState::last_completed_at`) at least this
/// often — the reflect timer's own `OnUnitInactiveSec=3h` re-arm makes
/// anything past this many hours a sign the timer or the process itself is
/// stuck, not just a laptop that's been asleep for an ordinary night.
pub const REFLECT_STALE_WARN_HOURS: f64 = 48.0;
/// telegram-state.json staleness warning floor — only checked when
/// `telegram.toml` exists at all (an unconfigured bridge has nothing to be
/// stale about). The long-poll loop backs off up to 5 minutes on its own
/// failures, so anything past an hour with no poll at all means the bridge
/// itself isn't running.
pub const TELEGRAM_STATE_STALE_WARN_HOURS: f64 = 1.0;
/// Disk-headroom warning floor for the filesystem holding `kb.db`.
pub const DISK_HEADROOM_WARN_BYTES: u64 = 1_000_000_000;

/// Whether the reflect-completion check passes: `None` (never completed)
/// always fails; otherwise passes at or under `REFLECT_STALE_WARN_HOURS`.
pub fn reflect_completion_ok(hours_since: Option<f64>) -> bool {
    matches!(hours_since, Some(h) if h <= REFLECT_STALE_WARN_HOURS)
}

/// Whether the telegram-state staleness check passes — same shape as
/// `reflect_completion_ok`, only ever consulted when telegram.toml exists.
pub fn telegram_state_ok(hours_since: Option<f64>) -> bool {
    matches!(hours_since, Some(h) if h <= TELEGRAM_STATE_STALE_WARN_HOURS)
}

/// Whether free disk space at the kb.db path clears the warning floor.
pub fn disk_headroom_ok(free_bytes: u64) -> bool {
    free_bytes >= DISK_HEADROOM_WARN_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, ok: bool) -> Check {
        Check { name: name.to_string(), ok, detail: "detail".to_string() }
    }

    // --- report aggregation ---

    #[test]
    fn format_report_renders_ok_and_fail_markers() {
        let checks = vec![check("ollama", true), check("kb.db", false)];
        let report = format_report(&checks);
        assert!(report.contains("[OK] ollama: detail\n"));
        assert!(report.contains("[FAIL] kb.db: detail\n"));
    }

    #[test]
    fn any_failed_true_only_when_some_check_fails() {
        assert!(!any_failed(&[check("a", true), check("b", true)]));
        assert!(any_failed(&[check("a", true), check("b", false)]));
        assert!(!any_failed(&[]));
    }

    #[test]
    fn failing_names_is_sorted_and_only_the_failures() {
        let checks = vec![check("zeta", false), check("alpha", true), check("mid", false)];
        assert_eq!(failing_names(&checks), vec!["mid".to_string(), "zeta".to_string()]);
    }

    #[test]
    fn streak_key_is_a_stable_join() {
        assert_eq!(streak_key(&["a".to_string(), "b".to_string()]), "a,b");
        assert_eq!(streak_key(&[]), "");
    }

    // --- notification streak suppression ---

    #[test]
    fn should_notify_never_fires_when_nothing_is_failing() {
        assert!(!should_notify(&[], None, 1000));
        let last = NotifyStreak { key: "ollama".to_string(), last_notified_secs: 0 };
        assert!(!should_notify(&[], Some(&last), 1000));
    }

    #[test]
    fn should_notify_fires_on_first_ever_failure() {
        assert!(should_notify(&["ollama".to_string()], None, 1000));
    }

    #[test]
    fn should_notify_suppresses_the_same_failing_set_within_the_window() {
        let last = NotifyStreak { key: "ollama".to_string(), last_notified_secs: 1000 };
        assert!(!should_notify(&["ollama".to_string()], Some(&last), 1000 + NOTIFY_SUPPRESS_WINDOW_SECS - 1));
    }

    #[test]
    fn should_notify_fires_again_once_the_window_elapses_for_the_same_set() {
        let last = NotifyStreak { key: "ollama".to_string(), last_notified_secs: 1000 };
        assert!(should_notify(&["ollama".to_string()], Some(&last), 1000 + NOTIFY_SUPPRESS_WINDOW_SECS));
    }

    #[test]
    fn should_notify_fires_immediately_when_the_failing_set_changes() {
        let last = NotifyStreak { key: "ollama".to_string(), last_notified_secs: 1000 };
        assert!(should_notify(&["ollama".to_string(), "kb.db".to_string()], Some(&last), 1001));
    }

    // --- threshold helpers ---

    #[test]
    fn reflect_completion_ok_boundary() {
        assert!(!reflect_completion_ok(None));
        assert!(reflect_completion_ok(Some(0.0)));
        assert!(reflect_completion_ok(Some(REFLECT_STALE_WARN_HOURS)));
        assert!(!reflect_completion_ok(Some(REFLECT_STALE_WARN_HOURS + 0.01)));
    }

    #[test]
    fn telegram_state_ok_boundary() {
        assert!(!telegram_state_ok(None));
        assert!(telegram_state_ok(Some(TELEGRAM_STATE_STALE_WARN_HOURS)));
        assert!(!telegram_state_ok(Some(TELEGRAM_STATE_STALE_WARN_HOURS + 0.01)));
    }

    #[test]
    fn disk_headroom_ok_boundary() {
        assert!(disk_headroom_ok(DISK_HEADROOM_WARN_BYTES));
        assert!(!disk_headroom_ok(DISK_HEADROOM_WARN_BYTES - 1));
    }
}
