#!/usr/bin/env bash
# kb-decision.sh — Claude Code UserPromptSubmit hook.
#
# Decision-cue fast path. When the user's own message reads like a decision
# ("decision:", "let's go with X", "from now on ...", "approved"), store it
# verbatim as an unreviewed memory right now — no model call, no waiting for
# the session digest. `mach kb reflect`'s curation and dedupe passes
# reconcile it against whatever the digest later extracts.
#
# Prints nothing on purpose: a UserPromptSubmit hook's stdout becomes
# injected context, and this hook only records. Never blocks the prompt:
# no `set -e`, detached `mach kb add`, every path exits 0.

MACH_BIN="${MACH_BIN:-mach}"
MAX_CHARS="${MACH_KB_DECISION_MAX_CHARS:-600}"

[ "${MACH_KB_DIGEST:-}" = "1" ] && exit 0
command -v "$MACH_BIN" >/dev/null 2>&1 || exit 0

KB_HOOK_INPUT="$(cat)"
export KB_HOOK_INPUT MAX_CHARS

# Emits "<sid>\t<text>" when the prompt is a decision, nothing otherwise.
decision="$(python3 - <<'PY' 2>/dev/null
import json, os, re, sys
try:
    d = json.loads(os.environ.get("KB_HOOK_INPUT", ""))
except Exception:
    sys.exit(0)
p = d.get("prompt", "")
sid = d.get("session_id", "")
if not isinstance(p, str) or not isinstance(sid, str):
    sys.exit(0)
text = p.strip()
if len(text) < 12 or text.startswith("/"):
    sys.exit(0)
low = text.lower()
# a secret pasted next to a decision must never land in the bank
if re.search(r"(sk-[a-z0-9]{8,}|ghp_[a-z0-9]{8,}|akia[a-z0-9]{12,}|-----begin|password\s*[:=]|token\s*[:=]|secret\s*[:=]|[a-z0-9+/]{40,}={0,2})", low):
    sys.exit(0)
cues = [
    r"^\s*decision\s*:",
    r"\bdecided\b",
    r"\blet'?s go with\b",
    r"\bwe(?:'ll| will) (?:use|go with|do)\b",
    r"^\s*go with\b",
    r"\bfrom now on\b",
    r"\bfinal answer\b",
    r"^\s*approved\b",
    r"\bsettled on\b",
    r"\bwe(?:'re| are) going with\b",
    r"\bthe plan is\b",
]
if not any(re.search(c, low, re.MULTILINE) for c in cues):
    sys.exit(0)
text = re.sub(r"\s+", " ", text)
cap = int(os.environ.get("MAX_CHARS", "600"))
if len(text) > cap:
    text = text[:cap].rstrip() + " ..."
sid = re.sub(r"[^A-Za-z0-9._-]", "", sid) or "unknown"
sys.stdout.write(sid + "\t" + text)
PY
)"

[ -z "$decision" ] && exit 0
sid="${decision%%	*}"
text="${decision#*	}"
[ -z "$text" ] && exit 0

today="$(date -u +%Y-%m-%d)"
project="$(basename "${PWD:-}" 2>/dev/null)"
[ "$project" = "$(basename "$HOME")" ] && project=""

(
    if [ -n "$project" ]; then
        timeout 30s "$MACH_BIN" kb add "In the $today session the user stated: $text" \
            --source "decision-cue $today session $sid" --project "$project" \
            --unreviewed --no-classify --importance 6 >/dev/null 2>&1
    else
        timeout 30s "$MACH_BIN" kb add "In the $today session the user stated: $text" \
            --source "decision-cue $today session $sid" \
            --unreviewed --no-classify --importance 6 >/dev/null 2>&1
    fi
) </dev/null >/dev/null 2>&1 &
disown 2>/dev/null

exit 0
