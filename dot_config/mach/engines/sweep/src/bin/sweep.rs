
//! sweep — standalone entry point. All behavior lives in the `sweep::cli`
//! module so `mach sweep` can call the same code.
fn main() -> std::io::Result<()> {
    sweep::cli::run(std::env::args().skip(1))
}
