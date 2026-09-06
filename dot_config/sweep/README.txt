sweep — disk usage inspector + staged deleter
=============================================

Three parts, one shared Rust engine:

  target/release/sweep    terminal TUI (ncdu-style) + `--top N` one-shot mode
  target/release/sweepd   daemon: JSON protocol over $XDG_RUNTIME_DIR/sweep.sock
  quickshell/SweepPanel.qml  Quickshell popup panel (talks to sweepd)

Deletion model (both UIs)
-------------------------
Marking an item MOVES it to a staging trash (~/.local/share/sweep/trash/…).
Nothing is lost until you commit ("delete forever" button / `w` in the TUI):
commit permanently deletes the staged items. Unmark or "restore all" moves
them back instantly. Quitting without committing restores everything.

Build
-----
  cargo build --release
  install -Dm755 target/release/sweep  ~/.local/bin/sweep
  install -Dm755 target/release/sweepd ~/.local/bin/sweepd

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

TUI keys
--------
  j/k move   enter open dir   h up   space mark/unmark (staged to trash)
  w commit-delete (y/N confirm)   u restore all   s sort   a apparent/disk
  g/G top/bottom   q quit (restores anything staged)
