#!/usr/bin/env python3
"""PreToolUse guard: Claude may read the charter, never change it.

~/.config/mach/charter.toml holds the fixed nature, goals and parameters that
self-reflection is oriented by. Only the user edits it. This blocks file-tool
writes to it and Bash commands that pair it with a write form. A pattern
check can be evaded by a determined command; the real guarantee is the
chezmoi diff the user reviews. Also protects this guard itself.

Exit 2 blocks the call and shows stderr to Claude; every other outcome
(including any error in this script) allows it, so the guard never breaks
unrelated work.
"""
import json
import re
import sys

PROTECTED = ("charter.toml", "charter-guard.py")

WRITE_FORMS = re.compile(
    r"(>|\btee\b|\bsed\s+(-\w*\s+)*-\w*i|\bmv\b|\bcp\b|\brm\b|\btruncate\b|\bdd\b|\bln\b|\bchmod\b|"
    r"\binstall\b|\bpython3?\b|\bperl\b|\bruby\b|\bnode\b|\bchezmoi\s+(add|re-add|forget|edit|apply|destroy)\b)"
)

MESSAGE = (
    "Blocked: the charter (~/.config/mach/charter.toml) and its guard are Ivan's to change, not mine. "
    "Reading is fine. If I think it should change, I say so and he edits it."
)


def touches_protected(text):
    return any(p in text for p in PROTECTED)


def main():
    try:
        event = json.load(sys.stdin)
    except Exception:
        return 0
    tool = event.get("tool_name", "")
    inp = event.get("tool_input") or {}
    if tool in ("Edit", "Write", "NotebookEdit", "MultiEdit"):
        path = inp.get("file_path") or inp.get("notebook_path") or ""
        if touches_protected(path):
            print(MESSAGE, file=sys.stderr)
            return 2
        return 0
    if tool == "Bash":
        cmd = inp.get("command") or ""
        if not touches_protected(cmd):
            return 0
        # Discarding output is not a write.
        scrubbed = re.sub(r"\d?>\s*/dev/null|2>&1", "", cmd)
        if WRITE_FORMS.search(scrubbed):
            print(MESSAGE, file=sys.stderr)
            return 2
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except SystemExit:
        raise
    except Exception:
        sys.exit(0)
