#!/usr/bin/env bash
set -euo pipefail

if ! command -v fish >/dev/null 2>&1; then
    command -v notify-send >/dev/null 2>&1 && notify-send "Private fish shell" "fish is not installed."
    exit 1
fi

launch() {
    "$@" &
    exit 0
}

try_terminal() {
    case "$1" in
        kitty)
            command -v kitty >/dev/null 2>&1 && launch kitty -1 fish --private
            ;;
        foot)
            command -v foot >/dev/null 2>&1 && launch foot fish --private
            ;;
        alacritty)
            command -v alacritty >/dev/null 2>&1 && launch alacritty -e fish --private
            ;;
        wezterm)
            command -v wezterm >/dev/null 2>&1 && launch wezterm start --always-new-process -- fish --private
            ;;
        konsole)
            command -v konsole >/dev/null 2>&1 && launch konsole -e fish --private
            ;;
        kgx|gnome-console)
            command -v kgx >/dev/null 2>&1 && launch kgx -- fish --private
            ;;
        gnome-terminal)
            command -v gnome-terminal >/dev/null 2>&1 && launch gnome-terminal -- fish --private
            ;;
        xfce4-terminal)
            command -v xfce4-terminal >/dev/null 2>&1 && launch xfce4-terminal -e fish --private
            ;;
        uxterm)
            command -v uxterm >/dev/null 2>&1 && launch uxterm -e fish --private
            ;;
        xterm)
            command -v xterm >/dev/null 2>&1 && launch xterm -e fish --private
            ;;
    esac
}

if [[ -n "${TERMINAL:-}" ]]; then
    try_terminal "${TERMINAL%% *}"
fi

for term in kitty foot alacritty wezterm konsole kgx gnome-terminal xfce4-terminal uxterm xterm; do
    try_terminal "$term"
done

command -v notify-send >/dev/null 2>&1 && notify-send "Private fish shell" "No supported terminal emulator was found."
exit 1
