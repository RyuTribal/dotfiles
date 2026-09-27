#!/usr/bin/env bash
# engrain-subagent.sh — Claude Code SubagentStart hook.
#
# Subagents get no SessionStart (engrain-model.sh runs once for the main
# session only) and no per-prompt recall (engrain-recall.sh is UserPromptSubmit,
# and a subagent is never prompted directly) — so without this hook they
# start with zero knowledge of the user. This hook hands them the same
# compact mental model engrain-model.sh gives the main session, plus the
# session project's card, once when the subagent starts.
#
# Contract: this hook must NEVER block a subagent from starting. No
# `set -e`, and every path ends in `exit 0` — a missing `engrain` binary, an
# empty store, unparseable input, or any other failure degrades to
# silence, not an error.

ENGRAIN_BIN="${ENGRAIN_BIN:-engrain}"

# A engrain-spawned `claude -p` (card, digest, judge) is not a user session:
# it must not be handed recalled memories it never asked for. Same guard
# engrain-model.sh and engrain-recall.sh carry.
[ "${ENGRAIN_DIGEST:-}" = "1" ] && exit 0

command -v "$ENGRAIN_BIN" >/dev/null 2>&1 || exit 0

input="$(cat)"

# Pull cwd/agent_type out of the SubagentStart hook JSON. Any parse
# failure degrades to empty strings: an empty cwd just loses the project
# card, and an empty agent_type never matches the skip-list below.
parsed="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    d = {}
sys.stdout.write((d.get("cwd") or "") + "\n")
sys.stdout.write((d.get("agent_type") or "") + "\n")
' 2>/dev/null)"
cwd="$(printf '%s\n' "$parsed" | sed -n '1p')"
agent_type="$(printf '%s\n' "$parsed" | sed -n '2p')"

# These agent types do their own thing and need no user context.
case "$agent_type" in
    claude-code-guide|statusline-setup) exit 0 ;;
esac

model="$(timeout 2s "$ENGRAIN_BIN" model --max-chars 4000 2>/dev/null)"
rc=$?

# Same fallback as engrain-model.sh: an installed binary older than --max-chars
# rejects it outright, so retry unflagged and truncate here (char-safe via
# python3) instead of silently losing the mental model until reinstall.
if [ "$rc" -ne 0 ]; then
    model="$(timeout 2s "$ENGRAIN_BIN" model 2>/dev/null)"
    if [ $? -eq 0 ]; then
        model="$(printf '%s' "$model" | python3 -c '
import sys
sys.stdout.write(sys.stdin.read()[:4000])
' 2>/dev/null)"
    else
        model=""
    fi
fi

context=""
if [ -n "$model" ]; then
    context="What you know about this user (compact; per-prompt recall does not reach subagents):
$model"
fi

if [ -n "$cwd" ]; then
    proj="$(basename "$cwd" | tr '[:upper:]' '[:lower:]')"
    card_json="$(timeout 2s "$ENGRAIN_BIN" search "$proj" --cwd "$cwd" --json --limit 1 2>/dev/null)"
    if [ -n "$card_json" ]; then
        card="$(printf '%s' "$card_json" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    d = {}
card = d.get("project_card")
if card:
    sys.stdout.write(card)
' 2>/dev/null)"
        if [ -n "$card" ]; then
            if [ -n "$context" ]; then
                context="$context

$card"
            else
                context="$card"
            fi
        fi
    fi
fi

[ -z "$context" ] && exit 0

# Same one-line nudge engrain-recall.py appends to prompt-time recall whenever
# it injects something: subagents get no per-prompt recall of their own
# (see header), so this mental model + project card is the only place
# they'll ever see the same "use it, search deeper, or write it down" cue.
ACTION_LINE='If these memories bear on the task, use them; if they are close but incomplete, run `engrain search "<topic>"` or `engrain why <id>` before exploring files. Save durable new facts with `engrain add`.'

# Emit the hookSpecificOutput envelope via python3 so escaping is
# correct, and truncate to 9,000 chars there too (char-based, not the
# byte-based cut a shell `head -c` would risk mid-UTF-8-character) —
# comfortably under the 10,000-char limit past which Claude Code replaces
# the field with a file path + preview. The action line's own length is
# reserved out of that 9,000-char budget FIRST, then appended after
# truncating the base context, so it is never itself the part that gets
# cut off.
printf '%s' "$context" | ACTION_LINE="$ACTION_LINE" python3 -c '
import json, os, sys
context = sys.stdin.read()
action = os.environ.get("ACTION_LINE", "")
budget = 9000
suffix = ("\n\n" + action) if action else ""
if len(context) + len(suffix) > budget:
    context = context[: max(0, budget - len(suffix))]
context = context + suffix
print(json.dumps({"hookSpecificOutput": {"hookEventName": "SubagentStart", "additionalContext": context}}))
'

exit 0
