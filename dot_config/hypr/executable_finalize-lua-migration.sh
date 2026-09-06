#!/usr/bin/env bash
# Finish the hyprlang -> Lua migration. Run this AFTER logging out and back in,
# once Hyprland is actually running the Lua config.
#
# Why it is a separate step: the legacy .conf files, the three shell scripts and
# the matugen templates can only be correct for ONE format at a time. They were
# left in hyprlang form so the pre-logout session kept working. This flips them.
#
# Deleting hyprland.conf while Hyprland is running the LEGACY config makes it
# regenerate a 6-keybind stub and reload from it, which strips your keybinds
# mid-session. Hence the guard below.

set -euo pipefail

H="$HOME/.config/hypr"
STAGE="$HOME/.config/hypr/.migration"

# ---- guard: refuse to run unless the live compositor is on the Lua config ----
sig="${HYPRLAND_INSTANCE_SIGNATURE:-}"
if [[ -z "$sig" ]]; then
    echo "error: HYPRLAND_INSTANCE_SIGNATURE unset -- run this inside your Hyprland session." >&2
    exit 1
fi
log="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/hypr/$sig/hyprland.log"
if [[ ! -r "$log" ]]; then
    echo "error: cannot read $log" >&2
    exit 1
fi
if ! grep -q 'Using lua config' "$log"; then
    echo "error: this session is NOT running the Lua config." >&2
    echo "       Log out and back in first, then re-run this script." >&2
    grep -m1 -E '\[cfg\]' "$log" >&2 || true
    exit 1
fi
echo "confirmed: session is running the Lua config"

# ---- 1. scripts: hyprctl dispatch now takes Lua, and the workspace helper moved into Lua ----
if [[ -d "$STAGE/script-fixes" ]]; then
    install -m755 "$STAGE/script-fixes/fix_hyprlock.sh" "$H/hyprland/scripts/fix_hyprlock.sh"
    install -m755 "$STAGE/script-fixes/move_current_workspace_to_monitor.sh" \
        "$H/custom/scripts/move_current_workspace_to_monitor.sh"
    echo "patched 2 scripts to the Lua dispatch syntax"
else
    echo "warning: $STAGE/script-fixes is gone (scratchpad cleared)." >&2
    echo "         Patch these two by hand -- see the notes at the bottom." >&2
fi

# superseded by hyprland/lib/init.lua
rm -f "$H/hyprland/scripts/workspace_action.sh"
echo "removed workspace_action.sh (replaced by hyprland/lib/init.lua)"

# ---- 2. matugen: Hyprland's colours must be emitted as Lua; hyprlock's stay hyprlang ----
for cfg in "$HOME/.config/matugen/config.toml" \
           "$HOME/.config/quickshell/ii/scripts/colors/matugen.config.toml"; do
    [[ -f "$cfg" ]] || continue
    sed -i 's|templates/hyprland/colors\.conf|templates/hyprland/colors.lua|; s|hypr/hyprland/colors\.conf|hypr/hyprland/colors.lua|' "$cfg"
    echo "repointed $(basename "$(dirname "$cfg")")/$(basename "$cfg") -> colors.lua"
done

# ---- 3. drop the superseded hyprlang files (Hyprland already ignores them) ----
removed=0
for f in hyprland.conf monitors.conf workspaces.conf \
         hyprland/env.conf hyprland/execs.conf hyprland/general.conf hyprland/rules.conf \
         hyprland/colors.conf hyprland/keybinds.conf \
         custom/env.conf custom/execs.conf custom/general.conf custom/rules.conf custom/keybinds.conf; do
    if [[ -f "$H/$f" ]]; then
        rm -f "$H/$f"
        removed=$((removed + 1))
    fi
done
echo "removed $removed superseded .conf files"
echo "kept hyprlock.conf, hypridle.conf, hyprlock/colors.conf -- those programs have no Lua parser"

# ---- 4. re-theme so colors.lua exists from the template rather than the static copy ----
if command -v matugen >/dev/null 2>&1; then
    echo
    echo "note: run your wallpaper/theme switch once so matugen regenerates colors.lua"
    echo "      from the new template (a static copy is in place until then)."
fi

echo
echo "done. Reload with: hyprctl reload"
