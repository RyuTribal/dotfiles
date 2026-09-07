#!/usr/bin/env bash
# kb-recall.sh — Claude Code UserPromptSubmit hook.
#
# Searches `mach kb` for memories related to the incoming prompt and, when
# any score high enough, prints them to stdout. Claude Code adds a
# UserPromptSubmit hook's plain-text stdout as additionalContext automatically
# on exit 0 — no JSON envelope needed.
#
# Engagement-gated reinforcement: this hook no longer reinforces anything
# itself (`--touch` is gone — impression is not engagement, the same gap IR
# ranking draws between a shown result and a clicked one). Instead it
# appends the ids it actually injects, per prompt, to a per-session recall
# log at ~/.local/share/mach/recall-log/<session_id>.jsonl. `mach kb
# ingest-sessions` reads that log back against the session's own transcript
# once the session is over, and reinforces only the memories the
# conversation actually engaged with — see `engines/kb/src/ingest.rs`.
#
# Contract: this hook must NEVER block a prompt. Every path below ends in
# `exit 0` (no `set -e`, so a failing command falls through instead of
# aborting the script) — an ollama outage, a missing `mach` binary, or any
# other failure degrades to silence, not an error.

MACH_BIN="${MACH_BIN:-mach}"
# --min-score applies the threshold engine-side (post-limit) so the recall
# log only ever records ids that clear it, not every row the ranked search
# happened to return before this script's own filter below runs.
SCORE_THRESHOLD="0.45"
MIN_PROMPT_LEN=12
RECALL_LOG_DIR="$HOME/.local/share/mach/recall-log"

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

session_id="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    s = data.get("session_id", "")
    print(s if isinstance(s, str) else "")
except Exception:
    print("")
' 2>/dev/null)"

# Defensive fallback if python3 is unavailable or produced nothing, and jq is.
if [ -z "$prompt" ] && command -v jq >/dev/null 2>&1; then
    prompt="$(printf '%s' "$input" | jq -r '.prompt // empty' 2>/dev/null)"
fi
if [ -z "$session_id" ] && command -v jq >/dev/null 2>&1; then
    session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
fi

[ -z "$prompt" ] && exit 0
[ "${#prompt}" -lt "$MIN_PROMPT_LEN" ] && exit 0

case "$prompt" in
    /*) exit 0 ;;
esac

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

out_file="$(mktemp 2>/dev/null)" || exit 0
err_file="$(mktemp 2>/dev/null)" || { rm -f "$out_file"; exit 0; }
ids_file="$(mktemp 2>/dev/null)" || { rm -f "$out_file" "$err_file"; exit 0; }
trap 'rm -f "$out_file" "$err_file" "$ids_file"' EXIT

timeout 2s "$MACH_BIN" kb search "$prompt" --limit 4 --json --min-score "$SCORE_THRESHOLD" \
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

# Provenance-visible recall: a raw source tag ("note:foo", "session-digest",
# "meeting:2026-...") is a fine audit trail but a poor thing to hand the
# model directly — it invites treating an auto-extracted digest line and a
# hand-typed note as equally authoritative. This maps each source-class to
# a plain phrase saying HOW it was learned, so authority stays legible:
# something the user said directly ("you told me") reads differently from
# something picked up secondhand from a session or meeting transcript.
# Insights/themes never go through this — they keep their own
# [derived belief]/[derived theme] markers below, a separate axis (a belief
# ABOUT the user derived from evidence, not a provenance class).
def source_phrase(source):
    s = (source or "").strip()
    if s.startswith("meeting:"):
        return "from a meeting"
    if s.startswith("memory-backfill:") or s.startswith("memory-backfill"):
        return "from earlier project memory"
    if s == "session-digest" or s.startswith("session-digest:") \
            or s == "transcript-backfill" or s.startswith("transcript-backfill:"):
        return "picked up from a session"
    if s.startswith("note:") or s in ("kb design", "manual", ""):
        return "you told me"
    # Any other/unrecognized source (including a plain reviewed row with no
    # prefix at all) is deliberate, reviewed input by default — the same
    # bucket as "note:"/"manual" — never the auto-extracted one, so an
    # unrecognized tag can never masquerade as more authoritative than it is.
    return "you told me"

try:
    with open(sys.argv[1], "r") as f:
        hits = json.load(f)
except Exception:
    sys.exit(0)

if not isinstance(hits, list):
    sys.exit(0)

# Session-deduped injection: a memory id that was already injected within
# the LAST 10 recall-log entries (a sliding window over the current session
# log, one entry per prompt that injected anything) is suppressed this time
# -- re-showing the same top memories on every prompt is pure token waste.
# The window slides, not a one-shot "seen ever" flag: once a memory has been
# out of the last 10 entries for a while it is fair game to surface again.
# Read BEFORE the entry for this prompt is appended (that append happens in
# the bash below, after this script exits), so "last 10" here means the 10
# most recent *prior* prompts. A missing/unreadable/corrupt log, or any
# unparseable line within it, just means an empty suppression set -- never
# worth failing recall over.
suppressed = set()
log_path = sys.argv[3] if len(sys.argv) > 3 else ""
if log_path:
    try:
        with open(log_path, "r") as f:
            log_lines = [ln for ln in f if ln.strip()]
    except Exception:
        log_lines = []
    for ln in log_lines[-10:]:
        try:
            entry = json.loads(ln)
        except Exception:
            continue
        for n in entry.get("ids") or []:
            if isinstance(n, int):
                suppressed.add(n)

lines = []
ids = []
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
    date = (h.get("created_at") or "")[:10]
    if h.get("derived"):
        # An insight (or, at level 2, a theme across several insights) from
        # `mach kb reflect`, not a raw stored memory — a belief derived
        # from >= 2 memories (or >= 2 insights, for a theme), not something
        # the user said verbatim. Flag it distinctly so it is not mistaken
        # for a direct quote or fact the way a plain memory recall would be
        # treated, and further distinguish a theme from a plain insight.
        # Insights/themes are never touched by engagement-gated
        # reinforcement either (see store::search_insights_ranked — no
        # strength term to begin with), so their ids are deliberately
        # excluded from the recall log below, and never subject to the
        # session-dedupe window either (there is no id here to dedupe on).
        confidence = h.get("confidence")
        conf_str = " (confidence {:.2})".format(confidence) if isinstance(confidence, (int, float)) else ""
        label = "derived theme" if h.get("level") == 2 else "derived belief"
        lines.append("- [{}, {}]{} {}".format(label, date, conf_str, content))
    else:
        mem_id = h.get("id")
        if isinstance(mem_id, int) and mem_id in suppressed:
            continue
        phrase = source_phrase(h.get("source"))
        lines.append("- [{}] {} ({})".format(date, content, phrase))
        if isinstance(mem_id, int):
            ids.append(mem_id)

if lines:
    print("You remember (your memory of this user from past sessions — use it first rather than re-exploring; each entry notes how it was learned):")
    for l in lines:
        print(l)

# Only ids that were actually rendered above are logged here (a suppressed
# id never reaches `ids`) -- the engagement sweep (`mach kb ingest-sessions`)
# depends on this log meaning "shown", not "considered".
with open(sys.argv[2], "w") as f:
    json.dump(ids, f)
' "$out_file" "$ids_file" "${session_id:+$RECALL_LOG_DIR/$session_id.jsonl}" 2>/dev/null)"

[ -n "$context" ] && printf '%s\n' "$context"

# Append this prompt's injected ids to the session's recall log, so `mach kb
# ingest-sessions` can later pair them against the transcript and judge
# engagement. Best-effort: a missing session id, an unwritable log dir, or
# an empty ids list all just mean no log line this time — never worth
# failing the hook over.
if [ -n "$session_id" ]; then
    ids_json="$(cat "$ids_file" 2>/dev/null)"
    if [ -n "$ids_json" ] && [ "$ids_json" != "[]" ]; then
        mkdir -p "$RECALL_LOG_DIR" 2>/dev/null && {
            ts="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
            printf '{"ts":"%s","ids":%s}\n' "$ts" "$ids_json" >>"$RECALL_LOG_DIR/$session_id.jsonl" 2>/dev/null
        }
    fi
fi

exit 0
