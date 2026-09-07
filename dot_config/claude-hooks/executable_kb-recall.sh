#!/usr/bin/env bash
# kb-recall.sh — Claude Code UserPromptSubmit hook entry point.
#
# All logic lives in kb-recall.py (one interpreter instead of the four
# python3 spawns + mktemp plumbing this script used to do itself — process
# startup dominated hook latency once the search step got cheap). This
# wrapper survives only because the hook config points here.
#
# Contract unchanged: never block a prompt. No python3, or a crashing
# script, degrades to silence and exit 0.

command -v python3 >/dev/null 2>&1 || exit 0
python3 "$(dirname "$0")/kb-recall.py" 2>/dev/null
exit 0
