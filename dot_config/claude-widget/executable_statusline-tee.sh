#!/usr/bin/env bash
# Claude Code statusline hook: persist the status JSON for desktop widgets,
# then delegate to the original statusline command so its behavior is unchanged.
mkdir -p ~/.cache/claude-widget
tee ~/.cache/claude-widget/status.json | exec bash /home/ryutribal/.claude/plugins/marketplaces/caveman/src/hooks/caveman-statusline.sh
