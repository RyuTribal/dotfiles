-- You can put custom rules here
-- Window/layer rules: https://wiki.hyprland.org/Configuring/Window-Rules/
-- Workspace rules: https://wiki.hyprland.org/Configuring/Workspace-Rules/

-- sweep disk inspector (Quickshell FloatingWindow) — float + center it
hl.window_rule({
    match = {
        title = "^(sweep)$",
    },
    float = true,
    center = true,
})
