//! Picks a capture binary, discovers what to record from, and spawns/stops
//! the actual recorder subprocesses.
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::active::pid_is_alive;

/// 16-bit, 48kHz, mono -- the fixed format both binaries are told to
/// record in, and the basis for `disk::estimated_mb_per_hour`'s ~350MB/h/
/// track figure.
pub const SAMPLE_RATE: u32 = 48000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBinary {
    PwRecord,
    Parecord,
}

impl CaptureBinary {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureBinary::PwRecord => "pw-record",
            CaptureBinary::Parecord => "parecord",
        }
    }
}

/// `command -v <bin>` via a shell, mirroring `install.sh`'s own
/// `command -v` checks (rather than hand-walking `$PATH`) -- handles the
/// same edge cases (symlinks, non-executable hits) a real shell would.
fn on_path(bin: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {} >/dev/null 2>&1", bin))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `pw-record` (pipewire, native) preferred; `parecord` (pulseaudio-utils,
/// works fine against pipewire-pulse) as fallback; `None` if neither is on
/// PATH -- `mach meet start` refuses to start in that case rather than
/// silently doing nothing.
pub fn detect_binary() -> Option<CaptureBinary> {
    if on_path("pw-record") {
        Some(CaptureBinary::PwRecord)
    } else if on_path("parecord") {
        Some(CaptureBinary::Parecord)
    } else {
        None
    }
}

/// The PipeWire/Pulse node names to record from: the default source (mic)
/// and, for dual mode, the default sink's monitor (the meeting app's
/// output audio) -- named `"<sink>.monitor"` by the pipewire-pulse
/// compatibility layer, verified live against this machine's
/// `pactl list sources short` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Targets {
    pub mic: String,
    pub system: Option<String>,
}

fn run_pactl(args: &[&str]) -> io::Result<String> {
    let out = Command::new("pactl").args(args).output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "pactl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `mic` comes from `pactl get-default-source` and is required -- a failure
/// here means there's no sensible mic target at all, solo or dual.
/// `system` comes from `pactl get-default-sink` + `.monitor`; its own
/// failure only matters to a dual-mode caller (solo mode never looks at
/// it), so it's `None` rather than propagated as an error here.
pub fn discover_targets() -> io::Result<Targets> {
    let mic = run_pactl(&["get-default-source"])?;
    let system = run_pactl(&["get-default-sink"]).ok().map(|sink| format!("{}.monitor", sink));
    Ok(Targets { mic, system })
}

/// `pub(crate)`, not private -- `cli::cmd_stop` reuses this to detach the
/// background `mach meet process` spawn the same way a recorder is
/// detached, rather than a second copy of the same three-line `setsid()`
/// closure.
pub(crate) fn detach_pre_exec(cmd: &mut Command) {
    // SAFETY: the closure only calls the async-signal-safe `setsid(2)` and
    // returns an `io::Error` on failure -- no allocation, no locking, and
    // it runs in the freshly-forked child before exec, so `Command::spawn`'s
    // returned pid IS the recorder's own pid (no double-fork ambiguity to
    // track). This is what actually detaches the recorder from the
    // launching terminal: on hangup (e.g. the terminal window closing),
    // the kernel delivers SIGHUP to the terminal's session/foreground
    // process group, and `setsid()` makes this process the leader of a
    // brand new session with no controlling terminal at all, so it's never
    // a member of that group to begin with.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub fn build_recorder_command(binary: CaptureBinary, target: &str, out_path: &Path) -> Command {
    let mut cmd = match binary {
        CaptureBinary::PwRecord => {
            let mut c = Command::new("pw-record");
            c.arg("--target")
                .arg(target)
                .arg("--rate")
                .arg(SAMPLE_RATE.to_string())
                .arg("--channels")
                .arg("1")
                .arg("--format")
                .arg("s16")
                .arg(out_path);
            c
        }
        CaptureBinary::Parecord => {
            let mut c = Command::new("parecord");
            c.arg("-d")
                .arg(target)
                .arg(format!("--rate={}", SAMPLE_RATE))
                .arg("--channels=1")
                .arg("--format=s16le")
                .arg(out_path);
            c
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach_pre_exec(&mut cmd);
    cmd
}

/// Spawns one recorder, detached (see `detach_pre_exec`), and returns its
/// pid. The `Child` handle is deliberately dropped without waiting: once
/// `mach meet start` exits, the recorder is reparented to init (or systemd
/// as a user-service reaper) like any other orphan, and nothing here ever
/// calls `wait()` on it -- exactly the "survives the launching terminal
/// closing" behavior this subcommand needs.
pub fn spawn_recorder(binary: CaptureBinary, target: &str, out_path: &Path) -> io::Result<u32> {
    let mut cmd = build_recorder_command(binary, target, out_path);
    let child = cmd.spawn()?;
    Ok(child.id())
}

fn send_signal(pid: u32, sig: i32) -> io::Result<()> {
    let ret = unsafe { libc::kill(pid as i32, sig) };
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let start = Instant::now();
    while pid_is_alive(pid) {
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

/// Stops one recorder for a graceful WAV finalize: SIGINT first (verified
/// live -- `pw-record` flushes and closes out a valid, playable WAV header
/// on SIGINT, no corruption), waited on for up to 5s. A recorder that's
/// still alive after that is almost certainly wedged rather than mid-flush,
/// so it's escalated to SIGTERM (documented clean-stop fallback) with a
/// shorter 3s wait -- `mach meet stop` must never hang forever on one bad
/// recorder. Missing-pid errors from `kill` (ESRCH -- it already exited on
/// its own) are swallowed rather than surfaced, since that's not a failure
/// from this function's point of view.
pub fn stop_recorder(pid: u32) {
    if send_signal(pid, libc::SIGINT).is_ok() && wait_for_exit(pid, Duration::from_secs(5)) {
        return;
    }
    if pid_is_alive(pid) {
        let _ = send_signal(pid, libc::SIGTERM);
        wait_for_exit(pid, Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_recorder_command_pw_record_has_expected_args() {
        let cmd = build_recorder_command(CaptureBinary::PwRecord, "my.source", Path::new("/tmp/mic.wav"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "pw-record");
        assert_eq!(
            args,
            vec!["--target", "my.source", "--rate", "48000", "--channels", "1", "--format", "s16", "/tmp/mic.wav"]
        );
    }

    #[test]
    fn build_recorder_command_parecord_has_expected_args() {
        let cmd = build_recorder_command(CaptureBinary::Parecord, "my.sink.monitor", Path::new("/tmp/system.wav"));
        let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
        assert_eq!(cmd.get_program().to_string_lossy(), "parecord");
        assert_eq!(
            args,
            vec!["-d", "my.sink.monitor", "--rate=48000", "--channels=1", "--format=s16le", "/tmp/system.wav"]
        );
    }

    #[test]
    fn capture_binary_as_str() {
        assert_eq!(CaptureBinary::PwRecord.as_str(), "pw-record");
        assert_eq!(CaptureBinary::Parecord.as_str(), "parecord");
    }
}
