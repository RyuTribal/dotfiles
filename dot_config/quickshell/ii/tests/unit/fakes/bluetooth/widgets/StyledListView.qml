import QtQuick

// Stands in for modules/common/widgets/StyledListView.qml in tst_bluetooth_views.qml: a plain ListView
// with the switch BluetoothDialog.qml sets, without the add and remove animations.
ListView {
    property bool animateAppearance: true
}
