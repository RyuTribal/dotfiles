#!/usr/bin/env bash
set -euo pipefail

# --- settings ---
BASE_DIR="${HOME}/notes"
EDITOR="nvim"
# ----------------

year="$(date +%Y)"
month="$(date +%m)"
day="$(date +%d)"

dir="${BASE_DIR}/${year}/${month}"
file="${dir}/${day}.md"

mkdir -p "$dir"

# If file doesn't exist yet, create it with a header
if [[ ! -f "$file" ]]; then
  {
    printf "# %s-%s-%s\n\n" "$year" "$month" "$day"
  } > "$file"
fi

# Open in your favorite terminal + editor
if command -v kitty >/dev/null 2>&1; then
  exec kitty "$EDITOR" "$file"
elif command -v alacritty >/dev/null 2>&1; then
  exec alacritty -e "$EDITOR" "$file"
elif command -v foot >/dev/null 2>&1; then
  exec foot "$EDITOR" "$file"
else
  exec "$EDITOR" "$file"
fi
