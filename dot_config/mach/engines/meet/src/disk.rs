//! Disk-space guard: refuses `mach meet start` on a near-full filesystem,
//! and estimates recording growth rate for the start-time warning.
use std::io;
use std::path::Path;
use std::process::Command;

use crate::meeting::Mode;

/// 16-bit 48kHz mono, per track -- the number the task's own guard spec
/// gives, and consistent with the fixed format `capture::SAMPLE_RATE` /
/// `build_recorder_command` actually record in.
pub const MB_PER_HOUR_PER_TRACK: u64 = 350;

/// Refuse to start below this much free space on the target filesystem.
pub const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Bytes free on the filesystem containing `path`, via `df -B1 --output=avail`
/// -- shelling out rather than a `statvfs` binding, matching this crate's
/// (and the rest of the codebase's) preference for a subprocess over an
/// extra dependency for a one-off syscall wrapper.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let out = Command::new("df").arg("-B1").arg("--output=avail").arg(path).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!("df failed: {}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .nth(1) // line 0 is the "Avail" header
        .and_then(|l| l.trim().parse::<u64>().ok())
        .ok_or_else(|| io::Error::other(format!("could not parse `df` output: {:?}", text)))
}

/// Estimated recording growth rate for `mode` -- one track for solo, two
/// for dual.
pub fn estimated_mb_per_hour(mode: Mode) -> u64 {
    MB_PER_HOUR_PER_TRACK * mode.track_names().len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimated_mb_per_hour_solo_is_one_track() {
        assert_eq!(estimated_mb_per_hour(Mode::Solo), 350);
    }

    #[test]
    fn estimated_mb_per_hour_dual_is_two_tracks() {
        assert_eq!(estimated_mb_per_hour(Mode::Dual), 700);
    }

    #[test]
    fn free_bytes_on_a_real_path_succeeds_and_is_plausible() {
        // Live/integration-ish, but cheap and deterministic enough to run
        // as a unit test: any real path on this machine has some free
        // space to report, and it must parse as a number.
        let n = free_bytes(Path::new("/tmp")).unwrap();
        assert!(n > 0);
    }
}
