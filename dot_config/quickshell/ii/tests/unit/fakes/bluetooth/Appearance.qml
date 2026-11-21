pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_bluetooth_views.qml: the keys BluetoothDialog.qml,
// BluetoothDeviceItem.qml and BluetoothToggle.qml read. The test checks text, visibility and calls, not
// colours; the item's arrow turns with an instant animation.
QtObject {
    id: root

    readonly property Component instantAnimation: Component {
        NumberAnimation {
            duration: 0
        }
    }

    property var colors: ({
            colError: "#ffb4ab",
            colErrorHover: "#ffc4bb",
            colErrorActive: "#ffd4cb",
            colOnError: "#690005",
            colPrimary: "#84d2e6",
            colPrimaryHover: "#94e2f6",
            colPrimaryActive: "#a4f2ff",
            colOnPrimary: "#003640",
            colOnSurfaceVariant: "#b2cad3",
            colOnLayer3: "#dde3e6",
            colSubtext: "#8c9aa0"
        })
    property var font: ({
            pixelSize: ({
                    smaller: 12,
                    small: 13,
                    larger: 19
                })
        })
    property var rounding: ({
            large: 23
        })
    property var animation: ({
            elementMoveFast: ({
                    numberAnimation: root.instantAnimation
                })
        })
}
