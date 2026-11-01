import "../../services/hypr.js" as H
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs
import QtQuick
import QtQuick.Layouts
import Quickshell

Item {
    id: root
    readonly property var monitor: Hypr.monitorFor(root.QsWindow.window?.screen)
    readonly property var activeWindow: Hypr.activeWindow

    // The focused window when it is on this monitor, else the icon window of this monitor's
    // workspace, else "Desktop" and the workspace (section 30, N30-5).
    readonly property var barText: H.barTitle(root.activeWindow, root.monitor?.id,
        Hypr.iconWindow(root.monitor?.active_workspace), Translation.tr("Desktop"),
        Translation.tr("Workspace") + " " + (root.monitor?.active_workspace ?? 1))

    implicitWidth: colLayout.implicitWidth

    ColumnLayout {
        id: colLayout

        anchors.verticalCenter: parent.verticalCenter
        anchors.left: parent.left
        anchors.right: parent.right
        spacing: -4

        StyledText {
            Layout.fillWidth: true
            font.pixelSize: Appearance.font.pixelSize.smaller
            color: Appearance.colors.colSubtext
            elide: Text.ElideRight
            text: root.barText.subtitle

        }

        StyledText {
            Layout.fillWidth: true
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colOnLayer0
            elide: Text.ElideRight
            text: root.barText.title
        }

    }

}
