
//! mach — unified CLI for the machine daemon's features.
//!
//! Hand-rolled subcommand dispatch, matching the style the sweep engine
//! already uses for its own argument parsing (no clap).
use std::env;

fn print_help() {
    println!("mach — machine daemon control CLI");
    println!();
    println!("usage: mach <subcommand> [args...]");
    println!();
    println!("subcommands:");
    println!("  sweep [args...]   disk usage browser & staged deleter (mach sweep --help)");
    println!("  kb                knowledge bank (not yet implemented — phase 2)");
}

fn main() -> std::io::Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("sweep") => sweep::cli::run(args),
        Some("kb") => {
            eprintln!("mach: kb is not implemented yet (phase 2)");
            std::process::exit(1);
        }
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
