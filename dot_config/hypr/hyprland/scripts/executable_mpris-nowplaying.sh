#!/usr/bin/env bash
set -euo pipefail
artist="$(playerctl metadata artist 2>/dev/null || true)"
title="$(playerctl metadata title 2>/dev/null || true)"
state="$(playerctl status 2>/dev/null || true)"
[ -z "$artist$title$state" ] && { echo ""; exit 0; }
icon=$([ "$state" = "Playing" ] && echo "▶" || echo "⏸")
printf '<b>%s</b> — %s  <span alpha="70%%">%s</span>\n' "${artist:-Unknown}" "${title:-Untitled}" "$icon"

