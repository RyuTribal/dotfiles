
//! machd — machine daemon host process.
//!
//! Phase 1 placeholder: no subsystems are registered here yet. sweepd (the
//! disk-usage daemon) stays its own binary for now, because the Quickshell
//! panel (~/.config/quickshell/ii/SweepPanel.qml) spawns a process literally
//! named "sweepd" and talks to it over a JSON protocol on
//! $XDG_RUNTIME_DIR/sweep.sock. Folding sweepd into machd is a phase-2+
//! integration: it needs a subsystem-registration API here, and then either
//! updating the panel's spawn command (out of scope — panel is not to be
//! touched in this phase) or having machd listen under the same socket path
//! and exec/proxy to preserve the existing contract.
fn main() {
    eprintln!("machd: no subsystems registered yet (phase 1 placeholder)");
}
