#!/usr/bin/env bash
# kb-recall.sh — Claude Code UserPromptSubmit hook.
#
# Searches `mach kb` for memories related to the incoming prompt and, when
# any score high enough, prints them to stdout. Claude Code adds a
# UserPromptSubmit hook's plain-text stdout as additionalContext automatically
# on exit 0 — no JSON envelope needed.
#
# Contract: this hook must NEVER block a prompt. Every path below ends in
# `exit 0` (no `set -e`, so a failing command falls through instead of
# aborting the script) — an ollama outage, a missing `mach` binary, or any
# other failure degrades to silence, not an error.

MACH_BIN="${MACH_BIN:-mach}"
# This is the only caller that should pass --touch: it's the recall path,
# so a hit surfacing here is an actual injection, not just a manual query.
# --min-score applies the threshold engine-side (post-limit) so reinforcement
# only ever touches rows that clear it, not every row the ranked search
# happened to return before this script's own filter below runs.
SCORE_THRESHOLD="0.45"
MIN_PROMPT_LEN=12

input="$(cat)"

prompt="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    p = data.get("prompt", "")
    print(p if isinstance(p, str) else "")
except Exception:
    print("")
' 2>/dev/null)"

# Defensive fallback if python3 is unavailable or produced nothing, and jq is.
if [ -z "$prompt" ] && command -v jq >/dev/null 2>&1; then
    prompt="$(printf '%s' "$input" | jq -r '.prompt // empty' 2>/dev/null)"
fi

[ -z "$prompt" ] && exit 0
[ "${#prompt}" -lt "$MIN_PROMPT_LEN" ] && exit 0

case "$prompt" in
    /*) exit 0 ;;
esac

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

out_file="$(mktemp 2>/dev/null)" || exit 0
err_file="$(mktemp 2>/dev/null)" || { rm -f "$out_file"; exit 0; }
trap 'rm -f "$out_file" "$err_file"' EXIT

timeout 2s "$MACH_BIN" kb search "$prompt" --limit 4 --json --touch --min-score "$SCORE_THRESHOLD" \
    >"$out_file" 2>"$err_file"
rc=$?

# Nonzero exit (including a timeout kill) means recall didn't complete
# cleanly — stay silent.
[ "$rc" -ne 0 ] && exit 0

# `mach kb search` degrades to a substring fallback (every hit forced to
# score 1.0) when it can't reach ollama for embeddings — that fallback is
# noisy/unreliable for automatic injection, so treat the warning it prints
# on stderr the same as an outage: silence, not a flood of loose matches.
if grep -qi 'cannot reach ollama\|falling back to substring match' "$err_file" 2>/dev/null; then
    exit 0
fi

context="$(python3 -c '
import json, sys

# "score" is now the ranked blend (0.70*sim + 0.20*recency + 0.10*strength),
# not plain cosine — same field name, already-filtered by --min-score above,
# so this is a redundant-but-harmless second check.
THRESHOLD = 0.45

try:
    with open(sys.argv[1], "r") as f:
        hits = json.load(f)
except Exception:
    sys.exit(0)

if not isinstance(hits, list):
    sys.exit(0)

lines = []
for h in hits:
    if not isinstance(h, dict):
        continue
    try:
        score = float(h.get("score", 0))
    except (TypeError, ValueError):
        continue
    if score < THRESHOLD:
        continue
    content = (h.get("content") or "").strip()
    if not content:
        continue
    source = h.get("source") or "unknown"
    date = (h.get("created_at") or "")[:10]
    lines.append("- [{}, {}] {}".format(source, date, content))

if lines:
    print("Knowledge bank recall (mach kb):")
    for l in lines:
        print(l)
' "$out_file" 2>/dev/null)"

[ -n "$context" ] && printf '%s\n' "$context"

exit 0
