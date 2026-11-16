pragma Singleton

import QtQuick

// Stands in for services/Hypr.qml in tst_brightness_service.qml: Brightness.qml reads only the focused
// monitor, which the tests move.
QtObject {
    property string focusedMonitorName: "eDP-1"
}
