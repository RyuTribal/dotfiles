//! machd — machine daemon host process.
//!
//! Hosts registered subsystems; the telegram note bridge is the first (and
//! so far only) one. A subsystem is just "a config type to load, and a `run`
//! function that blocks until told to shut down" -- there's no formal
//! registry trait yet because there's exactly one subsystem to register.
//! When a second one shows up (e.g. sweepd, per the phase-1 doc comment
//! this file used to carry), that's the point to extract a real
//! `Subsystem` trait and a `Vec<Box<dyn Subsystem>>` this `main` iterates,
//! each on its own thread, joined on the same shutdown flag below.
//!
//! Always runs in the foreground: there is no double-fork/daemonize step
//! anywhere in this process, matching what systemd's `Type=simple` unit
//! (`mach-telegramd.service`) expects of its main process. `--foreground` is
//! accepted (and is the default either way) so `machd --foreground` remains
//! the documented way to run it by hand for debugging -- it changes nothing
//! about behavior, only signals human intent at the command line.
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn print_help() {
    println!("machd — machine daemon host process");
    println!();
    println!("usage: machd [--foreground]");
    println!();
    println!("Hosts registered subsystems (currently: the telegram note bridge).");
    println!("Reads its config from ~/.local/share/mach/telegram.toml; exits");
    println!("cleanly with a one-line log if that file is missing or still holds");
    println!("placeholder values (the mach-telegramd.service unit's");
    println!("ConditionPathExists keeps systemd from even starting it in that");
    println!("case -- see install.sh's onboarding instructions).");
    println!();
    println!("  --foreground   run attached to the terminal (the documented way");
    println!("                 to run this by hand for debugging -- machd never");
    println!("                 forks/daemonizes either way, so this changes");
    println!("                 nothing about behavior)");
}

/// Registers a SIGTERM/SIGINT handler that flips `shutdown` to `true` --
/// `signal-hook`'s `flag` module does this with an async-signal-safe atomic
/// store, no unsafe code needed here. Best-effort: if registration itself
/// fails (extremely unlikely -- would mean the process is nearly out of
/// file descriptors), this daemon still runs, it just can't be asked to
/// shut down gracefully via signal; `systemd stop` would then fall through
/// to SIGKILL after its timeout instead of a clean SIGTERM handoff.
fn install_signal_handlers(shutdown: &Arc<AtomicBool>) {
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, shutdown.clone()) {
        eprintln!("machd: warning: could not register SIGTERM handler: {}", e);
    }
    if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGINT, shutdown.clone()) {
        eprintln!("machd: warning: could not register SIGINT handler: {}", e);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return;
    }
    // Accepted for the documented debugging invocation; see the module doc
    // comment for why it doesn't otherwise change behavior.
    let _foreground = args.iter().any(|a| a == "--foreground");

    let cfg = match telegram::config::load() {
        Ok(c) => c,
        Err(e) => {
            // Clean exit, one-line log -- never a crash-loop over a missing
            // or placeholder config. Under the shipped systemd unit this
            // path is normally unreachable (ConditionPathExists keeps the
            // unit from even starting); it's the fallback for a bare
            // `machd`/`machd --foreground` run by hand before the config
            // exists.
            eprintln!("machd: {}", e);
            std::process::exit(0);
        }
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(&shutdown);

    eprintln!("machd: starting telegram subsystem");
    match telegram::run(cfg, &shutdown) {
        Ok(()) => {
            eprintln!("machd: telegram subsystem shut down cleanly");
        }
        Err(e) => {
            eprintln!("machd: telegram subsystem exited with an error: {}", e);
            std::process::exit(1);
        }
    }
}
