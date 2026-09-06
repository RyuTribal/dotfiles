import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts
import Quickshell

// Click-opened popup for the right sidebar's Tools button. Follows
// SelectionDialog's scrim + anchored-panel idiom (see
// modules/common/widgets/SelectionDialog.qml) rather than the bar's
// hover-triggered StyledPopup (modules/bar/StyledPopup.qml), since this
// menu has to stay open on its own until the user picks an entry or
// dismisses it, rather than following mouse hover.
//
// Growing the tool list: each entry is one object in the `tools` array
// below - name, icon, description and an action() to run. No registry
// service or config schema, just add a line here for the next tool.
Item {
    id: root

    property bool open: false
    visible: open
    anchors.fill: parent

    readonly property var tools: [
        {
            name: Translation.tr("Sweep"),
            icon: "storage",
            description: Translation.tr("Disk usage inspector"),
            action: () => {
                // SweepPanel.qml owns its own visibility as a local
                // property on its Scope (not a GlobalStates flag), toggled
                // through its IpcHandler{ target: "sweep" } - see the
                // toggle()/show()/hide() functions there. Match that API.
                Quickshell.execDetached(["qs", "-c", "ii", "ipc", "call", "sweep", "toggle"]);
            }
        }
    ]

    function runTool(tool) {
        tool.action();
        root.open = false;
        GlobalStates.sidebarRightOpen = false;
    }

    // Scrim: clicking outside the panel dismisses the popup without
    // running anything, mirroring SelectionDialog's cancel-by-scrim feel.
    MouseArea {
        anchors.fill: parent
        hoverEnabled: true
        onClicked: root.open = false
    }

    StyledRectangularShadow {
        target: panel
    }

    Rectangle {
        id: panel
        anchors.top: parent.top
        anchors.right: parent.right
        anchors.topMargin: 55
        anchors.rightMargin: 10
        width: 270
        implicitHeight: panelColumn.implicitHeight + 20
        radius: Appearance.rounding.normal
        color: Appearance.colors.colSurfaceContainerHigh
        border.width: 1
        border.color: Appearance.colors.colLayer0Border

        // Swallow clicks so they don't fall through to the scrim behind.
        MouseArea {
            anchors.fill: parent
            onClicked: {}
        }

        ColumnLayout {
            id: panelColumn
            anchors.fill: parent
            anchors.margins: 10
            spacing: 8

            RowLayout {
                spacing: 8

                MaterialSymbol {
                    text: "construction"
                    iconSize: Appearance.font.pixelSize.large
                    color: Appearance.colors.colOnLayer1
                }
                StyledText {
                    text: Translation.tr("Tools")
                    font.pixelSize: Appearance.font.pixelSize.normal
                    font.weight: Font.DemiBold
                    color: Appearance.colors.colOnLayer1
                }
            }

            Rectangle {
                Layout.fillWidth: true
                implicitHeight: 1
                color: Appearance.colors.colLayer0Border
            }

            Repeater {
                model: root.tools

                delegate: Rectangle {
                    id: toolRow
                    required property var modelData
                    Layout.fillWidth: true
                    implicitHeight: 46
                    radius: Appearance.rounding.small
                    color: toolArea.containsMouse ? Appearance.colors.colLayer1Hover : "transparent"

                    RowLayout {
                        anchors.fill: parent
                        anchors.margins: 6
                        spacing: 10

                        MaterialSymbol {
                            text: toolRow.modelData.icon
                            iconSize: Appearance.font.pixelSize.larger
                            color: Appearance.colors.colOnLayer1
                        }
                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 0

                            StyledText {
                                text: toolRow.modelData.name
                                font.pixelSize: Appearance.font.pixelSize.normal
                                color: Appearance.colors.colOnLayer1
                            }
                            StyledText {
                                text: toolRow.modelData.description
                                font.pixelSize: Appearance.font.pixelSize.smaller
                                color: Appearance.colors.colSubtext
                            }
                        }
                    }

                    MouseArea {
                        id: toolArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onClicked: root.runTool(toolRow.modelData)
                    }
                }
            }
        }
    }
}
