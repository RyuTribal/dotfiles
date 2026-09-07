#!/usr/bin/env bash
# chezmoi-daemon
# mach installer — builds/installs the machine-daemon binaries.
# The quickshell panel (SweepPanel.qml), shell.qml wiring, and the SUPER+U
# keybind are delivered by chezmoi as managed config; this script must not
# touch config files.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR="$HOME/.local/bin"

echo "== mach installer =="

install_bins() {
  install -Dm755 "$HERE/target/release/mach"   "$BIN_DIR/mach"
  install -Dm755 "$HERE/target/release/machd"  "$BIN_DIR/machd"
  install -Dm755 "$HERE/target/release/sweep"  "$BIN_DIR/sweep"
  install -Dm755 "$HERE/target/release/sweepd" "$BIN_DIR/sweepd"
  echo "installed: $BIN_DIR/{mach,machd,sweep,sweepd}"
}

build() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found — install rust, then re-run (chezmoi apply or: bash $HERE/install.sh)" >&2
    exit 1
  fi
  echo "Building mach workspace (cargo build --release)..."
  (cd "$HERE" && cargo build --release)
}

# Always build: cargo is incremental, so an up-to-date tree is a fast no-op,
# and a changed tree never silently reinstalls stale binaries (which the old
# exists-and-runs check allowed).
build
install_bins

if ! "$BIN_DIR/sweep" --help >/dev/null 2>&1; then
  echo "Installed binary didn't run on this system — rebuilding locally..."
  build
  install_bins
fi

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "NOTE: $BIN_DIR is not on your PATH — add it or the panel can't spawn sweepd." ;;
esac

# kb (phase 2) needs a running ollama with the embedding model pulled. This
# is a soft check: kb itself degrades gracefully (clear error on add, warned
# substring fallback on search) when ollama is missing, so an absent or
# unconfigured ollama must not fail the whole install.
check_ollama() {
  if ! command -v ollama >/dev/null 2>&1; then
    echo "NOTE: ollama not found — 'mach kb' needs it for embeddings (https://ollama.com)."
    echo "      Install it, then run: ollama pull nomic-embed-text"
    return
  fi
  if ollama list 2>/dev/null | awk '{print $1}' | grep -qx 'nomic-embed-text:latest\|nomic-embed-text'; then
    echo "ollama: nomic-embed-text already present."
  else
    echo "ollama found but nomic-embed-text is missing — pulling it now..."
    if ollama pull nomic-embed-text; then
      echo "ollama: nomic-embed-text pulled."
    else
      echo "WARNING: 'ollama pull nomic-embed-text' failed — 'mach kb add'/'search' will error until this succeeds (is ollama serve running?)."
    fi
  fi
}
check_ollama

# mach kb reflect (phase 3) runs on a schedule via a systemd user timer
# rather than a hand-rolled loop inside machd — a timer survives logouts,
# gets Persistent=true catch-up, and needs no daemon process of its own.
# This is a soft install: a non-systemd environment (no `systemctl`, or a
# user session systemd doesn't manage) must not fail the whole install —
# `mach kb reflect` still works fine run by hand or from cron in that case.
install_reflect_timer() {
  local unit_dir="$HOME/.config/systemd/user"
  install -Dm644 "$HERE/systemd/mach-reflect.service" "$unit_dir/mach-reflect.service"
  install -Dm644 "$HERE/systemd/mach-reflect.timer"   "$unit_dir/mach-reflect.timer"
  echo "installed: $unit_dir/mach-reflect.{service,timer}"

  if ! command -v systemctl >/dev/null 2>&1; then
    echo "NOTE: systemctl not found — units installed but not enabled. Run 'mach kb reflect' via cron or by hand instead."
    return
  fi
  if ! systemctl --user list-units >/dev/null 2>&1; then
    echo "NOTE: no systemd --user session available here — units installed but not enabled."
    return
  fi
  systemctl --user daemon-reload
  if systemctl --user enable --now mach-reflect.timer >/dev/null 2>&1; then
    echo "enabled: mach-reflect.timer (daily 04:00, +10min after boot, +/-15min jitter)"
  else
    echo "WARNING: 'systemctl --user enable --now mach-reflect.timer' failed — enable it by hand once mach is on PATH."
  fi
}
install_reflect_timer

echo
echo "Done. Reload quickshell if running, then press SUPER+U."
echo "Or test now:  qs -c ii ipc call sweep toggle"
