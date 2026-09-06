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
        title = ".*Welcome",
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
        title = ".*Shell conflicts.*",
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
-- kde-material-you-colors spawns a window when changing dark/light theme. This is to make sure it doesn't interfere at all.
hl.window_rule({
    match = {
        class = "^(plasma-changeicons)$",
    },
    float = true,
    no_initial_focus = true,
    move = "999999 999999",
})

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

-- No shadow for tiled windows (matches windows that are not floating).
hl.window_rule({
    match = {
        float = 0,
    },
    no_shadow = true,
})

-- ######## Workspace rules ########
hl.workspace_rule({
    workspace = "special:special",
    gaps_out = 30,
})

-- ######## Layer rules ########
-- Regenerated from rules.conf: hyprlang2lua dropped the rule bodies
-- and left the `match:namespace ` prefix inside the match value.

hl.layer_rule({ match = { namespace = ".*" }, xray = true })
hl.layer_rule({ match = { namespace = "^(walker)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(selection)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(overview)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(anyrun)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "indicator.*" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(osk)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(hyprpicker)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(noanim)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(gtk-layer-shell)$" }, blur = true })
hl.layer_rule({ match = { namespace = "^(gtk-layer-shell)$" }, ignore_alpha = 0.0 })
hl.layer_rule({ match = { namespace = "^(launcher)$" }, blur = true })
hl.layer_rule({ match = { namespace = "^(launcher)$" }, ignore_alpha = 0.5 })
hl.layer_rule({ match = { namespace = "^(notifications)$" }, blur = true })
hl.layer_rule({ match = { namespace = "^(notifications)$" }, ignore_alpha = 0.69 })
hl.layer_rule({ match = { namespace = "^(logout_dialog)$" }, blur = true })
hl.layer_rule({ match = { namespace = "sideleft.*" }, animation = "slide left" })
hl.layer_rule({ match = { namespace = "sideright.*" }, animation = "slide right" })
hl.layer_rule({ match = { namespace = "session[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "bar[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "bar[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "barcorner.*" }, blur = true })
hl.layer_rule({ match = { namespace = "barcorner.*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "dock[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "dock[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "indicator.*" }, blur = true })
hl.layer_rule({ match = { namespace = "indicator.*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "overview[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "overview[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "cheatsheet[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "cheatsheet[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "sideright[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "sideright[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "sideleft[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "sideleft[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "osk[0-9]*" }, blur = true })
hl.layer_rule({ match = { namespace = "osk[0-9]*" }, ignore_alpha = 0.6 })
hl.layer_rule({ match = { namespace = "quickshell:.*" }, blur_popups = true })
hl.layer_rule({ match = { namespace = "quickshell:.*" }, blur = true })
hl.layer_rule({ match = { namespace = "quickshell:.*" }, ignore_alpha = 0.79 })
hl.layer_rule({ match = { namespace = "quickshell:bar" }, animation = "slide" })
hl.layer_rule({ match = { namespace = "quickshell:verticalBar" }, animation = "slide" })
hl.layer_rule({ match = { namespace = "quickshell:screenCorners" }, animation = "fade" })
hl.layer_rule({ match = { namespace = "quickshell:sidebarRight" }, animation = "slide right" })
hl.layer_rule({ match = { namespace = "quickshell:wallpaperSelector" }, animation = "slide top" })
hl.layer_rule({ match = { namespace = "quickshell:osk" }, animation = "slide bottom" })
hl.layer_rule({ match = { namespace = "quickshell:dock" }, animation = "slide bottom" })
hl.layer_rule({ match = { namespace = "quickshell:cheatsheet" }, animation = "slide bottom" })
hl.layer_rule({ match = { namespace = "quickshell:session" }, blur = true })
hl.layer_rule({ match = { namespace = "quickshell:session" }, no_anim = true })
hl.layer_rule({ match = { namespace = "quickshell:session" }, ignore_alpha = 0.0 })
hl.layer_rule({ match = { namespace = "quickshell:notificationPopup" }, animation = "fade" })
hl.layer_rule({ match = { namespace = "quickshell:backgroundWidgets" }, blur = true })
hl.layer_rule({ match = { namespace = "quickshell:backgroundWidgets" }, ignore_alpha = 0.05 })
hl.layer_rule({ match = { namespace = "quickshell:screenshot" }, no_anim = true })
hl.layer_rule({ match = { namespace = "quickshell:screenCorners" }, animation = "popin 120%" })
hl.layer_rule({ match = { namespace = "quickshell:lockWindowPusher" }, no_anim = true })
hl.layer_rule({ match = { namespace = "quickshell:overview" }, no_anim = true })
hl.layer_rule({ match = { namespace = "gtk4-layer-shell" }, no_anim = true })
hl.layer_rule({ match = { namespace = "shell:bar" }, blur = true })
hl.layer_rule({ match = { namespace = "shell:bar" }, ignore_alpha = 0.0 })
hl.layer_rule({ match = { namespace = "shell:notifications" }, blur = true })
hl.layer_rule({ match = { namespace = "shell:notifications" }, ignore_alpha = 0.1 })
