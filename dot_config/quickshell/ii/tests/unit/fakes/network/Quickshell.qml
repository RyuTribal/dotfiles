pragma Singleton

import QtQuick

// Stands in for Quickshell's global object in tst_network_views.qml: execDetached records each
// command in `launched` instead of starting it, so the views' launchers can be checked and nothing
// reaches NetworkManager.
QtObject {
    property var launched: []

    function execDetached(command) {
        launched.push(command);
    }
}
