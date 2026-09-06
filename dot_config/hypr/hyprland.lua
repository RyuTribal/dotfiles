-- This file sources other files in `hyprland` and `custom` folders
-- You wanna add your stuff in files in `custom`
--
-- Converted from the hyprlang config by hyprlang2lua, then hand-corrected.
-- The old `.conf` files are kept alongside for reference; Hyprland ignores them
-- once hyprland.lua exists.
--
-- The `submap = global` wrapper from the hyprlang config is gone: it only existed
-- so `binditn ... catchall` would work. In Lua, catchall is a bind option
-- (see hyprland/keybinds.lua), so the submap is unnecessary.

-- Env vars must be real env vars, not Lua locals: the exec_cmd strings below
-- rely on the shell expanding $qsConfig (e.g. `qs -c $qsConfig`).
hl.env("qsConfig", "ii")

-- Workspace group helpers (replaces hyprland/scripts/workspace_action.sh)
require("hyprland.lib")

-- Defaults
require("hyprland.env")
require("hyprland.execs")
require("hyprland.general")
require("hyprland.rules")
require("hyprland.colors")
require("hyprland.keybinds")

-- Custom
require("custom.env")
require("custom.execs")
require("custom.general")
require("custom.rules")
require("custom.keybinds")

-- nwg-displays support
-- NOTE: nwg-displays writes hyprlang (.conf). Until it learns to emit Lua,
-- monitors below are declared here by hand and these two are placeholders.
require("workspaces")
require("monitors")

hl.monitor({
    output = "eDP-1",
    mode = "1920x1200@60.10",
    position = "4672x0",
    scale = "1.00",
    vrr = 1,
})

hl.monitor({
    output = "HDMI-A-1",
    mode = "1920x1080@60.00",
    position = "2752x0",
    scale = "1.00",
})

hl.monitor({
    output = "DP-6",
    mode = "1920x1080@60.00",
    position = "832x0",
    scale = "1.00",
})

-- hyprmon: managed monitor profile include
require("hyprmon")
