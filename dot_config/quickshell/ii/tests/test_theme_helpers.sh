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

assert_matches() {
    local pattern="$1"
    local actual="$2"
    local message="$3"

    if [[ ! "$actual" =~ $pattern ]]; then
        echo "ASSERTION FAILED: $message" >&2
        echo "  pattern: $pattern" >&2
        echo "  actual:  $actual" >&2
        exit 1
    fi
}

test_normalize_hex_color() {
    assert_eq "#253139" "$(normalize_hex_color "253139")" "adds leading hash"
    assert_eq "#ABCDEF" "$(normalize_hex_color "#ABCDEF")" "preserves existing hash"
}

test_sample_image_color() {
    local color
    color="$(sample_image_color "assets/images/default_wallpaper.png")"
    assert_matches '^#[0-9A-Fa-f]{6}$' "$color" "samples a six-digit hex color"
}

test_build_matugen_theme_command_for_image() {
    local cmd=()
    build_matugen_theme_command cmd "assets/images/default_wallpaper.png" "dark" "scheme-expressive" ""

    assert_eq "matugen" "${cmd[0]}" "uses matugen"
    assert_eq "color" "${cmd[1]}" "uses color mode for wallpaper-derived themes"
    assert_eq "hex" "${cmd[2]}" "uses hex color input"
    assert_matches '^#[0-9A-Fa-f]{6}$' "${cmd[3]}" "passes sampled image color"
    assert_eq "--mode" "${cmd[4]}" "includes mode flag"
    assert_eq "dark" "${cmd[5]}" "passes requested mode"
    assert_eq "--type" "${cmd[6]}" "includes type flag"
    assert_eq "scheme-expressive" "${cmd[7]}" "passes requested scheme type"
}

test_build_matugen_theme_command_for_explicit_color() {
    local cmd=()
    build_matugen_theme_command cmd "" "light" "scheme-tonal-spot" "253139"

    assert_eq "#253139" "${cmd[3]}" "normalizes explicit colors"
    assert_eq "light" "${cmd[5]}" "passes requested light mode"
    assert_eq "scheme-tonal-spot" "${cmd[7]}" "passes requested explicit scheme"
}

test_normalize_hex_color
test_sample_image_color
test_build_matugen_theme_command_for_image
test_build_matugen_theme_command_for_explicit_color
