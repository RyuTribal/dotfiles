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
  # `note` belongs to engrain now (~/programming/engrain/install.sh links it).
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

# Memory lives in engrain (~/programming/engrain), a separate project with
# its own installer and units. mach only depends on it (meet and telegram
# store through it), so check it is there, retire the units mach used to
# own, and add mach's own pieces on top of engrain's.
if ! command -v engrain >/dev/null 2>&1; then
  echo "NOTE: engrain not found on PATH; install it: ~/programming/engrain/install.sh"
fi

have_user_systemd() {
  command -v systemctl >/dev/null 2>&1 && systemctl --user list-units >/dev/null 2>&1
}

# Units from before the engrain split: engrain-reflect/-index/-backup/-health
# replace them.
retire_memory_units() {
  local unit_dir="$HOME/.config/systemd/user"
  have_user_systemd || return 0
  for t in mach-reflect mach-index mach-kb-backup mach-health; do
    systemctl --user disable --now "$t.timer" >/dev/null 2>&1 || true
    rm -f "$unit_dir/$t.timer" "$unit_dir/$t.service"
  done
  rm -f "$BIN_DIR/mach-kb-backup"
  systemctl --user daemon-reload
}
retire_memory_units

# mach meet process files meeting facts into engrain, so it takes engrain's
# job lock and runs on its own timer (it used to be a pre-step of the old
# reflect unit). Plus a drop-in giving engrain-reflect this machine's extra
# project root (this config repo) and where mach meet keeps transcripts.
install_meet_and_engrain_dropin() {
  local unit_dir="$HOME/.config/systemd/user"
  install -Dm644 "$HERE/systemd/mach-meet.service" "$unit_dir/mach-meet.service"
  install -Dm644 "$HERE/systemd/mach-meet.timer"   "$unit_dir/mach-meet.timer"
  install -Dm644 "$HERE/systemd/engrain-reflect.service.d/mach.conf" "$unit_dir/engrain-reflect.service.d/mach.conf"
  echo "installed: $unit_dir/mach-meet.{service,timer}, engrain-reflect.service.d/mach.conf"
  have_user_systemd || return 0
  systemctl --user daemon-reload
  systemctl --user enable --now mach-meet.timer >/dev/null 2>&1 \
    && echo "enabled: mach-meet.timer" \
    || echo "WARNING: 'systemctl --user enable --now mach-meet.timer' failed"
}
install_meet_and_engrain_dropin

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

# mach-telegramd (machd: telegram bridge) — same soft-install
# treatment as the other systemd units: a non-systemd environment must not
# fail the whole install. Unlike the reflect/backup timers this is a
# persistent daemon (Type=simple), not a periodic oneshot, so there is
# exactly one unit, no timer. It always starts (no ConditionPathExists) --
# the telegram subsystem no-ops cleanly on its own when a real
# telegram.toml doesn't exist yet, so enabling it early is safe.
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
    echo "enabled: mach-telegramd.service (machd — the telegram bridge stays disabled until"
    echo "         ~/.local/share/mach/telegram.toml exists — see below if it doesn't yet)"
  else
    echo "WARNING: 'systemctl --user enable --now mach-telegramd.service' failed — enable it by hand once mach is on PATH."
  fi
  # `enable --now` STARTS a stopped unit but never restarts a running one,
  # so on every reinstall after the first, machd kept serving the previous
  # binary from memory. try-restart is the right verb: it
  # restarts the unit if it is running and does nothing at all if it is
  # not, so the enable path above stays the only thing that starts it.
  if ! systemctl --user try-restart mach-telegramd.service >/dev/null 2>&1; then
    echo "WARNING: 'systemctl --user try-restart mach-telegramd.service' failed — restart it by hand,"
    echo "         or it keeps running the binary it started with."
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
  echo "      cp $HERE/telegram.toml.example $cfg"
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
