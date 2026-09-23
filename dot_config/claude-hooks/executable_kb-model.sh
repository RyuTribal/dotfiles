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

# A mach-spawned `claude -p` (card, digest, judge) is not a user session:
# it must not be handed recalled memories it never asked for, because the
# model cannot tell them from the evidence it was told to work from, and a
# card that cites an evidence id while restating an injected one is a
# provenance lie. Same guard `kb-checkpoint.sh` already carries.
[ "${MACH_KB_DIGEST:-}" = "1" ] && exit 0

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

model="$(timeout 2s "$MACH_BIN" kb model --max-chars 8000 2>/dev/null)"
rc=$?

# An installed binary older than the --max-chars flag (see install.sh
# rollout gap) rejects it outright rather than ignoring it -- fall back to
# the unflagged call and truncate here instead, so this hook degrades to
# "briefly stale binary" rather than "no mental model at all" until the
# next install. Truncation is char-based via python3 (not `head -c`, which
# can cut mid-UTF-8-character) to match what --max-chars itself promises.
if [ "$rc" -ne 0 ]; then
    model="$(timeout 2s "$MACH_BIN" kb model 2>/dev/null)"
    rc=$?
    [ "$rc" -ne 0 ] && exit 0
    model="$(printf '%s' "$model" | python3 -c '
import sys
sys.stdout.write(sys.stdin.read()[:8000])
' 2>/dev/null)"
fi

[ -z "$model" ] && exit 0

printf '%s\n' "What you know about this user and their world (your accumulated understanding from all past sessions; specific memories surface per-prompt):"
printf '%s\n' "$model"

# Index drift for the session's project, once per session. Deliberately not
# in kb-recall (per-prompt): this is a standing condition, not a per-prompt
# fact, and the recall path has a sub-500ms budget.
proj="$(basename "${CLAUDE_PROJECT_DIR:-$PWD}" | tr '[:upper:]' '[:lower:]')"
if [ -n "$proj" ]; then
    drift="$(timeout 3s "$MACH_BIN" kb projects list 2>/dev/null | awk -v p="$proj" '$1==p && /DRIFTED|never indexed/ {print}')"
    [ -n "$drift" ] && printf 'Project index drift: %s\nRe-index with the index-project skill, then run `mach kb projects mark-indexed %s`.\n' "$drift" "$proj"
fi

exit 0
