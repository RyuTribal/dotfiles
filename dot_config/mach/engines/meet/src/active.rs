//! The `.active` pidfile at `~/.local/share/mach/meetings/.active` --
//! refuses a second concurrent `mach meet start`, and lets `stop`/`status`
//! find the running meeting without scanning every directory.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveMeeting {
    /// Absolute path to the meeting directory.
    pub dir: String,
    /// Every recorder pid spawned for this meeting (see `Pids::all`).
    pub pids: Vec<u32>,
    pub started_at: String,
}

pub fn active_path(root: &Path) -> PathBuf {
    root.join(".active")
}

/// Reads and parses the pidfile. `Ok(None)` covers both "no meeting is
/// recording" (file absent) and "the pidfile is corrupt" (unparseable) --
/// the latter should be effectively impossible (only this module ever
/// writes it) but must never crash `mach meet status`/`stop`; the caller is
/// left to report "no meeting is currently recording" either way, which is
/// the honest answer when the file can't be trusted.
pub fn read_active(root: &Path) -> io::Result<Option<ActiveMeeting>> {
    let path = active_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let s = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&s).ok())
}

pub fn write_active(root: &Path, active: &ActiveMeeting) -> io::Result<()> {
    let json = serde_json::to_string_pretty(active).map_err(io::Error::other)?;
    fs::write(active_path(root), json)
}

pub fn remove_active(root: &Path) -> io::Result<()> {
    let path = active_path(root);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// True when NONE of `active`'s pids are still alive (per `pid_alive`) --
/// a crashed/killed-out-from-under-us meeting that `mach meet start` should
/// recover from rather than refuse to start over. Generic over the
/// liveness check so this is unit-testable with a fake instead of real
/// `/proc` entries.
pub fn is_stale<F: Fn(u32) -> bool>(active: &ActiveMeeting, pid_alive: F) -> bool {
    !active.pids.iter().any(|&p| pid_alive(p))
}

/// True if `pid` is alive right now (`/proc/<pid>` exists) -- no name/comm
/// check; used for `mach meet stop`'s "has it exited yet" wait loop, where
/// pid-reuse within a few seconds isn't a realistic concern.
pub fn pid_is_alive(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

/// True if `pid` is alive AND its `/proc/<pid>/comm` looks like one of our
/// recorder binaries -- the stricter check `mach meet start` uses for
/// stale-pidfile recovery, guarding against the OS having recycled a dead
/// meeting's old pid for an unrelated process in the meantime (`pid_is_alive`
/// alone can't tell those apart).
pub fn pid_is_a_recorder(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{}/comm", pid)) {
        Ok(comm) => matches!(comm.trim(), "pw-record" | "parecord"),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn active(pids: Vec<u32>) -> ActiveMeeting {
        ActiveMeeting { dir: "/tmp/some-dir".to_string(), pids, started_at: "2026-09-07T17:33:00Z".to_string() }
    }

    #[test]
    fn stale_when_no_pid_is_alive() {
        let a = active(vec![111, 112]);
        assert!(is_stale(&a, |_| false));
    }

    #[test]
    fn not_stale_when_any_pid_is_alive() {
        let a = active(vec![111, 112]);
        let alive: HashSet<u32> = [112].into_iter().collect();
        assert!(!is_stale(&a, |p| alive.contains(&p)));
    }

    #[test]
    fn not_stale_when_all_pids_alive() {
        let a = active(vec![111, 112]);
        assert!(!is_stale(&a, |_| true));
    }

    #[test]
    fn solo_mode_single_pid_stale_check() {
        let a = active(vec![111]);
        assert!(is_stale(&a, |_| false));
        assert!(!is_stale(&a, |_| true));
    }

    #[test]
    fn active_json_roundtrips() {
        let a = active(vec![111, 112]);
        let json = serde_json::to_string(&a).unwrap();
        let back: ActiveMeeting = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn read_active_on_missing_file_is_none() {
        let root = std::env::temp_dir().join(format!("mach-meet-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        assert_eq!(read_active(&root).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn write_then_read_active_roundtrips_through_real_fs() {
        let root = std::env::temp_dir().join(format!("mach-meet-test-rw-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let a = active(vec![111, 112]);
        write_active(&root, &a).unwrap();
        assert_eq!(read_active(&root).unwrap(), Some(a));
        remove_active(&root).unwrap();
        assert_eq!(read_active(&root).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_active_on_corrupt_file_is_none_not_an_error() {
        let root = std::env::temp_dir().join(format!("mach-meet-test-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(active_path(&root), b"not json at all").unwrap();
        assert_eq!(read_active(&root).unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pid_is_alive_true_for_our_own_process() {
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn pid_is_alive_false_for_an_unused_high_pid() {
        // PIDs above /proc/sys/kernel/pid_max on any real system this test
        // runs on -- not a real process.
        assert!(!pid_is_alive(4_000_000_000));
    }
}
