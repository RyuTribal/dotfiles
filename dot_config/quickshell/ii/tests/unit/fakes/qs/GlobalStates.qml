pragma Singleton

import QtQuick

// Stands in for GlobalStates.qml in tst_wallpapers.qml: only whether the wallpaper selector is open,
// which Wallpapers.qml's list and the selector's tiles read and write. tst_claude_indicator.qml loads
// ClaudeIndicator.qml over it, whose click opens the top menu on a tab.
QtObject {
    property bool wallpaperSelectorOpen: false
    property bool topMenuOpen: false
    property string topMenuTab: ""
    // tst_notifications_service.qml loads Notifications.qml over it: the sidebar hides and expires
    // popups; the lock screen hides the popup window.
    property bool sidebarRightOpen: false
    property bool screenLocked: false
}
