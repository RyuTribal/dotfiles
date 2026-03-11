#!/usr/bin/env bash

set -euo pipefail

cd "$(dirname "$0")/.."

source scripts/colors/theme_helpers.sh

assert_eq() {
    local expected="$1"
    local actual="$2"
    local message="$3"

    if [[ "$expected" != "$actual" ]]; then
        echo "ASSERTION FAILED: $message" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        exit 1
    fi
}

assert_file_contains() {
    local pattern="$1"
    local path="$2"
    local message="$3"

    if ! grep -Eq "$pattern" "$path"; then
        echo "ASSERTION FAILED: $message" >&2
        echo "  pattern: $pattern" >&2
        echo "  file:    $path" >&2
        cat "$path" >&2
        exit 1
    fi
}

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

base16_json="$tmpdir/base16.json"
palette_json="$tmpdir/terminal-palette.json"
sequences_file="$tmpdir/sequences.txt"

cat > "$base16_json" <<'EOF'
{
  "base16": {
    "base00": {"dark": "101112"},
    "base01": {"dark": "202122"},
    "base02": {"dark": "303132"},
    "base03": {"dark": "404142"},
    "base04": {"dark": "505152"},
    "base05": {"dark": "606162"},
    "base06": {"dark": "707172"},
    "base07": {"dark": "808182"},
    "base08": {"dark": "909192"},
    "base09": {"dark": "a0a1a2"},
    "base0a": {"dark": "b0b1b2"},
    "base0b": {"dark": "c0c1c2"},
    "base0c": {"dark": "d0d1d2"},
    "base0d": {"dark": "e0e1e2"},
    "base0e": {"dark": "f0f1f2"},
    "base0f": {"dark": "111213"}
  }
}
EOF

write_terminal_palette_json_from_base16 "$base16_json" "$palette_json" "dark"

assert_eq "#101112" "$(jq -r '.term0' "$palette_json")" "term0 maps from base00"
assert_eq "#909192" "$(jq -r '.term1' "$palette_json")" "term1 maps from base08"
assert_eq "#c0c1c2" "$(jq -r '.term2' "$palette_json")" "term2 maps from base0b"
assert_eq "#b0b1b2" "$(jq -r '.term3' "$palette_json")" "term3 maps from base0a"
assert_eq "#e0e1e2" "$(jq -r '.term4' "$palette_json")" "term4 maps from base0d"
assert_eq "#f0f1f2" "$(jq -r '.term5' "$palette_json")" "term5 maps from base0e"
assert_eq "#d0d1d2" "$(jq -r '.term6' "$palette_json")" "term6 maps from base0c"
assert_eq "#606162" "$(jq -r '.term7' "$palette_json")" "term7 maps from base05"
assert_eq "#404142" "$(jq -r '.term8' "$palette_json")" "term8 maps from base03"
assert_eq "#808182" "$(jq -r '.term15' "$palette_json")" "term15 maps from base07"

render_terminal_sequences_file "$palette_json" "scripts/colors/terminal/sequences.txt" "$sequences_file" "85"

assert_file_contains '\]4;0;#101112' "$sequences_file" "sequence file includes term0"
assert_file_contains '\]4;10;#c0c1c2' "$sequences_file" "sequence file includes bright green"
assert_file_contains '\]10;#606162' "$sequences_file" "foreground uses term7"
assert_file_contains '\]11;\[100\]#101112' "$sequences_file" "background alpha follows the shipped template"
