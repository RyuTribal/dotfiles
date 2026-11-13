-- ######## Window rules ########

-- Uncomment to apply global transparency to all windows:
-- windowrule = opacity 0.89 override 0.89 override, match:class .*

-- Disable blur for xwayland context menus
-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

hl.window_rule({
    match = {
        class = "^()$",
        title = "^()$",
    },
    no_blur = true,
})

-- windowrule = no_blur 1, match:xwayland 1

-- Floating
hl.window_rule({
    match = {
        title = "^(Open File)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(Select a File)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(Choose wallpaper)(.*)$",
    },
    center = true,
    float = true,
    size = "60% 65%",
})

hl.window_rule({
    match = {
        title = "^(Open Folder)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(Save As)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(Library)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(File Upload)(.*)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(.*)(wants to save)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        title = "^(.*)(wants to open)$",
    },
    center = true,
    float = true,
})

hl.window_rule({
    match = {
        class = "^(blueberry\\.py)$",
    },
    float = true,
})

hl.window_rule({
    match = {
        class = "^(guifetch)$",
    },
    float = true,
})

hl.window_rule({
    match = {
        class = "^(pavucontrol)$",
    },
    float = true,
    size = "45% 45%",
    center = true,
})

hl.window_rule({
    match = {
        class = "^(org.pulseaudio.pavucontrol)$",
    },
    float = true,
    size = "45% 45%",
    center = true,
})

hl.window_rule({
    match = {
        class = "^(nm-connection-editor)$",
    },
    float = true,
    size = "45% 45%",
    center = true,
})

hl.window_rule({
    match = {
        class = ".*plasmawindowed.*",
    },
    float = true,
})

hl.window_rule({
    match = {
        class = "kcm_.*",
    },
    float = true,
})

hl.window_rule({
    match = {
        class = ".*bluedevilwizard",
    },
    float = true,
})

hl.window_rule({
    match = {
        title = "^(illogical-impulse Settings)$",
    },
    float = true,
})

hl.window_rule({
    match = {
        class = "org.freedesktop.impl.portal.desktop.kde",
    },
    float = true,
    size = "60% 65%",
})

hl.window_rule({
    match = {
        class = "^(Zotero)$",
    },
    float = true,
    size = "45% 45%",
})

-- Move
-- stupid dolphin copy
hl.window_rule({
    match = {
        title = "^(Copying — Dolphin)$",
    },
    move = "40 80",
})

-- Tiling
hl.window_rule({
    match = {
        class = "^dev\\.warp\\.Warp$",
    },
    float = false,
})

-- Picture-in-Picture
hl.window_rule({
    match = {
        title = "^([Pp]icture[-\\s]?[Ii]n[-\\s]?[Pp]icture)(.*)$",
    },
    float = true,
    keep_aspect_ratio = true,
    move = "73% 72%",
    size = "25% 25%",
    float = true,
    pin = true,
})

-- --- Tearing ---
hl.window_rule({
    match = {
        title = ".*\\.exe",
    },
    immediate = true,
})

hl.window_rule({
    match = {
        title = ".*minecraft.*",
    },
    immediate = true,
})

hl.window_rule({
    match = {
        class = "^(steam_app).*",
    },
    immediate = true,
})
