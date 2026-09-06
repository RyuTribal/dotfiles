-- See https://wiki.hyprland.org/Configuring/Binds/
--!
--#! User
-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

hl.bind("CTRL+SUPER + Slash", hl.dsp.exec_cmd("xdg-open ~/.config/illogical-impulse/config.json")) -- Edit shell config
hl.bind("CTRL+SUPER+ALT + Slash", hl.dsp.exec_cmd("xdg-open ~/.config/hypr/custom/keybinds.lua")) -- Edit extra keybinds
hl.bind("CTRL+SUPER + C", hl.dsp.exec_cmd("~/.config/hypr/custom/scripts/launch_private_fish.sh"), { description = "Private fish shell" }) -- Private fish shell (no history)
-- was `unbind = Super, J`: drop the default bar-toggle bind before rebinding
hl.unbind("SUPER + J")
hl.bind("SUPER + J", hl.dsp.layout("togglesplit")) -- Toggle split orientation

-- Add stuff here
-- Use #! to add an extra column on the cheatsheet
-- Use ##! to add a section in that column
-- Add a comment after a bind to add a description, like above

hl.bind("SUPER+SHIFT + D", hl.dsp.exec_cmd("~/.config/hypr/custom/scripts/brave-debug-trace.sh"), { description = "Brave debug startup trace" }) -- Brave trace startup
hl.bind("CTRL+SUPER+SHIFT + BracketRight", hl.dsp.exec_cmd("~/.config/hypr/custom/scripts/move_current_workspace_to_monitor.sh next"), { description = "Move workspace to next monitor" }) -- Move current workspace to next monitor
hl.bind("CTRL+SUPER+SHIFT + BracketLeft", hl.dsp.exec_cmd("~/.config/hypr/custom/scripts/move_current_workspace_to_monitor.sh prev"), { description = "Move workspace to previous monitor" }) -- Move current workspace to previous monitor

hl.bind("CTRL+SUPER+SHIFT + C", hl.dsp.exec_cmd("kitty \"python3\""), { description = "open python" })
