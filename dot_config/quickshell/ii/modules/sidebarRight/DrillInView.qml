import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.sidebarRight.quickToggles
import QtQuick
import QtQuick.Layouts

// A generic drill-in page for the sidebar's quick settings panel: a
// back-button header followed by whatever content is passed in. Used to
// re-root the volume mixer, Wi-Fi network list and Bluetooth device list
// as pages instead of separate modal dialogs.
ColumnLayout {
    id: root
    required property string title
    signal back()
    default property alias content: contentColumn.data

    spacing: 8

    RowLayout {
        Layout.fillWidth: true
        spacing: 5

        QuickToggleButton {
            toggled: false
            buttonIcon: "arrow_back"
            onClicked: root.back()
            StyledToolTip {
                content: Translation.tr("Back")
            }
        }
        StyledText {
            Layout.fillWidth: true
            font.pixelSize: Appearance.font.pixelSize.larger
            color: Appearance.colors.colOnLayer1
            elide: Text.ElideRight
            text: root.title
        }
    }

    ColumnLayout {
        id: contentColumn
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 0
    }
}
