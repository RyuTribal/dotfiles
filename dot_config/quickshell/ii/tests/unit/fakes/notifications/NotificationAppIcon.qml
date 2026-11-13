import QtQuick

// Stands in for modules/common/widgets/NotificationAppIcon.qml in tst_notification_list.qml (the real
// one needs Quickshell.Widgets): the icon's size only.
Item {
    property string image
    property var appIcon
    property var summary

    implicitWidth: 38
    implicitHeight: 38
}
