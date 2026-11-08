pragma Singleton

import QtQuick

// Stands in for GlobalStates.qml in tst_wallpapers.qml: only whether the wallpaper selector is open,
// which Wallpapers.qml's list and the selector's tiles read and write.
QtObject {
    property bool wallpaperSelectorOpen: false
}
