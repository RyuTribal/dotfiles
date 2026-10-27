import QtQuick
import QtQuick.Layouts
import qs.modules.common

// The text and styling of the "config.json has an error" toast, in the colours of ReloadPopup's
// failure case. It has no window, so ConfigErrorPopup and a plain test window can both show it.
Item {
    id: root

    // Qt's JSON.parse reports no position, so the text never names a line.
    readonly property string text: `config.json has an error; changes are not saved until it is fixed: ${Config.parseError}`

    implicitWidth: background.implicitWidth
    implicitHeight: background.implicitHeight

    Rectangle {
        id: background
        anchors.fill: parent
        color: "#ffe99195"
        radius: 12
        implicitWidth: layout.implicitWidth + 30
        implicitHeight: layout.implicitHeight + 30

        ColumnLayout {
            id: layout
            anchors.centerIn: parent
            spacing: 10

            Text {
                renderType: Text.NativeRendering
                font.family: "Rubik"
                font.pointSize: 14
                text: "Quickshell: config.json is invalid"
                color: "#ff93000A"
            }

            Text {
                Layout.maximumWidth: 600
                renderType: Text.NativeRendering
                font.family: "JetBrains Mono NF"
                font.pointSize: 11
                text: root.text
                wrapMode: Text.Wrap
                color: "#ff93000A"
            }
        }
    }
}
