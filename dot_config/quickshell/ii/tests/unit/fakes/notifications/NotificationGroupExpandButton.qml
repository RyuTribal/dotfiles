import QtQuick

// Stands in for modules/common/widgets/NotificationGroupExpandButton.qml in tst_notification_list.qml
// (the real one is a RippleButton): the button's size and the members NotificationGroup sets.
Item {
    property int count
    property bool expanded
    property real fontSize
    property var altAction

    signal clicked

    implicitWidth: 30
    implicitHeight: 20
}
