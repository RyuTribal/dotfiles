#!/usr/bin/env bash
# engrain-recall.sh — Claude Code UserPromptSubmit hook entry point.
#
# All logic lives in engrain-recall.py (one interpreter instead of the four
# python3 spawns + mktemp plumbing this script used to do itself — process
# startup dominated hook latency once the search step got cheap). This
# wrapper survives only because the hook config points here.
#
# Contract unchanged: never block a prompt. No python3, or a crashing
# script, degrades to silence and exit 0.

# A engrain-spawned `claude -p` (card, digest, judge) is not a user session:
# it must not be handed recalled memories it never asked for, because the
# model cannot tell them from the evidence it was told to work from, and a
# card that cites an evidence id while restating an injected one is a
# provenance lie. Same guard `engrain-checkpoint.sh` already carries.
[ "${ENGRAIN_DIGEST:-}" = "1" ] && exit 0
command -v python3 >/dev/null 2>&1 || exit 0
python3 "$(dirname "$0")/engrain-recall.py" 2>/dev/null
exit 0
