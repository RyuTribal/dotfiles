#!/usr/bin/env bash
# Renders the first theme generation on a machine that has none. It does nothing where one exists.
set -euo pipefail
[ -e "$HOME/.local/state/axon/theme/current" ] && exit 0
"$HOME/.local/bin/axon" call theme.set id=cozy --wait --timeout 120000 || {
    echo "axon bootstrap: the first theme did not render; see axon get theme and journalctl --user -u axond -n 50" >&2
    exit 1
}
