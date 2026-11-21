import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland

Scope {
    id: notificationPopup

    PanelWindow {
        id: root
        // Shown until the list has given its space back, not only while there are popups. The list
        // applies a removal in its polish, which a hidden window does not run: hidden at once, the window
        // kept the last group and played its fade when the next popup showed it again. With no group
        // left, contentHeight is 0 and so is the mask, and an empty mask takes no input.
        visible: (Notifications.popupList.length > 0 || listview.contentHeight > 0) && !GlobalStates.screenLocked
        screen: Quickshell.screens.find(s => s.name === Hypr.focusedMonitorName) ?? null

        WlrLayershell.namespace: "quickshell:notificationPopup"
        WlrLayershell.layer: WlrLayer.Overlay
        exclusiveZone: 0

        anchors {
            top: true
            right: true
            bottom: true
        }

        // Input reaches only the groups, so the screen around them stays usable.
        mask: Region {
            item: drawnGroups
        }

        color: "transparent"
        implicitWidth: Appearance.sizes.notificationPopupWidth

        // Where the groups are drawn. The list lays them out from originY in its contentItem, and
        // originY moves when a group above the others leaves, so the contentItem's own rectangle (0 to
        // contentHeight) can sit above the groups. Masked to it, the popups passed every click to the
        // window below once a group had left above them.
        Item {
            id: drawnGroups
            x: listview.x
            y: listview.y + listview.contentItem.y + listview.originY
            width: listview.width
            height: listview.contentHeight
        }

        NotificationListView {
            id: listview
            anchors {
                top: parent.top
                bottom: parent.bottom
                right: parent.right
                rightMargin: 4
                topMargin: 4
            }
            implicitWidth: parent.width - Appearance.sizes.elevationMargin * 2
            popup: true
        }
    }
}
