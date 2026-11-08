#!/usr/bin/env bash
# Claude Code statusline hook: push the status JSON to axon, then run the original statusline.
json=$(cat)
axon call claude.push --timeout 2000 - <<<"{\"status\":$json}" >/dev/null 2>&1 &
exec bash "$HOME/.claude/plugins/marketplaces/caveman/src/hooks/caveman-statusline.sh" <<<"$json"
