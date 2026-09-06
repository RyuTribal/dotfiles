import qs.modules.common
import qs.modules.common.widgets
import qs
import Quickshell
import Quickshell.Io

QuickToggleButton {
    id: root
    buttonIcon: "gamepad"
    toggled: toggled

    onClicked: {
        root.toggled = !root.toggled
        if (root.toggled) {
            // The Lua config rejects `hyprctl keyword` ("keyword can't work with
            // non-legacy parsers"), so options are set through `hyprctl eval`.
            // disable_while_typing is turned off so the touchpad stays usable
            // while holding keys in games.
            Quickshell.execDetached(["hyprctl", "eval", `hl.config({
                animations = { enabled = false },
                decoration = { shadow = { enabled = false }, blur = { enabled = false }, rounding = 0 },
                general = { gaps_in = 0, gaps_out = 0, border_size = 1, allow_tearing = true },
                input = { touchpad = { disable_while_typing = false } },
            })`])
        } else {
            Quickshell.execDetached(["hyprctl", "reload"])
        }
    }
    Process {
        id: fetchActiveState
        running: true
        command: ["bash", "-c", `[ "$(hyprctl getoption animations:enabled -j | jq -r ".bool")" = "true" ]`]
        onExited: (exitCode, exitStatus) => {
            root.toggled = exitCode !== 0 // Inverted because enabled = nonzero exit
        }
    }
    StyledToolTip {
        content: Translation.tr("Game mode")
    }
}