-- matugen template -> ~/.config/hypr/hyprland/colors.lua
-- Ported from the old colors.conf template; colour roles and alphas are
-- unchanged so the generated theme matches what you had.
-- The hyprbars plugin block was dropped: hyprbars is not installed and the
-- Lua API exposes plugins via hl.plugin.<name>(...) instead of a plugin section.

hl.config({
    general = {
        col = {
            active_border   = "rgba({{colors.outline.default.hex_stripped}}AA)",
            inactive_border = "rgba({{colors.outline_variant.default.hex_stripped}}AA)",
        },
    },
    misc = {
        background_color = "rgba({{colors.surface.dark.hex_stripped}}FF)",
    },
})

hl.window_rule({
    match        = { pin = 1 },
    border_color = "rgba({{colors.primary.default.hex_stripped}}AA) rgba({{colors.primary.default.hex_stripped}}77)",
})
