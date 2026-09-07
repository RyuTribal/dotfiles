//! `meet` -- subcommand dispatch for `mach meet ...`, matching the
//! hand-rolled arg-parsing style `sweep`/`kb` already use (no clap).
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::active::{self, ActiveMeeting};
use crate::capture;
use crate::disk;
use crate::meeting::{self, Meeting, Mode, Pids, TrackInfo};
use crate::process::{self, ProcessNotifier};
use crate::time;

fn print_help() {
    println!("mach meet -- meeting recorder (capture + processing)");
    println!();
    println!("usage: mach meet <subcommand> [args...]");
    println!();
    println!("subcommands:");
    println!("  start [--solo] [--title \"...\"]  begin recording (refuses a second");
    println!("                                    concurrent meeting)");
    println!("  stop                              stop the active meeting, finalize its");
    println!("                                    WAV files and meeting.json, and queue it");
    println!("                                    for background processing");
    println!("  status                            whether a meeting is recording, elapsed");
    println!("                                    time, and whether track sizes are growing");
    println!("  process [DIR]                     transcribe + summarize a meeting; with no");
    println!("                                    DIR, every meeting marked needs-processing");
    println!("                                    or needs-summary, oldest first");
}

pub fn run(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    match args.next().as_deref() {
        Some("start") => cmd_start(args),
        Some("stop") => cmd_stop(args),
        Some("status") => cmd_status(args),
        Some("process") => cmd_process(args),
        Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("mach meet: unknown subcommand '{}'", other);
            print_help();
            std::process::exit(1);
        }
        None => {
            print_help();
            Ok(())
        }
    }
}

fn home() -> io::Result<PathBuf> {
    std::env::var("HOME").map(PathBuf::from).map_err(|_| io::Error::other("HOME not set"))
}

/// `~/.local/share/mach/meetings/` -- created on demand, same convention
/// `kb::note::images_dir` uses for its own store directory.
fn meetings_root() -> io::Result<PathBuf> {
    let dir = home()?.join(".local/share/mach/meetings");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Touches an empty `needs-processing` marker in `dir` -- phase B's queue
/// signal, written both by a normal `mach meet stop` and by `start`'s
/// crash-recovery path (a stale meeting keeps whatever audio it captured
/// and is marked exactly the same way a cleanly-stopped one is, so phase B
/// doesn't need two different signals to look for).
fn write_needs_processing_marker(dir: &Path) -> io::Result<()> {
    fs::write(dir.join("needs-processing"), b"")
}

fn human_mb(bytes: u64) -> String {
    format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
}

// ---------- start ----------

fn cmd_start(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut solo = false;
    let mut title: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--solo" => solo = true,
            "--title" => {
                title = match args.next() {
                    Some(t) => Some(t),
                    None => {
                        eprintln!("mach meet start: --title requires an argument");
                        std::process::exit(1);
                    }
                };
            }
            "-h" | "--help" => {
                println!("usage: mach meet start [--solo] [--title \"...\"]");
                println!("       --solo    record mic.wav only (default: dual mic + system audio)");
                return Ok(());
            }
            other => {
                eprintln!("mach meet start: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }
    let mode = if solo { Mode::Solo } else { Mode::Dual };

    let root = meetings_root()?;

    // Refuse a second concurrent meeting -- unless the existing pidfile is
    // stale (every recorder pid it names is dead), in which case this is
    // crash recovery: the old directory keeps whatever audio it captured,
    // gets marked needs-processing same as a clean stop, and start proceeds.
    if let Some(existing) = active::read_active(&root)? {
        if active::is_stale(&existing, active::pid_is_a_recorder) {
            println!(
                "mach meet start: recovered from a stale meeting at {} (its recorder(s) were no \
                 longer running) -- that directory's audio is preserved and marked for processing",
                existing.dir
            );
            let old_dir = PathBuf::from(&existing.dir);
            if old_dir.is_dir() {
                if let Err(e) = write_needs_processing_marker(&old_dir) {
                    eprintln!("mach meet start: warning: could not mark {} needs-processing: {}", existing.dir, e);
                }
            }
            active::remove_active(&root)?;
        } else {
            eprintln!(
                "mach meet start: a meeting is already recording ({}, started {}) -- run \
                 `mach meet stop` first",
                existing.dir, existing.started_at
            );
            std::process::exit(1);
        }
    }

    // Disk guard: refuse before touching anything else if the target
    // filesystem is close to full.
    let free = disk::free_bytes(&root)?;
    if free < disk::MIN_FREE_BYTES {
        eprintln!(
            "mach meet start: only {} free on the filesystem holding {} (need at least {}) -- \
             refusing to start",
            human_mb(free),
            root.display(),
            human_mb(disk::MIN_FREE_BYTES)
        );
        std::process::exit(1);
    }
    println!(
        "mach meet start: {} free; estimated growth ~{}MB/hour ({} track{})",
        human_mb(free),
        disk::estimated_mb_per_hour(mode),
        mode.track_names().len(),
        if mode.track_names().len() == 1 { "" } else { "s" }
    );

    let binary = match capture::detect_binary() {
        Some(b) => b,
        None => {
            eprintln!(
                "mach meet start: neither `pw-record` (pipewire) nor `parecord` (pulseaudio-utils) \
                 was found on PATH -- install one of them and try again"
            );
            std::process::exit(1);
        }
    };

    let targets = capture::discover_targets()?;
    if mode == Mode::Dual && targets.system.is_none() {
        eprintln!(
            "mach meet start: could not determine the default sink (for system-audio capture) via \
             `pactl` -- fix pactl/pipewire-pulse, or run with --solo to record mic only"
        );
        std::process::exit(1);
    }

    let started_secs = time::now_secs();
    let started_at = time::rfc3339_from_secs(started_secs);
    let base_name = meeting::dir_name(started_secs, title.as_deref());
    let dir_name = meeting::disambiguate(&base_name, |c| root.join(c).exists());
    let dir = root.join(&dir_name);
    fs::create_dir_all(&dir)?;

    let mic_path = dir.join(Mode::file_for("mic"));
    let mic_pid = match capture::spawn_recorder(binary, &targets.mic, &mic_path) {
        Ok(pid) => pid,
        Err(e) => {
            let _ = fs::remove_dir_all(&dir);
            eprintln!("mach meet start: failed to spawn mic recorder ({} -> {}): {}", targets.mic, mic_path.display(), e);
            std::process::exit(1);
        }
    };

    let system_pid = if mode == Mode::Dual {
        // Safe: the Dual/targets.system.is_none() check above already
        // refused before any process was spawned.
        let system_target = targets.system.as_deref().unwrap();
        let system_path = dir.join(Mode::file_for("system"));
        match capture::spawn_recorder(binary, system_target, &system_path) {
            Ok(pid) => Some(pid),
            Err(e) => {
                capture::stop_recorder(mic_pid);
                let _ = fs::remove_dir_all(&dir);
                eprintln!(
                    "mach meet start: failed to spawn system-audio recorder ({} -> {}): {} -- mic recorder \
                     stopped, nothing left half-started",
                    system_target,
                    system_path.display(),
                    e
                );
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    let pids = Pids { mic: mic_pid, system: system_pid };
    let meeting = Meeting {
        started_at: started_at.clone(),
        ended_at: None,
        mode,
        title: title.clone(),
        sample_rate: capture::SAMPLE_RATE,
        capture_binary: binary.as_str().to_string(),
        pids,
        tracks: Vec::new(),
        duration_secs: None,
    };
    meeting.write(&dir)?;

    let active = ActiveMeeting { dir: dir.display().to_string(), pids: pids.all(), started_at };
    active::write_active(&root, &active)?;

    println!("recording started: {}", dir.display());
    println!("  mode: {}", if mode == Mode::Dual { "dual (mic + system)" } else { "solo (mic only)" });
    println!("  capture binary: {}", binary.as_str());
    println!("  mic target: {}", targets.mic);
    if mode == Mode::Dual {
        if let Some(sys) = &targets.system {
            println!("  system target: {}", sys);
        }
    }
    println!("  stop with: mach meet stop");
    Ok(())
}

// ---------- stop ----------

fn cmd_stop(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    if let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach meet stop");
                return Ok(());
            }
            other => {
                eprintln!("mach meet stop: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let root = meetings_root()?;
    let active = match active::read_active(&root)? {
        Some(a) => a,
        None => {
            eprintln!("mach meet stop: no meeting is currently recording");
            std::process::exit(1);
        }
    };
    let dir = PathBuf::from(&active.dir);

    for pid in &active.pids {
        capture::stop_recorder(*pid);
    }

    let mut meeting = match Meeting::read(&dir) {
        Ok(m) => m,
        Err(e) => {
            // The pidfile pointed somewhere whose meeting.json is gone or
            // unreadable -- recorders are already stopped above regardless,
            // so this only affects how much we can report; still clear the
            // pidfile so `mach meet start` isn't refused forever over a
            // directory that's already unrecoverable.
            active::remove_active(&root)?;
            eprintln!("mach meet stop: recorders stopped, but couldn't read {}: {}", Meeting::path(&dir).display(), e);
            std::process::exit(1);
        }
    };

    let ended_secs = time::now_secs();
    let ended_at = time::rfc3339_from_secs(ended_secs);
    let started_secs = time::parse_rfc3339(&meeting.started_at).map(|s| s as u64).unwrap_or(ended_secs);
    let duration_secs = ended_secs.saturating_sub(started_secs);

    let tracks: Vec<TrackInfo> = meeting
        .mode
        .track_names()
        .iter()
        .map(|&name| {
            let file = Mode::file_for(name);
            let bytes = fs::metadata(dir.join(&file)).map(|m| m.len()).unwrap_or(0);
            TrackInfo { name: name.to_string(), file, bytes }
        })
        .collect();

    meeting.ended_at = Some(ended_at);
    meeting.duration_secs = Some(duration_secs);
    meeting.tracks = tracks.clone();
    meeting.write(&dir)?;

    write_needs_processing_marker(&dir)?;
    active::remove_active(&root)?;

    println!("recording stopped: {}", dir.display());
    println!("  duration: {}", human_duration(duration_secs));
    for t in &tracks {
        if t.bytes == 0 {
            println!("  {}: MISSING (0 bytes) -- recorder may have failed to start", t.file);
        } else {
            println!("  {}: {}", t.file, human_mb(t.bytes));
        }
    }

    match spawn_detached_process() {
        Ok(()) => println!("  processing queued in the background (mach meet process)"),
        Err(e) => eprintln!(
            "mach meet stop: warning: could not spawn background processing ({}) -- run `mach meet process` \
             manually, or wait for the next opportunistic `mach kb reflect` run",
            e
        ),
    }
    Ok(())
}

/// Spawns a detached `mach meet process` (no `DIR` -- it scans the whole
/// queue) so `mach meet stop` returns instantly instead of blocking on
/// whisper.cpp transcription and a sonnet call. Same detach pattern
/// `capture::spawn_recorder` uses for the recorder subprocesses themselves
/// (`setsid()` via `capture::detach_pre_exec`, so it survives the launching
/// terminal closing); stdout/stderr are appended to a small log file next
/// to the meetings root rather than nulled, since a process that can run
/// for tens of minutes (whisper.cpp on a long meeting) is worth being able
/// to inspect after the fact.
fn spawn_detached_process() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let root = meetings_root()?;
    let log = fs::OpenOptions::new().create(true).append(true).open(root.join(".process.log"))?;
    let log_err = log.try_clone()?;

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("meet").arg("process");
    cmd.stdin(std::process::Stdio::null()).stdout(log).stderr(log_err);
    capture::detach_pre_exec(&mut cmd);
    cmd.spawn()?;
    Ok(())
}

fn human_duration(secs: u64) -> String {
    format!("{}m{:02}s", secs / 60, secs % 60)
}

// ---------- status ----------

fn cmd_status(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    if let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach meet status");
                return Ok(());
            }
            other => {
                eprintln!("mach meet status: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let root = meetings_root()?;
    let active = match active::read_active(&root)? {
        Some(a) => a,
        None => {
            println!("no meeting is currently recording");
            return Ok(());
        }
    };
    let dir = PathBuf::from(&active.dir);
    let meeting = Meeting::read(&dir).ok();

    let now = time::now_secs();
    let started_secs = time::parse_rfc3339(&active.started_at).map(|s| s as u64).unwrap_or(now);
    let elapsed = now.saturating_sub(started_secs);

    println!("recording: {}", dir.display());
    println!("  elapsed: {}", human_duration(elapsed));
    if let Some(m) = &meeting {
        println!("  mode: {}", if m.mode == Mode::Dual { "dual (mic + system)" } else { "solo (mic only)" });
        if let Some(t) = &m.title {
            println!("  title: {}", t);
        }
    }

    let any_alive = active.pids.iter().any(|&p| active::pid_is_alive(p));
    if !any_alive {
        println!(
            "  WARNING: no recorder process appears to be running (crashed?) -- `mach meet stop` \
             will still finalize whatever's on disk"
        );
    }

    let track_names: Vec<&str> = meeting.as_ref().map(|m| m.mode.track_names().to_vec()).unwrap_or_else(|| vec!["mic"]);
    let files: Vec<(String, PathBuf)> =
        track_names.iter().map(|&n| (Mode::file_for(n), dir.join(Mode::file_for(n)))).collect();

    let before: Vec<u64> = files.iter().map(|(_, p)| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect();
    std::thread::sleep(std::time::Duration::from_millis(800));
    let after: Vec<u64> = files.iter().map(|(_, p)| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).collect();

    for (i, (name, _)) in files.iter().enumerate() {
        let growing = after[i] > before[i];
        println!(
            "  {}: {} ({})",
            name,
            human_mb(after[i]),
            if growing { "growing" } else if any_alive { "not growing yet" } else { "stopped" }
        );
    }
    Ok(())
}

// ---------- process ----------

fn cmd_process(mut args: impl Iterator<Item = String>) -> io::Result<()> {
    let mut dir_arg: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("usage: mach meet process [DIR]");
                println!(
                    "       DIR   a specific meeting directory (absolute, or a name under the meetings \
                     root) to (re)process; if omitted, every meeting directory currently marked \
                     needs-processing or needs-summary is processed, oldest first"
                );
                return Ok(());
            }
            other if dir_arg.is_none() => dir_arg = Some(other.to_string()),
            other => {
                eprintln!("mach meet process: unexpected argument '{}'", other);
                std::process::exit(1);
            }
        }
    }

    let root = meetings_root()?;
    let notifier = ProcessNotifier;

    match dir_arg {
        Some(d) => {
            let path = PathBuf::from(&d);
            let dir = if path.is_dir() { path } else { root.join(&d) };
            if !dir.is_dir() {
                eprintln!("mach meet process: '{}' is not a directory", d);
                std::process::exit(1);
            }
            process::process_dir(&dir, &notifier)
        }
        None => process::process_queue(&root, &notifier),
    }
}
