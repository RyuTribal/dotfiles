# mach

A multi-feature machine daemon workspace. Phase 1 restructures the old
standalone `sweep` project into a Cargo workspace so future features (a
knowledge-bank engine, in phase 2) live alongside it under one `mach` CLI
and one `machd` daemon host — without changing anything about how sweep
behaves today.

## Layout

```
mach/
  Cargo.toml          workspace root
  engines/
    sweep/            the sweep engine — lib (scan/store logic) + cli
                       (TUI, shared by the `sweep` binary and `mach sweep`)
                       + the `sweepd` socket daemon binary
  machd/               machine daemon host — phase 1 placeholder, hosts no
                       subsystems yet (see machd/src/main.rs for the plan)
  mach/                unified CLI: `mach sweep <args>`, `mach kb` (phase 2)
  quickshell/
    SweepPanel.qml     template copy for wiring the panel into a quickshell
                       config — NOT the live panel (that lives at
                       ~/.config/quickshell/ii/SweepPanel.qml and is
                       chezmoi-managed separately)
  install.sh           builds and installs mach, machd, sweep, sweepd
```

sweep — disk usage inspector + staged deleter
==============================================

Three parts, one shared Rust engine:

  target/release/sweep    terminal TUI (ncdu-style) + `--top N` one-shot mode
  target/release/sweepd   daemon: JSON protocol over $XDG_RUNTIME_DIR/sweep.sock
  quickshell/SweepPanel.qml  Quickshell popup panel (talks to sweepd)

`mach sweep <args>` behaves exactly like the standalone `sweep` binary —
both call into the same `sweep::cli::run` function in the sweep engine.

Deletion model (both UIs)
-------------------------
Marking an item MOVES it to a staging trash (~/.local/share/sweep/trash/…).
Nothing is lost until you commit ("delete forever" button / `w` in the TUI):
commit permanently deletes the staged items. Unmark or "restore all" moves
them back instantly. Quitting without committing restores everything.

Build
-----
  cargo build --release
  bash install.sh

Quickshell integration
----------------------
1. Copy SweepPanel.qml into your quickshell config dir, e.g.:
     ~/.config/quickshell/SweepPanel.qml

2. Instantiate it from your shell.qml root:
     ShellRoot {
         // ...your existing shell...
         SweepPanel {}
     }

3. Toggle it (the panel auto-starts sweepd if needed):
     qs ipc call sweep toggle

4. Hyprland keybind (hyprland.conf):
     bind = SUPER, U, exec, qs ipc call sweep toggle

The panel scans $HOME by default; change the `scanPath` property in
SweepPanel.qml (or set it where you instantiate: `SweepPanel { scanPath: "/" }`).

Daemon protocol (newline-delimited JSON on the socket)
------------------------------------------------------
  {"op":"scan","path":"/home/you"}   start async scan
  {"op":"status"}                    scanning progress / readiness
  {"op":"ls","id":0}                 list children of a node (omit id = root)
  {"op":"mark","id":7}               stage node 7 (moved to trash)
  {"op":"unmark","id":7}             restore node 7
  {"op":"restore_all"}               restore everything staged
  {"op":"commit"}                    PERMANENTLY delete staged items
  {"op":"quit"}                      restore staged, shutdown, cleanup

sweepd also answers `{"op":"mounts"}` with a df-equivalent list of real
block-device mounts and their usage.

TUI keys
--------
  j/k move   enter open dir   h up   space mark/unmark (staged to trash)
  w commit-delete (y/N confirm)   u restore all   s sort   a apparent/disk
  g/G top/bottom   q quit (restores anything staged)
