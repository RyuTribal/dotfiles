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

assert_nonempty() {
    local actual="$1"
    local message="$2"

    if [[ -z "$actual" ]]; then
        echo "ASSERTION FAILED: $message" >&2
        exit 1
    fi
}

test_find_legacy_site_packages_for_module() {
    local site_packages
    site_packages="$(find_site_packages_for_module kde_material_you_colors)"
    assert_nonempty "$site_packages" "finds a site-packages path for kde_material_you_colors"
    assert_eq "/usr/lib/python3.13/site-packages" "$site_packages" "prefers the installed legacy site-packages path"
}

test_build_kde_material_you_colors_command() {
    local cmd=()
    build_kde_material_you_colors_command cmd "-d" "#112233" "1"

    assert_eq "env" "${cmd[0]}" "uses env launcher"
    assert_eq "PYTHONPATH=/usr/lib/python3.13/site-packages" "${cmd[1]}" "injects legacy site-packages"
    assert_eq "python3" "${cmd[2]}" "uses current python3"
    assert_eq "-c" "${cmd[3]}" "uses inline entrypoint execution"
    assert_eq "-d" "${cmd[5]}" "passes the requested mode"
    assert_eq "--color" "${cmd[6]}" "passes color flag"
    assert_eq "#112233" "${cmd[7]}" "passes selected color"
    assert_eq "-sv" "${cmd[8]}" "passes scheme variant flag"
    assert_eq "1" "${cmd[9]}" "passes scheme variant number"
}

test_find_legacy_site_packages_for_module
test_build_kde_material_you_colors_command
