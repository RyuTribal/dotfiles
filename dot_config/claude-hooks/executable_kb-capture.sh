#!/usr/bin/env bash
# kb-capture.sh — Claude Code SessionEnd hook.
#
# Distills up to 5 durable, cross-session-worthy facts about the USER out of
# this session's transcript tail (preferences, projects, people,
# commitments — not code details, not session mechanics) and stores each as
# an unreviewed `mach kb` candidate for later curation via `mach kb review`.
#
# Contract: this hook must NEVER block session teardown. No `set -e`, and
# every path ends in `exit 0`.

# Recursion guard: the `claude -p` digest call below is itself a
# non-interactive session and fires its own SessionEnd. MACH_KB_DIGEST=1 is
# set only when *we* invoke it, so a nested firing of this same hook exits
# immediately instead of spawning another digest call forever.
if [ "${MACH_KB_DIGEST:-}" = "1" ]; then
    exit 0
fi

MACH_BIN="${MACH_BIN:-mach}"
CLAUDE_BIN="${CLAUDE_BIN:-claude}"
TAIL_LINES=400
MIN_TRANSCRIPT_LINES=200

input="$(cat)"

transcript_path="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    p = data.get("transcript_path", "")
    print(p if isinstance(p, str) else "")
except Exception:
    print("")
' 2>/dev/null)"

if [ -z "$transcript_path" ] && command -v jq >/dev/null 2>&1; then
    transcript_path="$(printf '%s' "$input" | jq -r '.transcript_path // empty' 2>/dev/null)"
fi

[ -z "$transcript_path" ] && exit 0
[ -f "$transcript_path" ] || exit 0

line_count="$(wc -l < "$transcript_path" 2>/dev/null)"
[ -z "$line_count" ] && line_count=0
if [ "$line_count" -lt "$MIN_TRANSCRIPT_LINES" ] 2>/dev/null; then
    exit 0
fi

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0
command -v "$CLAUDE_BIN" >/dev/null 2>&1 || exit 0

tail_content="$(tail -n "$TAIL_LINES" "$transcript_path" 2>/dev/null)"
[ -z "$tail_content" ] && exit 0

digest_prompt='You are extracting durable, cross-session-worthy facts about the USER from the tail of a Claude Code session transcript (JSONL below). Extract at most 5 facts: preferences, projects, people, or commitments that would still matter in a future, unrelated session. Do NOT extract code details, file contents, tool-call mechanics, or anything specific only to this one task. Never include secrets, credentials, tokens, or passwords. Output one fact per line, plain text, no numbering, no bullets, no preamble, no markdown. If nothing qualifies, output nothing at all — not even a note saying so.'

facts="$(printf '%s\n\n---TRANSCRIPT TAIL---\n%s\n' "$digest_prompt" "$tail_content" \
    | MACH_KB_DIGEST=1 timeout 60s "$CLAUDE_BIN" -p --model haiku \
        --permission-prompts none \
        --disallowedTools "Bash Edit Write NotebookEdit WebFetch WebSearch Agent" \
        2>/dev/null)"

[ -z "$facts" ] && exit 0

printf '%s\n' "$facts" | while IFS= read -r line; do
    line="$(printf '%s' "$line" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
    [ -z "$line" ] && continue
    printf '%s' "$line" | "$MACH_BIN" kb add - --source "session-digest" --unreviewed >/dev/null 2>&1
done

exit 0
