import qs
import QtQuick
import Quickshell
import Quickshell.Hyprland
pragma Singleton
pragma ComponentBehavior: Bound

Singleton {
    id: root
    property bool barOpen: true
    property bool sidebarRightOpen: false
    property bool mediaControlsOpen: false
    property bool osdBrightnessOpen: false
    property bool osdVolumeOpen: false
    property bool overviewOpen: false
    property bool wallpaperSelectorOpen: false
    property bool screenLocked: false
    property bool screenLockContainsCharacters: false
    property bool screenUnlockFailed: false
    property bool sessionOpen: false
    property bool superDown: false
    property bool superReleaseMightTrigger: true
    property bool topMenuOpen: false
    // One-shot tab-jump target for the top menu: set to a tab name ("media",
    // "calendar", "todo", "timer", "stats") to have TopMenuContent.qml switch
    // to it, then reset to "" immediately after being consumed. See
    // TopMenu.qml's IpcHandler.openTab and TopMenuContent.qml's Connections.
    property string topMenuTab: ""
    property bool workspaceShowNumbers: false

    GlobalShortcut {
        name: "workspaceNumber"
        description: "Hold to show workspace numbers, release to show icons"

        onPressed: {
            root.superDown = true
        }
        onReleased: {
            root.superDown = false
        }
    }
}