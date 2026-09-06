-- matugen template -> ~/.config/hypr/hyprland/colors.lua
-- Ported from the old colors.conf template; colour roles and alphas are
-- unchanged so the generated theme matches what you had.
-- The hyprbars plugin block was dropped: hyprbars is not installed and the
-- Lua API exposes plugins via hl.plugin.<name>(...) instead of a plugin section.

hl.config({
    general = {
        col = {
            active_border   = "rgba(7d949dAA)",
            inactive_border = "rgba(344a51AA)",
        },
    },
    misc = {
        background_color = "rgba(091519FF)",
    },
})

hl.window_rule({
    match        = { pin = 1 },
    border_color = "rgba(67dbb0AA) rgba(67dbb077)",
})
