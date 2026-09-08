//! machd — machine daemon host process.
//!
//! Hosts two registered subsystems, each on its own thread, joined on the
//! same shutdown flag below:
//!   - kb    (engines/kb/src/socket.rs) — always runs. Serves
//!           `$XDG_RUNTIME_DIR/mach-kb.sock` for Claude Code's
//!           `kb-recall.sh` hook's fast path (a warm db connection + a warm
//!           ollama HTTP agent, instead of that hook cold-starting a `mach
//!           kb search` subprocess every prompt). Needs no config -- the kb
//!           store always exists.
//!   - telegram (engines/telegram) — the note bridge. No-ops cleanly (one
//!           log line, immediate return -- see `run_telegram_subsystem`)
//!           when `~/.local/share/mach/telegram.toml` is missing or still
//!           holds placeholder values; this must never take the kb
//!           subsystem down with it.
//!
//! There's no formal `Subsystem` trait/registry yet -- two subsystems are
//! still few enough to just spawn by hand in `main`. If a third one shows
//! up, that's the point to extract one.
//!
//! Always runs in the foreground: there is no double-fork/daemonize step
//! anywhere in this process, matching what systemd's `Type=simple` unit
//! (`mach-telegramd.service`) expects of its main process. `--foreground` is
//! accepted (and is the default either way) so `machd --foreground` remains
//! the documented way to run it by hand for debugging -- it changes nothing
//! about behavior, only signals human intent at the command line.
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;

fn print_help() {
    println!("machd — machine daemon host process");
    println!();
    println!("usage: machd [--foreground]");
    println!();
    println!("Hosts two subsystems, each on its own thread:");
    println!("  kb        always runs -- serves $XDG_RUNTIME_DIR/mach-kb.sock for Claude");
    println!("            Code's kb-recall.sh hook (op \"search\" only). No config needed.");
    println!("  telegram  the note bridge. No-ops cleanly (one log line, stays up) if");
    println!("            ~/.local/share/mach/telegram.toml is missing or still holds");
    println!("            placeholder values -- see install.sh's onboarding instructions.");
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

/// Runs the telegram subsystem in this thread until `shutdown` is set.
/// No-ops cleanly -- one log line, immediate return, `Ok(())` -- when the
/// config file is missing or still holds placeholder values: this must
/// never take the whole daemon down, since the kb subsystem doesn't depend
/// on Telegram being configured at all (this is exactly what used to be
/// `machd`'s own top-level `ConditionPathExists`-backed exit, moved down
/// into just this one subsystem). Returns `Err` only for a genuinely
/// unexpected setup failure surfaced by `telegram::run` itself (e.g. an
/// unwritable state path or a broken kb store) -- see that function's own
/// doc comment.
fn run_telegram_subsystem(shutdown: &AtomicBool) -> Result<(), String> {
    let cfg = match telegram::config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("machd: telegram subsystem: {} -- staying disabled (kb subsystem is unaffected)", e);
            return Ok(());
        }
    };
    eprintln!("machd: starting telegram subsystem");
    telegram::run(cfg, shutdown)
}

/// Logs a subsystem thread's outcome and folds it into the process exit
/// code: a thread panic or an `Err` return both count as failure (`1`), a
/// clean `Ok(())` (whether from a full run or an immediate no-op) doesn't.
/// Runs one subsystem on its own thread and logs its error THE MOMENT it
/// returns, not at process exit. `main` joins both subsystems before it
/// reports anything, so a kb thread that died at startup (e.g. a migration
/// race with a concurrent `mach kb` CLI call) used to stay silent for as
/// long as the telegram thread kept running -- hours of "socket missing"
/// with nothing in the journal.
fn spawn_subsystem<F>(name: &'static str, f: F) -> thread::JoinHandle<Result<(), String>>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    thread::spawn(move || {
        let result = f();
        if let Err(e) = &result {
            eprintln!("machd: {} subsystem exited with an error: {}", name, e);
        }
        result
    })
}

fn report(name: &str, result: thread::Result<Result<(), String>>) -> i32 {
    match result {
        Ok(Ok(())) => 0,
        // already logged by `spawn_subsystem` when it happened
        Ok(Err(_)) => 1,
        Err(_) => {
            eprintln!("machd: {} subsystem thread panicked", name);
            1
        }
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

    let shutdown = Arc::new(AtomicBool::new(false));
    install_signal_handlers(&shutdown);

    let kb_shutdown = Arc::clone(&shutdown);
    let kb_thread = spawn_subsystem("kb", move || kb::socket::run(&kb_shutdown));

    let tg_shutdown = Arc::clone(&shutdown);
    let tg_thread = spawn_subsystem("telegram", move || run_telegram_subsystem(&tg_shutdown));

    // Both threads block in their own loop until `shutdown` flips (SIGTERM/
    // SIGINT) -- joining here is just waiting for that, same as the old
    // single-subsystem version blocked directly inside `telegram::run`.
    let kb_result = kb_thread.join();
    let tg_result = tg_thread.join();

    let kb_code = report("kb", kb_result);
    let tg_code = report("telegram", tg_result);
    std::process::exit(kb_code.max(tg_code));
}
