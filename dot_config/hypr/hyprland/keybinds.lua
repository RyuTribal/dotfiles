-- Lines ending with `# [hidden]` won't be shown on cheatsheet
-- Lines starting with #! are section headings

--!
--#! Shell
-- These absolutely need to be on top, or they won't work consistently
-- Converted from hyprlang by hyprlang2lua, then hand-corrected.

hl.bind("SUPER + Space", hl.dsp.global("quickshell:overviewToggle"), { description = "Toggle overview" }) -- Toggle overview/launcher
-- bindid = Super, Super_R, Toggle overview, global, quickshell:overviewToggleRelease # [hidden] Toggle overview/launcher
hl.bind("SUPER + Space", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || pkill fuzzel || fuzzel")) -- [hidden] Launcher (fallback)
-- bind = Super, Super_R, exec, qs -c $qsConfig ipc call TEST_ALIVE || pkill fuzzel || fuzzel # [hidden] Launcher (fallback)
-- was `binditn = Super, catchall`: catchall is a bind option in Lua, not a keysym
hl.bind("SUPER", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt"), { catchall = true, non_consuming = true, transparent = true, ignore_mods = true }) -- [hidden]
hl.bind("CTRL + Super_L", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("CTRL + Super_R", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:272", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:273", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:274", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:275", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:276", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse:277", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse_up", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]
hl.bind("SUPER + mouse_down", hl.dsp.global("quickshell:overviewToggleReleaseInterrupt")) -- [hidden]

hl.bind("Super_L", hl.dsp.global("quickshell:workspaceNumber"), { transparent = true, ignore_mods = true }) -- [hidden]
hl.bind("Super_R", hl.dsp.global("quickshell:workspaceNumber"), { transparent = true, ignore_mods = true }) -- [hidden]
hl.bind("SUPER + V", hl.dsp.global("quickshell:overviewClipboardToggle"), { description = "Clipboard history >> clipboard" }) -- Clipboard history >> clipboard
hl.bind("SUPER + Period", hl.dsp.global("quickshell:overviewEmojiToggle"), { description = "Emoji >> clipboard" }) -- Emoji >> clipboard
hl.bind("SUPER + Comma", hl.dsp.global("quickshell:overviewGlyphToggle"), { description = "Emoji >> clipboard" }) -- NerdFont >> clipboard
hl.bind("SUPER + N", hl.dsp.global("quickshell:sidebarRightToggle"), { description = "Toggle right sidebar" }) -- Toggle right sidebar
hl.bind("SUPER + A", hl.dsp.global("quickshell:topMenuToggle"), { description = "Toggle top menu" }) -- Toggle top menu
hl.bind("SUPER + H", hl.dsp.global("quickshell:cheatsheetToggle"), { description = "Toggle cheatsheet" }) -- Toggle cheatsheet
hl.bind("SUPER + K", hl.dsp.global("quickshell:oskToggle"), { description = "Toggle on-screen keyboard" }) -- Toggle on-screen keyboard
hl.bind("SUPER + P", hl.dsp.global("quickshell:mediaControlsToggle"), { description = "Toggle media controls" }) -- Toggle media controls
hl.bind("CTRL+ALT + Delete", hl.dsp.global("quickshell:sessionToggle"), { description = "Toggle session menu" }) -- Toggle session menu
hl.bind("SUPER+SHIFT + B", hl.dsp.global("quickshell:barToggle"), { description = "Toggle bar" }) -- Toggle bar
hl.bind("CTRL+ALT + Delete", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || pkill wlogout || wlogout -p layer-shell")) -- [hidden] Session menu (fallback)
hl.bind("SHIFT+SUPER+ALT + Slash", hl.dsp.exec_cmd("qs -p ~/.config/quickshell/$qsConfig/welcome.qml")) -- [hidden] Launch welcome app

hl.bind("XF86MonBrightnessUp", hl.dsp.exec_cmd("qs -c $qsConfig ipc call brightness increment || brightnessctl s 5%+"), { locked = true, repeating = true }) -- [hidden]
hl.bind("XF86MonBrightnessDown", hl.dsp.exec_cmd("qs -c $qsConfig ipc call brightness decrement || brightnessctl s 5%-"), { locked = true, repeating = true }) -- [hidden]
hl.bind("XF86AudioRaiseVolume", hl.dsp.exec_cmd("wpctl set-volume @DEFAULT_AUDIO_SINK@ 2%+"), { locked = true, repeating = true }) -- [hidden]
hl.bind("XF86AudioLowerVolume", hl.dsp.exec_cmd("wpctl set-volume @DEFAULT_AUDIO_SINK@ 2%-"), { locked = true, repeating = true }) -- [hidden]

hl.bind("XF86AudioMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_SINK@ toggle"), { locked = true }) -- [hidden]
hl.bind("SUPER+SHIFT + M", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_SINK@ toggle"), { locked = true, description = "Toggle mute" }) -- [hidden]
hl.bind("ALT + XF86AudioMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_SOURCE@ toggle"), { locked = true }) -- [hidden]
hl.bind("XF86AudioMicMute", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_SOURCE@ toggle"), { locked = true }) -- [hidden]
hl.bind("SUPER+ALT + M", hl.dsp.exec_cmd("wpctl set-mute @DEFAULT_SOURCE@ toggle"), { locked = true, description = "Toggle mic" }) -- [hidden]
hl.bind("CTRL+SUPER + T", hl.dsp.global("quickshell:wallpaperSelectorToggle"), { description = "Toggle wallpaper selector" }) -- Wallpaper selector
hl.bind("CTRL+SUPER + T", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || ~/.config/quickshell/$qsConfig/scripts/colors/switchwall.sh"), { description = "Change wallpaper" }) -- [hidden] Change wallpaper (fallback)
hl.bind("CTRL+SUPER + R", hl.dsp.exec_cmd("killall ags agsv1 gjs ydotool qs quickshell; qs -c $qsConfig &")) -- Restart widgets

--#! Utilities
-- Screenshot, Record, OCR, Color picker, Clipboard history
hl.bind("SUPER + V", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || pkill fuzzel || cliphist list | fuzzel --match-mode fzf --dmenu | cliphist decode | wl-copy"), { description = "Copy clipboard history entry" }) -- [hidden] Clipboard history >> clipboard (fallback)
hl.bind("SUPER + Period", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || pkill fuzzel || ~/.config/hypr/hyprland/scripts/fuzzel-emoji.sh copy"), { description = "Copy an emoji" }) -- [hidden] Emoji >> clipboard (fallback)
hl.bind("SUPER+SHIFT + S", hl.dsp.exec_cmd("qs -p ~/.config/quickshell/$qsConfig/screenshot.qml || pidof slurp || hyprshot --freeze --clipboard-only --mode region --silent"), { description = "Screen snip" }) -- Screen snip
-- OCR
hl.bind("SUPER+SHIFT + T", hl.dsp.exec_cmd("grim -g \"$(slurp $SLURP_ARGS)\" \"tmp.png\" && tesseract \"tmp.png\" - | wl-copy && rm \"tmp.png\""), { description = "Character recognition" }) -- [hidden]
-- Color picker
hl.bind("SUPER+SHIFT + C", hl.dsp.exec_cmd("hyprpicker -a"), { description = "Color picker" }) -- Pick color (Hex) >> clipboard
-- Fullscreen screenshot
hl.bind("Print", hl.dsp.exec_cmd("grim - | wl-copy"), { locked = true, description = "Screenshot >> clipboard" }) -- Screenshot >> clipboard
hl.bind("CTRL + Print", hl.dsp.exec_cmd("mkdir -p $(xdg-user-dir PICTURES)/Screenshots && grim $(xdg-user-dir PICTURES)/Screenshots/Screenshot_\"$(date '+%Y-%m-%d_%H.%M.%S')\".png"), { locked = true, description = "Screenshot >> clipboard & save" }) -- Screenshot >> clipboard & file
-- Recording stuff
hl.bind("SUPER+ALT + R", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/record.sh"), { description = "Record region (no sound)" }) -- Record region (no sound)
hl.bind("CTRL+ALT + R", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/record.sh --fullscreen"), { description = "Record screen (no sound)" }) -- [hidden] Record screen (no sound)
hl.bind("SUPER+SHIFT+ALT + R", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/record.sh --fullscreen-sound"), { description = "Record screen (with sound)" }) -- Record screen (with sound)
-- AI
-- bindd = Super+Shift+Alt, mouse:273, Generate AI summary for selected text, exec, ~/.config/hypr/hyprland/scripts/ai/primary-buffer-query.sh # AI summary for selected text
hl.bind("SUPER + M", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/new_md_note.sh"), { description = "Create or open a daily note" })

-- Hyprland
hl.bind("SUPER+SHIFT + E", hl.dsp.exec_cmd("kitty -1 -e fish -ic 'nvim ~/.config/hypr/hyprland/keybinds.lua'"), { description = "Edit hyprland binds" })
hl.bind("SUPER+SHIFT + N", hl.dsp.exec_cmd("kitty -1 -e fish -ic 'nvim ~/.config/nvim/init.lua'"), { description = "Edit Nvim" }) -- Edit nvim

--!
--#! Window
-- Focusing
hl.bind("SUPER + mouse:272", hl.dsp.window.drag()) -- Move
hl.bind("SUPER + mouse:274", hl.dsp.window.drag()) -- [hidden]
hl.bind("SUPER + mouse:273", hl.dsp.window.resize()) -- Resize
--/# bind = Super, ←/↑/→/↓,, # Focus in direction
hl.bind("SUPER + Left", hl.dsp.focus({ direction = "left" })) -- [hidden]
hl.bind("SUPER + Right", hl.dsp.focus({ direction = "right" })) -- [hidden]
hl.bind("SUPER + Up", hl.dsp.focus({ direction = "up" })) -- [hidden]
hl.bind("SUPER + Down", hl.dsp.focus({ direction = "down" })) -- [hidden]
hl.bind("SUPER + BracketLeft", hl.dsp.focus({ direction = "left" })) -- [hidden]
hl.bind("SUPER + BracketRight", hl.dsp.focus({ direction = "right" })) -- [hidden]
hl.bind("SUPER + J", hl.dsp.layout("togglesplit"))
--/# bind = Super+Shift, ←/↑/→/↓,, # Move in direction
hl.bind("SUPER+SHIFT + Left", hl.dsp.window.move({ direction = "l" })) -- [hidden]
hl.bind("SUPER+SHIFT + Right", hl.dsp.window.move({ direction = "r" })) -- [hidden]
hl.bind("SUPER+SHIFT + Up", hl.dsp.window.move({ direction = "u" })) -- [hidden]
hl.bind("SUPER+SHIFT + Down", hl.dsp.window.move({ direction = "d" })) -- [hidden]
hl.bind("ALT + F4", hl.dsp.window.close()) -- [hidden] Close (Windows)
hl.bind("SUPER + X", hl.dsp.window.close()) -- Close
hl.bind("SUPER+SHIFT+ALT + X", hl.dsp.exec_cmd("hyprctl kill")) -- Forcefully zap a window

-- Window split ratio
--/# binde = Super, ;/',, # Adjust split ratio
hl.bind("SUPER + Semicolon", hl.dsp.layout("splitratio -0.1"), { repeating = true }) -- [hidden]
hl.bind("SUPER + Apostrophe", hl.dsp.layout("splitratio +0.1"), { repeating = true }) -- [hidden]
-- Positioning mode
hl.bind("SUPER+ALT + Space", hl.dsp.window.float({ action = "toggle" })) -- Float/Tile
hl.bind("SUPER + D", hl.dsp.window.fullscreen({ mode = "maximized", action = "toggle" })) -- Maximize
hl.bind("SUPER + F", hl.dsp.window.fullscreen({ mode = "fullscreen", action = "toggle" })) -- Fullscreen
hl.bind("SUPER+ALT + F", hl.dsp.window.fullscreen_state({ internal = 0, client = 3, action = "toggle" })) -- Fullscreen spoof
hl.bind("SUPER+SHIFT + P", hl.dsp.window.pin()) -- Pin

--/# bind = Super+Shift, Hash,, # Send to workspace # (1, 2, 3,...)
hl.bind("SUPER+SHIFT + 1", move_to_workspace_in_group(1)) -- [hidden]
hl.bind("SUPER+SHIFT + 2", move_to_workspace_in_group(2)) -- [hidden]
hl.bind("SUPER+SHIFT + 3", move_to_workspace_in_group(3)) -- [hidden]
hl.bind("SUPER+SHIFT + 4", move_to_workspace_in_group(4)) -- [hidden]
hl.bind("SUPER+SHIFT + 5", move_to_workspace_in_group(5)) -- [hidden]
hl.bind("SUPER+SHIFT + 6", move_to_workspace_in_group(6)) -- [hidden]
hl.bind("SUPER+SHIFT + 7", move_to_workspace_in_group(7)) -- [hidden]
hl.bind("SUPER+SHIFT + 8", move_to_workspace_in_group(8)) -- [hidden]
hl.bind("SUPER+SHIFT + 9", move_to_workspace_in_group(9)) -- [hidden]
hl.bind("SUPER+SHIFT + 0", move_to_workspace_in_group(10)) -- [hidden]

-- #/# bind = Super+Shift, Scroll ↑/↓,, # Send to workspace left/right
hl.bind("SUPER+SHIFT + mouse_down", hl.dsp.window.move({ workspace = "r-1" })) -- [hidden]
hl.bind("SUPER+SHIFT + mouse_up", hl.dsp.window.move({ workspace = "r+1" })) -- [hidden]
hl.bind("SUPER+ALT + mouse_down", hl.dsp.window.move({ workspace = -1 })) -- [hidden]
hl.bind("SUPER+ALT + mouse_up", hl.dsp.window.move({ workspace = "+1" })) -- [hidden]

--/# bind = Super+Shift, Page_↑/↓,, # Send to workspace left/right
hl.bind("SUPER+ALT + Page_Down", hl.dsp.window.move({ workspace = "+1" })) -- [hidden]
hl.bind("SUPER+ALT + Page_Up", hl.dsp.window.move({ workspace = -1 })) -- [hidden]
hl.bind("SUPER+SHIFT + Page_Down", hl.dsp.window.move({ workspace = "r+1" })) -- [hidden]
hl.bind("SUPER+SHIFT + Page_Up", hl.dsp.window.move({ workspace = "r-1" })) -- [hidden]
hl.bind("CTRL+SUPER+SHIFT + Right", hl.dsp.window.move({ workspace = "r+1" })) -- [hidden]
hl.bind("CTRL+SUPER+SHIFT + Left", hl.dsp.window.move({ workspace = "r-1" })) -- [hidden]

hl.bind("SUPER+ALT + S", hl.dsp.window.move({ workspace = "special", follow = false })) -- Send to scratchpad

hl.bind("CTRL+SUPER + S", hl.dsp.workspace.toggle_special("")) -- [hidden]
hl.bind("ALT + Tab", hl.dsp.window.cycle_next({ next = true })) -- [hidden] sus keybind
hl.bind("ALT + Tab", hl.dsp.window.bring_to_top()) -- [hidden] bring it to the top

--#! Workspace
-- Switching
--/# bind = Super, Hash,, # Focus workspace # (1, 2, 3,...)
hl.bind("SUPER + 1", focus_workspace_in_group(1)) -- [hidden]
hl.bind("SUPER + 2", focus_workspace_in_group(2)) -- [hidden]
hl.bind("SUPER + 3", focus_workspace_in_group(3)) -- [hidden]
hl.bind("SUPER + 4", focus_workspace_in_group(4)) -- [hidden]
hl.bind("SUPER + 5", focus_workspace_in_group(5)) -- [hidden]
hl.bind("SUPER + 6", focus_workspace_in_group(6)) -- [hidden]
hl.bind("SUPER + 7", focus_workspace_in_group(7)) -- [hidden]
hl.bind("SUPER + 8", focus_workspace_in_group(8)) -- [hidden]
hl.bind("SUPER + 9", focus_workspace_in_group(9)) -- [hidden]
hl.bind("SUPER + 0", focus_workspace_in_group(10)) -- [hidden]

-- #/# Same but with keypad
hl.bind("SUPER + KP_End", focus_workspace_in_group(1)) -- [hidden]
hl.bind("SUPER + KP_Down", focus_workspace_in_group(2)) -- [hidden]
hl.bind("SUPER + KP_Next", focus_workspace_in_group(3)) -- [hidden]
hl.bind("SUPER + KP_Left", focus_workspace_in_group(4)) -- [hidden]
hl.bind("SUPER + KP_Begin", focus_workspace_in_group(5)) -- [hidden]
hl.bind("SUPER + KP_Right", focus_workspace_in_group(6)) -- [hidden]
hl.bind("SUPER + KP_Home", focus_workspace_in_group(7)) -- [hidden]
hl.bind("SUPER + KP_Up", focus_workspace_in_group(8)) -- [hidden]
hl.bind("SUPER + KP_Prior", focus_workspace_in_group(9)) -- [hidden]
hl.bind("SUPER + KP_Insert", focus_workspace_in_group(10)) -- [hidden]

--/# bind = Ctrl+Super, ←/→,, # Focus left/right
hl.bind("CTRL+SUPER + Right", hl.dsp.focus({ workspace = "r+1" })) -- [hidden]
hl.bind("CTRL+SUPER + Left", hl.dsp.focus({ workspace = "r-1" })) -- [hidden]
--/# bind = Ctrl+Super+Alt, ←/→,, # [hidden] Focus busy left/right
hl.bind("CTRL+SUPER+ALT + Right", hl.dsp.focus({ workspace = "m+1" })) -- [hidden]
hl.bind("CTRL+SUPER+ALT + Left", hl.dsp.focus({ workspace = "m-1" })) -- [hidden]
--/# bind = Super, Page_↑/↓,, # Focus left/right
hl.bind("SUPER + Page_Down", hl.dsp.focus({ workspace = "+1" })) -- [hidden]
hl.bind("SUPER + Page_Up", hl.dsp.focus({ workspace = -1 })) -- [hidden]
hl.bind("CTRL+SUPER + Page_Down", hl.dsp.focus({ workspace = "r+1" })) -- [hidden]
hl.bind("CTRL+SUPER + Page_Up", hl.dsp.focus({ workspace = "r-1" })) -- [hidden]
--/# bind = Super, Scroll ↑/↓,, # Focus left/right
hl.bind("SUPER + mouse_up", hl.dsp.focus({ workspace = "+1" })) -- [hidden]
hl.bind("SUPER + mouse_down", hl.dsp.focus({ workspace = -1 })) -- [hidden]
hl.bind("CTRL+SUPER + mouse_up", hl.dsp.focus({ workspace = "r+1" })) -- [hidden]
hl.bind("CTRL+SUPER + mouse_down", hl.dsp.focus({ workspace = "r-1" })) -- [hidden]
--# Special
hl.bind("SUPER + S", hl.dsp.workspace.toggle_special("")) -- Toggle scratchpad
hl.bind("SUPER + mouse:275", hl.dsp.workspace.toggle_special("")) -- [hidden]
hl.bind("CTRL+SUPER + BracketLeft", hl.dsp.focus({ workspace = -1 })) -- [hidden]
hl.bind("CTRL+SUPER + BracketRight", hl.dsp.focus({ workspace = "+1" })) -- [hidden]
hl.bind("CTRL+SUPER + Up", hl.dsp.focus({ workspace = "r-5" })) -- [hidden]
hl.bind("CTRL+SUPER + Down", hl.dsp.focus({ workspace = "r+5" })) -- [hidden]

--!
-- Testing
hl.bind("SUPER+ALT + f11", hl.dsp.exec_cmd("bash -c 'RANDOM_IMAGE=$(find ~/Pictures -type f | grep -v -i \"nipple\" | grep -v -i \"pussy\" | shuf -n 1); ACTION=$(notify-send \"Test notification with body image\" \"This notification should contain your user account <b>image</b> and <a href=\\\"https://discord.com/app\\\">Discord</a> <b>icon</b>. Oh and here is a random image in your Pictures folder: <img src=\\\"$RANDOM_IMAGE\\\" alt=\\\"Testing image\\\"/>\" -a \"Hyprland keybind\" -p -h \"string:image-path:/var/lib/AccountsService/icons/$USER\" -t 6000 -i \"discord\" -A \"openImage=Open profile image\" -A \"action2=Open the random image\" -A \"action3=Useless button\"); [[ $ACTION == *openImage ]] && xdg-open \"/var/lib/AccountsService/icons/$USER\"; [[ $ACTION == *action2 ]] && xdg-open \\\"$RANDOM_IMAGE\\\"'")) -- [hidden]
hl.bind("SUPER+ALT + f12", hl.dsp.exec_cmd("bash -c 'RANDOM_IMAGE=$(find ~/Pictures -type f | grep -v -i \"nipple\" | grep -v -i \"pussy\" | shuf -n 1); ACTION=$(notify-send \"Test notification\" \"This notification should contain a random image in your <b>Pictures</b> folder and <a href=\\\"https://discord.com/app\\\">Discord</a> <b>icon</b>.\\n<i>Flick right to dismiss!</i>\" -a \"Discord (fake)\" -p -h \"string:image-path:$RANDOM_IMAGE\" -t 6000 -i \"discord\" -A \"openImage=Open profile image\" -A \"action2=Useless button\" -A \"action3=Cry more\"); [[ $ACTION == *openImage ]] && xdg-open \"/var/lib/AccountsService/icons/$USER\"'")) -- [hidden]
hl.bind("SUPER+ALT + Equal", hl.dsp.exec_cmd("notify-send \"Urgent notification\" \"Ah hell no\" -u critical -a 'Hyprland keybind'")) -- [hidden]

--#! Session
-- Dropped a leading `hyprctl dispatch -- inhibit-logind-lock &&`: that dispatcher
-- does not exist in this Hyprland (it already returned "Invalid dispatcher"
-- before the Lua migration), so it was a no-op that happened to exit 0. Under a
-- Lua config the args are parsed as Lua, so relying on that exit code is unsafe.
hl.bind("SUPER + L", hl.dsp.exec_cmd("loginctl lock-session"), { description = "Lock" }) -- Lock
hl.bind("SUPER+SHIFT + L", hl.dsp.exec_cmd("systemctl suspend || loginctl suspend"), { locked = true, description = "Suspend system" }) -- Sleep
-- bindl=,switch:on:Lid Switch, exec, systemctl suspend || loginctl suspend # [hidden] Suspend when laptop lid is closed, uncomment if for whatever reason it's not the default behavior
hl.bind("CTRL+SHIFT+ALT+SUPER + Delete", hl.dsp.exec_cmd("systemctl poweroff || loginctl poweroff"), { description = "Shutdown" }) -- [hidden] Power off

--#! Screen
-- Zoom
hl.bind("SUPER + Minus", hl.dsp.exec_cmd("qs -c $qsConfig ipc call zoom zoomOut"), { repeating = true }) -- Zoom out
hl.bind("SUPER + Equal", hl.dsp.exec_cmd("qs -c $qsConfig ipc call zoom zoomIn"), { repeating = true }) -- Zoom in
hl.bind("SUPER + Minus", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || ~/.config/hypr/hyprland/scripts/zoom.sh decrease 0.1"), { repeating = true }) -- [hidden] Zoom out
hl.bind("SUPER + Equal", hl.dsp.exec_cmd("qs -c $qsConfig ipc call TEST_ALIVE || ~/.config/hypr/hyprland/scripts/zoom.sh increase 0.1"), { repeating = true }) -- [hidden] Zoom in

--#! Media
hl.bind("SUPER+ALT + N", hl.dsp.exec_cmd("playerctl next || playerctl position `bc <<< \"100 * $(playerctl metadata mpris:length) / 1000000 / 100\"`"), { locked = true }) -- Next track
hl.bind("XF86AudioNext", hl.dsp.exec_cmd("playerctl next || playerctl position `bc <<< \"100 * $(playerctl metadata mpris:length) / 1000000 / 100\"`"), { locked = true }) -- [hidden]
hl.bind("XF86AudioPrev", hl.dsp.exec_cmd("playerctl previous"), { locked = true }) -- [hidden]
hl.bind("SUPER+SHIFT+ALT + mouse:275", hl.dsp.exec_cmd("playerctl previous")) -- [hidden]
hl.bind("SUPER+SHIFT+ALT + mouse:276", hl.dsp.exec_cmd("playerctl next || playerctl position `bc <<< \"100 * $(playerctl metadata mpris:length) / 1000000 / 100\"`")) -- [hidden]
hl.bind("SUPER+ALT + B", hl.dsp.exec_cmd("playerctl previous"), { locked = true }) -- Previous track
hl.bind("SUPER+ALT + P", hl.dsp.exec_cmd("playerctl play-pause"), { locked = true }) -- Play/pause media
hl.bind("XF86AudioPlay", hl.dsp.exec_cmd("playerctl play-pause"), { locked = true }) -- [hidden]
hl.bind("XF86AudioPause", hl.dsp.exec_cmd("playerctl play-pause"), { locked = true }) -- [hidden]

--#! Apps
hl.bind("SUPER + C", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"${TERMINAL}\" \"kitty -1\" \"foot\" \"alacritty\" \"wezterm\" \"konsole\" \"kgx\" \"uxterm\" \"xterm\"")) -- Terminal
hl.bind("SUPER + T", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh  \"${TERMINAL}\" \"kitty -1\" \"foot\" \"alacritty\" \"wezterm\" \"konsole\" \"kgx\" \"uxterm\" \"xterm\"")) -- [hidden] (terminal) (alt)
hl.bind("CTRL+ALT + T", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"${TERMINAL}\" \"kitty -1\" \"foot\" \"alacritty\" \"wezterm\" \"konsole\" \"kgx\" \"uxterm\" \"xterm\"")) -- [hidden] (terminal) (for Ubuntu people)
hl.bind("SUPER + E", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"dolphin\" \"nautilus\" \"nemo\" \"thunar\" \"${TERMINAL}\" \"kitty -1 fish -c yazi\"")) -- File manager
hl.bind("SUPER + Q", hl.dsp.exec_cmd("brave")) -- Browser
hl.bind("SHIFT+SUPER + Q", hl.dsp.exec_cmd("brave --incognito"))
hl.bind("SUPER+SHIFT + W", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"wps\" \"onlyoffice-desktopeditors\"")) -- Office software
hl.bind("CTRL+SUPER + V", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"pavucontrol-qt\" \"pavucontrol\"")) -- Volume mixer
hl.bind("SUPER + I", hl.dsp.exec_cmd("XDG_CURRENT_DESKTOP=gnome ~/.config/hypr/hyprland/scripts/launch_first_available.sh \"qs -p ~/.config/quickshell/$qsConfig/settings.qml\" \"systemsettings\" \"gnome-control-center\" \"better-control\"")) -- Settings app
hl.bind("CTRL+SHIFT + Escape", hl.dsp.exec_cmd("~/.config/hypr/hyprland/scripts/launch_first_available.sh \"command -v btop && kitty -1 fish -c btop\" \"gnome-system-monitor\" \"plasma-systemmonitor --page-name Processes\"")) -- Task manager

-- Cursed stuff
--# Make window not amogus large
hl.bind("CTRL+SUPER + Backslash", hl.dsp.window.resize({ x = 640, y = 480 })) -- [hidden]
