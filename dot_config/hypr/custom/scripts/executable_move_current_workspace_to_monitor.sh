#!/usr/bin/env bash

set -euo pipefail

notify_error() {
  local message="$1"
  if command -v notify-send >/dev/null 2>&1; then
    notify-send "Hyprland workspace move" "$message"
  fi
  printf '%s\n' "$message" >&2
}

direction="${1:-next}"

case "$direction" in
  next|prev)
    ;;
  *)
    printf 'Usage: %s [next|prev]\n' "$0" >&2
    exit 2
    ;;
esac

if ! monitors_json="$(hyprctl -j monitors 2>/dev/null)"; then
  notify_error "Could not query Hyprland monitors."
  exit 1
fi

mapfile -t monitor_names < <(jq -r '.[].name' <<<"$monitors_json")

if ((${#monitor_names[@]} < 2)); then
  notify_error "Need at least two monitors to move the current workspace."
  exit 1
fi

focused_index="$(jq -r 'to_entries[] | select(.value.focused) | .key' <<<"$monitors_json")"
if [[ -z "$focused_index" || "$focused_index" == "null" ]]; then
  notify_error "Could not determine the focused monitor."
  exit 1
fi

case "$direction" in
  next)
    target_index=$(((focused_index + 1) % ${#monitor_names[@]}))
    ;;
  prev)
    target_index=$(((focused_index - 1 + ${#monitor_names[@]}) % ${#monitor_names[@]}))
    ;;
esac

target_monitor="${monitor_names[target_index]}"

# Hyprland 0.55+ with a Lua config parses `hyprctl dispatch` args as Lua, so the
# legacy `dispatch <name> <arg>` form is rejected. Pass the dispatcher instead.
if ! hyprctl dispatch "hl.dsp.workspace.move({ monitor = \"$target_monitor\" })" >/dev/null 2>&1; then
  notify_error "Failed to move the current workspace to ${target_monitor}."
  exit 1
fi

if ! hyprctl dispatch "hl.dsp.focus({ monitor = \"$target_monitor\" })" >/dev/null 2>&1; then
  notify_error "Workspace moved to ${target_monitor}, but focus did not follow."
  exit 1
fi
