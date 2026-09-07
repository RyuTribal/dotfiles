
//! mach — unified CLI for the machine daemon's features.
//!
//! Hand-rolled subcommand dispatch, matching the style the sweep engine
//! already uses for its own argument parsing (no clap).
use std::env;
use std::path::Path;

fn print_help() {
    println!("mach — machine daemon control CLI");
    println!();
    println!("usage: mach <subcommand> [args...]");
    println!();
    println!("subcommands:");
    println!("  sweep [args...]   disk usage browser & staged deleter (mach sweep --help)");
    println!("  kb [args...]      personal vectorized knowledge bank (mach kb --help)");
    println!("  note [args...]    jot a quick note (mach note --help) — also reachable as");
    println!("                    the `note` command, a symlink to this binary");
}

/// Busybox-style dispatch: true when this binary was invoked via a symlink
/// (or hardlink/rename) named `note` rather than as `mach` itself — the
/// `install.sh`-installed `note -> mach` symlink is how `mach note "text"`
/// becomes just `note "text"`. Only the basename of argv[0] matters, so the
/// symlink can live anywhere on PATH.
fn invoked_as_note(argv0: &str) -> bool {
    Path::new(argv0).file_name().and_then(|f| f.to_str()) == Some("note")
}

fn main() -> std::io::Result<()> {
    let mut all_args = env::args();
    let argv0 = all_args.next().unwrap_or_default();
    let mut args = all_args;

    if invoked_as_note(&argv0) {
        return kb::note::run(args);
    }

    match args.next().as_deref() {
        Some("sweep") => sweep::cli::run(args),
        Some("kb") => kb::cli::run(args),
        Some("note") => kb::note::run(args),
        Some("-h") | Some("--help") => {
            print_help();
            Ok(())
        }
        Some(other) => {
            eprintln!("mach: unknown subcommand '{}'", other);
            print_help();
            std::process::exit(1);
        }
        None => {
            print_help();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invoked_as_note_detects_the_note_symlink_by_basename() {
        assert!(invoked_as_note("/home/user/.local/bin/note"));
        assert!(invoked_as_note("note"));
        assert!(!invoked_as_note("/home/user/.local/bin/mach"));
        assert!(!invoked_as_note("mach"));
        assert!(!invoked_as_note(""));
    }
}
