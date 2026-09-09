#!/usr/bin/env bash
# kb-capture.sh — Claude Code SessionEnd hook.
#
# Engagement-gated reinforcement: this hook no longer distills facts itself
# (it used to spawn its own nested `claude -p --model haiku` digest call on
# every session end). That work is now owned by `mach kb ingest-sessions`,
# which reads the same transcript plus kb-recall.sh's per-session recall log
# to BOTH judge which injected memories were actually engaged with (not
# merely shown) AND extract the durable-facts digest this hook used to do
# alone — see engines/kb/src/ingest.rs.
#
# This hook is now just the fast trigger: a clean SessionEnd gets its
# session processed near-instantly (`--session-id` bypasses the
# opportunistic sweep's idle-time gate, since this hook already knows the
# session just ended). A crash or otherwise unclean exit — this hook never
# firing at all — is caught instead by the mach-reflect timer's
# opportunistic `ExecStartPre` sweep (no `--session-id`, mtime + processed-set
# gated), so processing never depends on SessionEnd firing.
#
# Contract: this hook must NEVER block session teardown. No `set -e`, and
# every path ends in `exit 0`.

MACH_BIN="${MACH_BIN:-mach}"

# A mach-spawned `claude -p` (card, digest, judge) is not a user session:
# it must not be ingested as one. Without this, every card and judge call
# spawned by mach ran `kb ingest-sessions` over its own throwaway
# transcript on exit. Same guard `kb-checkpoint.sh` already carries.
[ "${MACH_KB_DIGEST:-}" = "1" ] && exit 0

command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

input="$(cat)"

session_id="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
    s = data.get("session_id", "")
    print(s if isinstance(s, str) else "")
except Exception:
    print("")
' 2>/dev/null)"

if [ -z "$session_id" ] && command -v jq >/dev/null 2>&1; then
    session_id="$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)"
fi

# ingest-sessions can take a while (transcript filtering plus up to two
# haiku calls) — session teardown cancels hooks that block that long, so
# this runs detached, same pattern kb-capture.sh's own digest call used to
# follow.
(
    if [ -n "$session_id" ]; then
        timeout 120s "$MACH_BIN" kb ingest-sessions --session-id "$session_id" >/dev/null 2>&1
    else
        # No session id available (shouldn't normally happen) — fall back
        # to the bare opportunistic sweep so a crash-safety net still runs,
        # even though this exact session won't qualify until it's stale.
        timeout 120s "$MACH_BIN" kb ingest-sessions >/dev/null 2>&1
    fi
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null

exit 0
