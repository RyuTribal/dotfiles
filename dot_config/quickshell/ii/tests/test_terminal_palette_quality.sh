#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

source scripts/colors/theme_helpers.sh

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

palette_json="$tmpdir/palette.json"

write_terminal_palette_json_from_theme "$palette_json" "assets/images/default_wallpaper.png" "dark" "scheme-expressive" ""

python3 - <<'PY' "$palette_json"
import json
import sys

def luminance(hex_color: str) -> float:
    hex_color = hex_color.lstrip("#")
    r = int(hex_color[0:2], 16) / 255.0
    g = int(hex_color[2:4], 16) / 255.0
    b = int(hex_color[4:6], 16) / 255.0
    return 0.2126 * r + 0.7152 * g + 0.0722 * b

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    palette = json.load(handle)

bg = palette["term0"]
fg = palette["term7"]
bright_fg = palette["term15"]

bg_lum = luminance(bg)
fg_lum = luminance(fg)
bright_fg_lum = luminance(bright_fg)

if fg_lum - bg_lum < 0.45:
    raise SystemExit(f"foreground contrast too low: bg={bg} fg={fg} delta={fg_lum - bg_lum:.3f}")

if bright_fg_lum < fg_lum:
    raise SystemExit(f"bright foreground should not be darker than base foreground: fg={fg} bright={bright_fg}")
PY
