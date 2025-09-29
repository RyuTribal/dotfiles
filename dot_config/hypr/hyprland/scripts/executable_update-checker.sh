#!/usr/bin/env bash
# Prints JSON like: {"repo":3,"aur":1}
# Requires: pacman-contrib (for checkupdates). AUR count via yay/paru if present.

set -euo pipefail

repo_count=0
aur_count=0

# Use a temp DB so we don't touch the live pacman DB (per checkupdates design).
# checkupdates exits 2 if dbs are out-of-date; let it refresh its temp copy.
if command -v checkupdates >/dev/null 2>&1; then
  # Some systems want LANG=C to keep parsing stable
  repo_count="$(checkupdates 2>/dev/null | wc -l | tr -d ' ')"
else
  repo_count=0
fi

# AUR helpers (query-only, no root needed)
if command -v yay >/dev/null 2>&1; then
  aur_count="$(yay -Qua 2>/dev/null | wc -l | tr -d ' ')"
elif command -v paru >/dev/null 2>&1; then
  aur_count="$(paru -Qua 2>/dev/null | wc -l | tr -d ' ')"
else
  aur_count=0
fi

printf '{"repo":%s,"aur":%s}\n' "${repo_count:-0}" "${aur_count:-0}"
