pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_network_views.qml: the keys WifiDialog.qml,
// WifiNetworkItem.qml and NetworkToggle.qml read. The test checks text and visibility, not colours.
QtObject {
    property var colors: ({
            colError: "#ffb4ab",
            colLayer4: "#303030",
            colLayer4Hover: "#404040",
            colLayer4Active: "#505050",
            colOnSurfaceVariant: "#b2cad3"
        })
    property var font: ({
            pixelSize: ({
                    small: 12,
                    larger: 19
                })
        })
    property var rounding: ({
            large: 23
        })
}
