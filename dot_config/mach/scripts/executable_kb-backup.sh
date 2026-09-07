#!/usr/bin/env bash
# mach-kb-backup — nightly `mach kb export` + prune to the last 14 backups.
#
# Invoked by mach-kb-backup.service. A systemd specifier in ExecStart would
# need %% escaping for the `date +%F` format string and can't do the
# "keep the last N" arithmetic at all — a script sidesteps both, so the
# unit just runs this and stays a one-line ExecStart.
set -euo pipefail

BACKUP_DIR="${MACH_KB_BACKUP_DIR:-$HOME/.local/share/mach/backups}"
KEEP=14
MACH_BIN="${MACH_BIN:-$HOME/.local/bin/mach}"

mkdir -p "$BACKUP_DIR"

out="$BACKUP_DIR/kb-$(date +%F).jsonl"
"$MACH_BIN" kb export --out "$out"

# Prune to the last $KEEP backups (oldest deleted first), matched by our own
# naming pattern so a stray file some other process drops in the directory
# is left alone. Sorting by filename works because the date format (%F =
# YYYY-MM-DD) sorts lexicographically the same as chronologically.
mapfile -t backups < <(find "$BACKUP_DIR" -maxdepth 1 -name 'kb-*.jsonl' -printf '%f\n' | sort)
count=${#backups[@]}
if (( count > KEEP )); then
  to_delete=$(( count - KEEP ))
  for ((i = 0; i < to_delete; i++)); do
    rm -f -- "$BACKUP_DIR/${backups[$i]}"
  done
fi
