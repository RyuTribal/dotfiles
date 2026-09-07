#!/usr/bin/env bash
# kb-model.sh — Claude Code SessionStart hook.
#
# Prints the `mach kb` mental model (all active insights/themes, tree-
# ordered, no source ids or memory leaves) once at session start. Claude
# Code adds a SessionStart hook's plain-text stdout as additionalContext
# automatically on exit 0 — no JSON envelope needed, same convention
# kb-recall.sh relies on for UserPromptSubmit.
#
# Contract: this hook must NEVER block session start. No `set -e`, and
# every path ends in `exit 0` — a missing `mach` binary, an empty store, or
# any other failure degrades to silence, not an error.

MACH_BIN="${MACH_BIN:-mach}"

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

model="$(timeout 2s "$MACH_BIN" kb model 2>/dev/null)"
rc=$?

[ "$rc" -ne 0 ] && exit 0
[ -z "$model" ] && exit 0

printf '%s\n' "Mental model (mach kb — beliefs derived from accumulated memories; specifics arrive per-prompt via recall):"
printf '%s\n' "$model"

exit 0
