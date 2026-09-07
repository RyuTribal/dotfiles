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
  install -Dm755 "$HERE/scripts/kb-backup.sh"  "$BIN_DIR/mach-kb-backup"
  # `note` is a plain symlink to `mach` — busybox-style argv0 dispatch (see
  # mach/src/main.rs's `invoked_as_note`) makes `note "text"` equivalent to
  # `mach note "text"`. Relative target so it keeps working if BIN_DIR moves.
  mkdir -p "$BIN_DIR"
  ln -sf mach "$BIN_DIR/note"
  echo "installed: $BIN_DIR/{mach,machd,sweep,sweepd,mach-kb-backup,note}"
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
    echo "enabled: mach-reflect.timer (daily 04:00 anchor, +10min after boot, re-arms 3h after each run, +/-15min jitter)"
  else
    echo "WARNING: 'systemctl --user enable --now mach-reflect.timer' failed — enable it by hand once mach is on PATH."
  fi
}
install_reflect_timer

# mach kb export (phase 4 backup) — same soft-install treatment as the
# reflect timer: a non-systemd environment must not fail the whole install,
# `mach-kb-backup` still works run by hand or from cron in that case.
install_kb_backup_timer() {
  local unit_dir="$HOME/.config/systemd/user"
  install -Dm644 "$HERE/systemd/mach-kb-backup.service" "$unit_dir/mach-kb-backup.service"
  install -Dm644 "$HERE/systemd/mach-kb-backup.timer"   "$unit_dir/mach-kb-backup.timer"
  echo "installed: $unit_dir/mach-kb-backup.{service,timer}"

  if ! command -v systemctl >/dev/null 2>&1; then
    echo "NOTE: systemctl not found — units installed but not enabled. Run 'mach-kb-backup' via cron or by hand instead."
    return
  fi
  if ! systemctl --user list-units >/dev/null 2>&1; then
    echo "NOTE: no systemd --user session available here — units installed but not enabled."
    return
  fi
  systemctl --user daemon-reload
  if systemctl --user enable --now mach-kb-backup.timer >/dev/null 2>&1; then
    echo "enabled: mach-kb-backup.timer (daily 03:00, +15min after boot, +/-30min jitter)"
  else
    echo "WARNING: 'systemctl --user enable --now mach-kb-backup.timer' failed — enable it by hand once mach is on PATH."
  fi
}
install_kb_backup_timer

# whisper.cpp (telegram voice-note transcription) — pacman first, per the
# user's package install discipline: this script never pip/npm/cargo-installs
# a transcription tool itself, only checks for one and reports how to get it.
# Soft check: a missing binary must not fail the whole install -- the
# telegram bridge's voice path degrades gracefully (audio saved untranscribed,
# a clear reply to the user) when whisper.cpp isn't there. The multilingual
# ggml-base model (~142MB) is auto-downloaded to ~/.local/share/mach/whisper/
# on first actual use, not here -- this only warns about that size up front.
check_whisper() {
  local bin=""
  for candidate in whisper-cli whisper-cpp whisper; do
    if command -v "$candidate" >/dev/null 2>&1; then
      bin="$candidate"
      break
    fi
  done
  if [ -n "$bin" ]; then
    echo "whisper.cpp: found '$bin' on PATH — telegram voice notes will be transcribed."
    echo "             (first voice note triggers a ~142MB model download to ~/.local/share/mach/whisper/)"
    return
  fi
  echo "NOTE: no whisper.cpp binary (whisper-cli/whisper-cpp/whisper) found on PATH."
  echo "      Telegram voice notes will still be saved, just untranscribed, until this is installed."
  if pacman -Si whisper.cpp >/dev/null 2>&1; then
    echo "      Install it with:  sudo pacman -S whisper.cpp"
  else
    echo "      Not in the official repos on this system — check the AUR:  paru -S whisper.cpp-git (or similar)"
  fi
  if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "      Also missing: ffmpeg (needed to convert Telegram's voice/audio format for whisper.cpp) —  sudo pacman -S ffmpeg"
  fi
}
check_whisper

# mach-telegramd (Telegram note bridge) — same soft-install treatment as the
# other systemd units: a non-systemd environment must not fail the whole
# install. Unlike the reflect/backup timers this is a persistent long-poll
# daemon (Type=simple), not a periodic oneshot, so there is exactly one unit,
# no timer. Its own ConditionPathExists means enabling it before a real
# telegram.toml exists is safe and expected — it simply won't start yet.
install_telegramd_unit() {
  local unit_dir="$HOME/.config/systemd/user"
  install -Dm644 "$HERE/systemd/mach-telegramd.service" "$unit_dir/mach-telegramd.service"
  echo "installed: $unit_dir/mach-telegramd.service"

  if ! command -v systemctl >/dev/null 2>&1; then
    echo "NOTE: systemctl not found — unit installed but not enabled. Run 'machd --foreground' by hand instead."
    return
  fi
  if ! systemctl --user list-units >/dev/null 2>&1; then
    echo "NOTE: no systemd --user session available here — unit installed but not enabled."
    return
  fi
  systemctl --user daemon-reload
  if systemctl --user enable --now mach-telegramd.service >/dev/null 2>&1; then
    echo "enabled: mach-telegramd.service (starts once ~/.local/share/mach/telegram.toml exists — see below if it doesn't yet)"
  else
    echo "WARNING: 'systemctl --user enable --now mach-telegramd.service' failed — enable it by hand once mach is on PATH."
  fi
}
install_telegramd_unit

# Telegram config onboarding — non-interactive (this runs unattended under
# chezmoi apply too), so it only ever prints instructions and never blocks
# or prompts. The real config deliberately lives outside this chezmoi-
# managed repo (~/.local/share/mach/telegram.toml, not ~/.config/mach/...)
# because it holds a secret bot token -- see the global secrets-handling
# rule this install script otherwise never touches.
telegram_config_onboarding() {
  local cfg="$HOME/.local/share/mach/telegram.toml"
  if [ -f "$cfg" ]; then
    return
  fi
  echo
  echo "=========================================================================="
  echo " Telegram note bridge: not configured yet"
  echo "=========================================================================="
  echo " mach-telegramd is installed (and enabled, if systemd is available) but"
  echo " won't start until you give it a bot token. Three steps:"
  echo
  echo " 1. Create a bot: message @BotFather on Telegram, send /newbot, follow"
  echo "    its prompts. It gives you a token like '123456:ABC-def...'."
  echo " 2. Get your own numeric user id: message @userinfobot on Telegram."
  echo " 3. Create the real config (OUTSIDE this repo — it holds a secret, so"
  echo "    it must never live under ~/.config where chezmoi can pick it up):"
  echo
  echo "      mkdir -p ~/.local/share/mach"
  echo "      cp $HERE/mach/telegram.toml.example $cfg"
  echo "      \$EDITOR $cfg"
  echo
  echo " Then start the bridge:  systemctl --user restart mach-telegramd.service"
  echo " (or run 'machd --foreground' by hand to watch it directly)."
  echo "=========================================================================="
}
telegram_config_onboarding

echo
echo "Done. Reload quickshell if running, then press SUPER+U."
echo "Or test now:  qs -c ii ipc call sweep toggle"
