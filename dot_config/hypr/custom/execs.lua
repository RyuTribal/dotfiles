-- You can make apps auto-start here
-- Relevant Hyprland wiki section: https://wiki.hyprland.org/Configuring/Keywords/#executing

-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

hl.window_rule({
    match = {
        initial_class = "^(discord)$",
    },
    workspace = "5 silent",
})

hl.on("hyprland.start", function()
    hl.exec_cmd("discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime")
end)

