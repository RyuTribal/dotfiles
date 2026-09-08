#!/usr/bin/env bash
# kb-checkpoint.sh — Claude Code Stop + PreCompact hook.
#
# Mid-session capture. `mach kb ingest-sessions` used to run only at
# SessionEnd (or the 10-minute idle sweep), so a long session's decisions
# reached the knowledge bank hours later, after compaction had already
# summarised them away, capped at a handful of facts. This hook runs the
# incremental digest (`--partial`: only transcript lines since the last
# checkpoint, see engines/kb/src/ingest.rs) on two triggers:
#
#   Stop        every CHECKPOINT_EVERY_TURNS assistant turns, or once
#               CHECKPOINT_EVERY_SECS have passed since the last checkpoint,
#               whichever first. Detached, so the turn never waits on it.
#   PreCompact  unconditionally and synchronously (bounded by `timeout`),
#               so nothing about to be compacted away goes uncaptured.
#
# Contract: never block, never fail. No `set -e`; every path ends in exit 0.
# Prints nothing: stdout from a Stop hook is shown to the user, and this
# hook has nothing to say.

MACH_BIN="${MACH_BIN:-mach}"
CHECKPOINT_EVERY_TURNS="${MACH_KB_CHECKPOINT_TURNS:-8}"
CHECKPOINT_EVERY_SECS="${MACH_KB_CHECKPOINT_SECS:-900}"

# Nested automated claude runs (reflect, improve, meet) must not checkpoint
# themselves into the bank.
[ "${MACH_KB_DIGEST:-}" = "1" ] && exit 0
command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

input="$(cat)"

read -r session_id event <<EOF
$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    s = d.get("session_id", "")
    e = d.get("hook_event_name", "")
    print((s if isinstance(s, str) else "") or "-", (e if isinstance(e, str) else "") or "-")
except Exception:
    print("- -")
' 2>/dev/null)
EOF

[ -z "$session_id" ] || [ "$session_id" = "-" ] && exit 0
# session ids are uuids; refuse anything that could not be a filename
case "$session_id" in
    *[!A-Za-z0-9._-]*) exit 0 ;;
esac

run_partial_detached() {
    (
        timeout 120s "$MACH_BIN" kb ingest-sessions --session-id "$session_id" --partial >/dev/null 2>&1
    ) </dev/null >/dev/null 2>&1 &
    disown 2>/dev/null
}

if [ "$event" = "PreCompact" ]; then
    timeout 90s "$MACH_BIN" kb ingest-sessions --session-id "$session_id" --partial >/dev/null 2>&1
    exit 0
fi

# Stop: counter file holds "<turns> <epoch-of-last-checkpoint>"
state_dir="${XDG_RUNTIME_DIR:-/tmp}/mach-kb-checkpoint"
mkdir -p "$state_dir" 2>/dev/null || exit 0
state_file="$state_dir/$session_id"
now="$(date +%s)"
turns=0
last="$now"
if [ -f "$state_file" ]; then
    read -r turns last <"$state_file" 2>/dev/null
    case "$turns" in ''|*[!0-9]*) turns=0 ;; esac
    case "$last" in ''|*[!0-9]*) last="$now" ;; esac
fi
turns=$((turns + 1))
elapsed=$((now - last))

if [ "$turns" -ge "$CHECKPOINT_EVERY_TURNS" ] || [ "$elapsed" -ge "$CHECKPOINT_EVERY_SECS" ]; then
    printf '0 %s\n' "$now" >"$state_file" 2>/dev/null
    run_partial_detached
else
    printf '%s %s\n' "$turns" "$last" >"$state_file" 2>/dev/null
fi

exit 0
