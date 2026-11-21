import QtQuick.Controls

// Stands in for modules/common/widgets/DialogListItem.qml in tst_bluetooth_views.qml: a button with the
// paddings, the cursor flag, the alt action and the content item BluetoothDeviceItem.qml sets, without
// RippleButton.
AbstractButton {
    property bool active: false
    property bool pointingHandCursor: true
    property var altAction: null

    horizontalPadding: 23
    verticalPadding: 12
}
