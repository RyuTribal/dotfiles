import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import Quickshell

StyledListView { // Scrollable window
    id: root
    property bool popup: false

    spacing: 3

    // No transition places a group. While StyledListView's add/remove transitions ran (500 ms), a
    // group that changed height did not move the groups below it, and they overlapped for good. The
    // list lays every group out at once, and each group plays its own appear and disappear below.
    add: null
    addDisplaced: null
    displaced: null
    move: null
    moveDisplaced: null
    remove: null
    removeDisplaced: null

    model: ScriptModel {
        values: root.popup ? Notifications.popupAppNameList : Notifications.appNameList
    }
    delegate: NotificationGroup {
        id: group
        required property int index
        required property var modelData
        popup: root.popup
        anchors.left: parent?.left
        anchors.right: parent?.right
        notificationGroup: popup ? Notifications.popupGroupsByAppName[modelData] : Notifications.groupsByAppName[modelData]

        // ListView emits add only for rows the model inserts while the list exists, so the groups
        // there when it is created, or made later by scrolling, show without animating.
        ListView.onAdd: {
            group.opacity = 0;
            group.scale = 0;
            appearAnimation.restart();
        }
        // The group keeps its space until it has faded out; stopped also fires when the animation is
        // cut short, so the space is always given back.
        ListView.onRemove: {
            appearAnimation.stop();
            group.ListView.delayRemove = true;
            disappearAnimation.restart();
        }

        // StyledListView's add transition (popin): opacity and scale from 0 to 1.
        NumberAnimation {
            id: appearAnimation
            target: group
            properties: "opacity,scale"
            to: 1
            duration: Appearance.animation.elementMove.duration
            easing.type: Appearance.animation.elementMove.type
            easing.bezierCurve: Appearance.animation.elementMove.bezierCurve
        }
        // StyledListView's remove transition as it showed: its slide to the right never moved a group,
        // whose anchors hold x, so it was this fade.
        NumberAnimation {
            id: disappearAnimation
            target: group
            property: "opacity"
            to: 0
            duration: Appearance.animation.elementMove.duration
            easing.type: Appearance.animation.elementMove.type
            easing.bezierCurve: Appearance.animation.elementMove.bezierCurve
            onStopped: group.ListView.delayRemove = false
        }
    }
}
