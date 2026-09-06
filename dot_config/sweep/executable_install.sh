#!/usr/bin/env bash
# chezmoi-daemon
# sweep installer — run this on the HOST:
#   bash ~/.hermes/sandboxes/docker/default/workspace/sweep/install.sh
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR="$HOME/.local/bin"
QS_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/quickshell"
HYPR_CONF="${XDG_CONFIG_HOME:-$HOME/.config}/hypr/hyprland.conf"

echo "== sweep installer =="

# 1) binaries
install -Dm755 "$HERE/target/release/sweep"  "$BIN_DIR/sweep"
install -Dm755 "$HERE/target/release/sweepd" "$BIN_DIR/sweepd"
echo "installed: $BIN_DIR/sweep, $BIN_DIR/sweepd"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "NOTE: $BIN_DIR is not on your PATH — add it or the panel can't spawn sweepd." ;;
esac

# sanity: binaries were built in a Debian container; verify they run here
if ! "$BIN_DIR/sweep" --help >/dev/null 2>&1; then
  echo "WARNING: prebuilt binary didn't run on this system."
  if command -v cargo >/dev/null 2>&1; then
    echo "Rebuilding locally with cargo..."
    if (cd "$HERE" && cargo build --release); then
      install -Dm755 "$HERE/target/release/sweep"  "$BIN_DIR/sweep"
      install -Dm755 "$HERE/target/release/sweepd" "$BIN_DIR/sweepd"
      echo "rebuilt and reinstalled."
    else
      echo "WARNING: local rebuild failed — continuing with config install anyway."
    fi
  else
    echo "WARNING: cargo not found — install rust and run: cargo build --release in $HERE"
  fi
fi

# 2) quickshell panel
mkdir -p "$QS_DIR"
cp "$HERE/quickshell/SweepPanel.qml" "$QS_DIR/SweepPanel.qml"
echo "installed: $QS_DIR/SweepPanel.qml"

SHELL_QML="$QS_DIR/shell.qml"
if [ -f "$SHELL_QML" ]; then
  if grep -q "SweepPanel" "$SHELL_QML"; then
    echo "shell.qml already references SweepPanel — leaving it alone."
  else
    cp "$SHELL_QML" "$SHELL_QML.bak-sweep"
    # insert 'SweepPanel {}' before the last closing brace of the file
    awk 'BEGIN{done=0} {lines[NR]=$0} END{
      for(i=NR;i>=1;i--) if(lines[i] ~ /^\}/ && !done){lines[i]="    SweepPanel {}\n}"; done=1; break}
      for(i=1;i<=NR;i++) print lines[i]
    }' "$SHELL_QML.bak-sweep" > "$SHELL_QML"
    echo "added 'SweepPanel {}' to shell.qml (backup: shell.qml.bak-sweep)"
  fi
else
  echo "No $SHELL_QML found — add this inside your shell root component:"
  echo "    SweepPanel {}"
fi

# 3) hyprland keybind
BIND='bind = SUPER, U, exec, qs ipc call sweep toggle  # sweep disk inspector'
if [ -f "$HYPR_CONF" ]; then
  if grep -q "qs ipc call sweep toggle" "$HYPR_CONF"; then
    echo "hyprland keybind already present."
  else
    printf '\n%s\n' "$BIND" >> "$HYPR_CONF"
    echo "appended keybind SUPER+U to hyprland.conf"
  fi
else
  echo "hyprland.conf not found at $HYPR_CONF — add manually:"
  echo "    $BIND"
fi

echo
echo "Done. Reload quickshell (qs kill; qs &) or re-login, then press SUPER+U."
echo "Or test now:  qs ipc call sweep toggle"
