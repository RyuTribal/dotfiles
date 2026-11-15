import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts
import Quickshell

Item {
    id: root
    required property var node // a row of Audio.streams (audio.js streamRow)

    implicitHeight: rowLayout.implicitHeight

    RowLayout {
        id: rowLayout
        anchors.fill: parent
        spacing: 6

        Image {
            property real size: slider.height * 0.9
            Layout.alignment: Qt.AlignHCenter | Qt.AlignVCenter
            visible: source != ""
            sourceSize.width: size
            sourceSize.height: size
            source: {
                let icon;
                icon = AppSearch.guessIcon(root.node.iconName);
                if (AppSearch.iconExists(icon))
                    return Quickshell.iconPath(icon, "image-missing");
                icon = AppSearch.guessIcon(root.node.nodeName);
                return Quickshell.iconPath(icon, "image-missing");
            }
        }

        ColumnLayout {
            Layout.fillWidth: true
            spacing: -4

            StyledText {
                Layout.fillWidth: true
                font.pixelSize: Appearance.font.pixelSize.small
                color: Appearance.colors.colSubtext
                elide: Text.ElideRight
                text: {
                    // axon gives application.name, else node.description, else node.name
                    const app = root.node.appName;
                    const media = root.node.mediaName;
                    return media ? `${app} • ${media}` : app;
                }
            }

            BackendSlider {
                id: slider
                serviceValue: root.node.audio.volume
                onSend: value => Audio.setStreamVolume(root.node.id, value)
            }
        }
    }
}
