#!/usr/bin/env bash
# Opens a terminal and performs a full system update.
# Prefers yay/paru (handles repo + AUR). Falls back to pacman for just repos.

set -euo pipefail

# Pick a terminal emulator you have installed
term_cmd=""
for t in foot kitty alacritty wezterm gnome-terminal konsole xfce4-terminal; do
  if command -v "$t" >/dev/null 2>&1; then
    term_cmd="$t"
    break
  fi
done

if [[ -z "$term_cmd" ]]; then
  # Last resort: run inline (no terminal). Useful if you bind it to a launcher that opens a term.
  term_cmd=""
fi

update_cmd=""
if command -v yay >/dev/null 2>&1; then
  update_cmd='yay -Syu --devel'
elif command -v paru >/dev/null 2>&1; then
  update_cmd='paru -Syu --devel'
else
  # Repo-only update. pacman will prompt via sudo/polkit if in a terminal.
  update_cmd='sudo pacman -Syu'
fi

# Give the user a summary + prompt at the end so the terminal stays visible.
shell_line="echo '>>> Running: ${update_cmd}'; ${update_cmd}; echo; echo 'Done. Press Enter to close.'; read _"

if [[ -n "$term_cmd" ]]; then
  case "$term_cmd" in
  gnome-terminal) exec "$term_cmd" -- bash -lc "$shell_line" ;;
  konsole) exec "$term_cmd" -e bash -lc "$shell_line" ;;
  xfce4-terminal) exec "$term_cmd" -e bash -lc "$shell_line" ;;
  *) exec "$term_cmd" -e bash -lc "$shell_line" ;;
  esac
else
  # No terminal found; just run in place
  bash -lc "$shell_line"
fi
