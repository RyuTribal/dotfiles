-- MONITOR CONFIG
-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

hl.monitor({
    output = "",
    mode = "preferred",
    position = "auto",
    scale = "1",
})

-- monitor=,addreserved, 0, 0, 0, 0 # Custom reserved area

-- HDMI port: mirror display. To see device name, use `hyprctl monitors`
-- monitor=HDMI-A-1,1920x1080@60,1920x0,1,mirror,eDP-1

hl.gesture({
    fingers = 3,
    direction = "swipe",
    action = "move",
})
hl.gesture({
    fingers = 4,
    direction = "horizontal",
    action = "workspace",
})
hl.gesture({
    fingers = 4,
    direction = "pinch",
    action = "float",
})
hl.gesture({
    fingers = 4,
    direction = "up",
    action = function() hl.dispatch(hl.dsp.global("quickshell:overviewToggle")) end,
})
hl.gesture({
    fingers = 4,
    direction = "down",
    action = function() hl.dispatch(hl.dsp.global("quickshell:overviewClose")) end,
})

-- hyprexpo/hyprbars plugin section dropped: neither plugin is installed.
-- Lua exposes plugins via hl.plugin.<name>(...) if you ever add them.
--[[
  hyprexpo { ... }
]]

hl.config({
    gestures = {
        workspace_swipe_distance = 700,
        workspace_swipe_cancel_ratio = 0.2,
        workspace_swipe_min_speed_to_force = 5,
        workspace_swipe_direction_lock = true,
        workspace_swipe_direction_lock_threshold = 10,
        workspace_swipe_create_new = true,
    },
    general = {
        resize_on_border = true,
        no_focus_fallback = true,
        allow_tearing = true, -- This just allows the `immediate` window rule to work
        snap = {
            enabled = true,
            respect_gaps = true,
        },
    },
    dwindle = {
        preserve_split = true,
        smart_split = false,
        smart_resizing = false,
        -- precise_mouse_move = true
    },
    input = {
        kb_layout = "us",
        numlock_by_default = true,
        repeat_delay = 250,
        repeat_rate = 35,
        follow_mouse = 1,
        off_window_axis_events = 2,
        touchpad = {
            natural_scroll = true,
            disable_while_typing = true,
            clickfinger_behavior = true,
            scroll_factor = 0.5,
        },
    },
    misc = {
        disable_hyprland_logo = true,
        disable_splash_rendering = true,
        vrr = 1,
        mouse_move_enables_dpms = true,
        key_press_enables_dpms = true,
        animate_manual_resizes = false,
        animate_mouse_windowdragging = false,
        enable_swallow = false,
        swallow_regex = "(foot|kitty|allacritty|Alacritty)",
        on_focus_under_fullscreen = 2,
        allow_session_lock_restore = true,
        session_lock_xray = true,
        initial_workspace_tracking = 1,
        focus_on_activate = true,
    },
    binds = {
        scroll_event_delay = 0,
        hide_special_on_workspace_change = true,
    },
    cursor = {
        zoom_factor = 1,
        zoom_rigid = false,
        hotspot_padding = 1,
    },
    -- Overview
})

