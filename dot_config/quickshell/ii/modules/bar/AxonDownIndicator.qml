import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts

// The axon-down icon (section 20, "When axon is down at startup"): shown
// once the shell has had no server hello for 3 s, at startup or after a
// drop, and hidden again by the next hello. Hovering shows why and the
// command that tells more. It is not a notification, because the
// notification server is axon itself.
MouseArea {
    id: root
    implicitWidth: Backend.downShown ? icon.implicitWidth : 0
    implicitHeight: Appearance.sizes.barHeight
    visible: Backend.downShown
    hoverEnabled: true

    readonly property string statusCommand: "systemctl --user status axond.socket axond"
    // What the popup says under its heading.
    readonly property string reason: Backend.lastError || "waiting for axon to answer"
    readonly property string popupText: reason + "\n" + statusCommand

    MaterialSymbol {
        id: icon
        anchors.centerIn: parent
        text: "link_off"
        iconSize: Appearance.font.pixelSize.larger
        color: Appearance.colors.colError
    }

    StyledPopup {
        hoverTarget: root

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            RowLayout {
                spacing: 5

                MaterialSymbol {
                    fill: 0
                    font.weight: Font.Medium
                    text: "link_off"
                    iconSize: Appearance.font.pixelSize.large
                    color: Appearance.colors.colOnSurfaceVariant
                }
                StyledText {
                    text: "axon is not reachable"
                    font {
                        weight: Font.Medium
                        pixelSize: Appearance.font.pixelSize.normal
                    }
                    color: Appearance.colors.colOnSurfaceVariant
                }
            }

            StyledText {
                text: root.popupText
                color: Appearance.colors.colOnSurfaceVariant
            }
        }
    }
}
