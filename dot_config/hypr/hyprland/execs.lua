-- Bar, wallpaper
-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

-- Input method

-- Core components (authentication, lock screen, notification daemon)

-- Audio

-- Clipboard: history
-- exec-once = wl-paste --watch cliphist store &

-- Cursor

hl.on("hyprland.start", function()
    hl.exec_cmd("~/.config/hypr/hyprland/scripts/start_geoclue_agent.sh")
    hl.exec_cmd("qs -c $qsConfig &")
    hl.exec_cmd("fcitx5")
    hl.exec_cmd("gnome-keyring-daemon --start --components=secrets")
    hl.exec_cmd("/usr/lib/polkit-kde-authentication-agent-1 || /usr/libexec/polkit-kde-authentication-agent-1  || /usr/lib/polkit-gnome/polkit-gnome-authentication-agent-1 || /usr/libexec/polkit-gnome-authentication-agent-1")
    hl.exec_cmd("hypridle")
    hl.exec_cmd("dbus-update-activation-environment --all")
    hl.exec_cmd("sleep 1 && dbus-update-activation-environment --systemd WAYLAND_DISPLAY XDG_CURRENT_DESKTOP")
    hl.exec_cmd("hyprpm reload")
    hl.exec_cmd("easyeffects --gapplication-service")
    hl.exec_cmd("wl-paste --type text --watch bash -c 'cliphist store && qs -c $qsConfig ipc call cliphistService update'")
    hl.exec_cmd("wl-paste --type image --watch bash -c 'cliphist store && qs -c $qsConfig ipc call cliphistService update'")
    hl.exec_cmd("hyprctl setcursor Bibata-Modern-Classic 24")
end)

