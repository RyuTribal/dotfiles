import QtQuick

// Stands in for modules/common/widgets/NotificationItem.qml in tst_notification_list.qml (the real one
// needs Quickshell.Hyprland): a fixed-height row, so a group's height follows its row count.
Item {
    property var notificationObject
    property bool expanded
    property bool onlyNotification

    implicitHeight: 18
}
