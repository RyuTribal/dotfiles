#!/usr/bin/env bash
# chezmoi-daemon
# sweep installer — builds/installs the daemon binaries only.
# The quickshell panel (SweepPanel.qml), shell.qml wiring, and the SUPER+U
# keybind are delivered by chezmoi as managed config; this script must not
# touch config files.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR="$HOME/.local/bin"

echo "== sweep installer =="

install_bins() {
  install -Dm755 "$HERE/target/release/sweep"  "$BIN_DIR/sweep"
  install -Dm755 "$HERE/target/release/sweepd" "$BIN_DIR/sweepd"
  echo "installed: $BIN_DIR/sweep, $BIN_DIR/sweepd"
}

build() {
  if ! command -v cargo >/dev/null 2>&1; then
    echo "ERROR: cargo not found — install rust, then re-run (chezmoi apply or: bash $HERE/install.sh)" >&2
    exit 1
  fi
  echo "Building sweep (cargo build --release)..."
  (cd "$HERE" && cargo build --release)
}

# Build when no binaries exist yet (fresh machine: chezmoi does not ship
# target/), or when the prebuilt ones do not run on this system.
if [ ! -x "$HERE/target/release/sweep" ] || [ ! -x "$HERE/target/release/sweepd" ]; then
  build
fi
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

echo
echo "Done. Reload quickshell if running, then press SUPER+U."
echo "Or test now:  qs -c ii ipc call sweep toggle"
