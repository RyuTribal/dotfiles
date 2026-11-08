-- The cozy theme's Hyprland look. axon's loader runs this file last, after every keybind, with
-- only the look functions of hl. The curves, animations, decoration and look rules are today's
-- values, moved here from general.lua and rules.lua; the colours come from the palette, the
-- rounding and gaps from tokens.toml.

hl.curve("expressiveFastSpatial", { type = "bezier", points = { { 0.42, 1.67 }, { 0.21, 0.90 } } })
hl.curve("expressiveSlowSpatial", { type = "bezier", points = { { 0.39, 1.29 }, { 0.35, 0.98 } } })
hl.curve("expressiveDefaultSpatial", { type = "bezier", points = { { 0.38, 1.21 }, { 0.22, 1.00 } } })
hl.curve("emphasizedDecel", { type = "bezier", points = { { 0.05, 0.7 }, { 0.1, 1 } } })
hl.curve("emphasizedAccel", { type = "bezier", points = { { 0.3, 0 }, { 0.8, 0.15 } } })
hl.curve("standardDecel", { type = "bezier", points = { { 0, 0 }, { 0, 1 } } })
hl.curve("menu_decel", { type = "bezier", points = { { 0.1, 1 }, { 0, 1 } } })
hl.curve("menu_accel", { type = "bezier", points = { { 0.52, 0.03 }, { 0.72, 0.08 } } })
hl.animation({
    leaf = "windowsIn",
    enabled = true,
    speed = 3,
    bezier = "emphasizedDecel",
    style = "popin 80%",
})
hl.animation({
    leaf = "windowsOut",
    enabled = true,
    speed = 2,
    bezier = "emphasizedDecel",
    style = "popin 90%",
})
hl.animation({
    leaf = "windowsMove",
    enabled = true,
    speed = 3,
    bezier = "emphasizedDecel",
    style = "slide",
})
hl.animation({
    leaf = "border",
    enabled = true,
    speed = 10,
    bezier = "emphasizedDecel",
})
hl.animation({
    leaf = "layersIn",
    enabled = true,
    speed = 2.7,
    bezier = "emphasizedDecel",
    style = "popin 93%",
})
hl.animation({
    leaf = "layersOut",
    enabled = true,
    speed = 2.4,
    bezier = "menu_accel",
    style = "popin 94%",
})
hl.animation({
    leaf = "fadeLayersIn",
    enabled = true,
    speed = 0.5,
    bezier = "menu_decel",
})
hl.animation({
    leaf = "fadeLayersOut",
    enabled = true,
    speed = 2.7,
    bezier = "menu_accel",
})
hl.animation({
    leaf = "workspaces",
    enabled = true,
    speed = 7,
    bezier = "menu_decel",
    style = "slide",
})
hl.animation({
    leaf = "specialWorkspaceIn",
    enabled = true,
    speed = 2.8,
    bezier = "emphasizedDecel",
    style = "slidevert",
})
hl.animation({
    leaf = "specialWorkspaceOut",
    enabled = true,
    speed = 1.2,
    bezier = "emphasizedAccel",
    style = "slidevert",
})

hl.config({
    general = {
        gaps_in = {{ tokens.sizes.gaps_in }},
        gaps_out = {{ tokens.sizes.gaps_out }},
        gaps_workspaces = {{ tokens.sizes.gaps_workspaces }},
        border_size = {{ tokens.sizes.border }},
        col = {
            active_border = "{{ colors.outline | alpha(0.6667) | hypr }}",
            inactive_border = "{{ colors.outline_variant | alpha(0.6667) | hypr }}",
        },
        snap = {
            window_gap = {{ tokens.sizes.gaps_in }},
            monitor_gap = {{ tokens.sizes.gaps_out }},
        },
    },
    decoration = {
        rounding = {{ tokens.rounding.window }},
        blur = {
            enabled = true,
            xray = true,
            special = false,
            new_optimizations = true,
            size = 14,
            passes = 3,
            brightness = 1,
            noise = 0.04,
            contrast = 1,
            popups = true,
            popups_ignorealpha = 0.6,
            input_methods = true,
            input_methods_ignorealpha = 0.8,
        },
        shadow = {
            enabled = true,
            range = 30,
            offset = "0 2",
            render_power = 4,
            color = "rgba(00000010)",
        },
        -- Dim
        dim_inactive = true,
        dim_strength = 0.025,
        dim_special = 0.07,
    },
    animations = {
        enabled = true,
    },
    misc = {
        background_color = "{{ colors.surface | hypr }}",
    },
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
hl.layer_rule({ match = { namespace = ".*" }, xray = true })
hl.layer_rule({ match = { namespace = "^(walker)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(selection)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(overview)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "^(anyrun)$" }, no_anim = true })
hl.layer_rule({ match = { namespace = "indicator.*" }, no_anim = true })
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
hl.layer_rule({ match = { namespace = "quickshell:.*" }, blur_popups = true })
hl.layer_rule({ match = { namespace = "quickshell:.*" }, blur = true })
hl.layer_rule({ match = { namespace = "quickshell:.*" }, ignore_alpha = 0.79 })
hl.layer_rule({ match = { namespace = "quickshell:bar" }, animation = "slide" })
hl.layer_rule({ match = { namespace = "quickshell:verticalBar" }, animation = "slide" })
hl.layer_rule({ match = { namespace = "quickshell:screenCorners" }, animation = "fade" })
hl.layer_rule({ match = { namespace = "quickshell:sidebarRight" }, animation = "slide right" })
hl.layer_rule({ match = { namespace = "quickshell:wallpaperSelector" }, animation = "slide top" })
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

hl.window_rule({
    match        = { pin = 1 },
    border_color = "{{ colors.primary | alpha(0.6667) | hypr }} {{ colors.primary | alpha(0.4667) | hypr }}",
})
