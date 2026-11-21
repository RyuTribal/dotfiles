import QtQuick
import QtQuick.Controls

// Stands in for modules/common/widgets/DialogButton.qml in tst_bluetooth_views.qml: the click and the
// properties BluetoothDialog.qml and BluetoothDeviceItem.qml set, without RippleButton.
AbstractButton {
    property string buttonText
    property color colBackground
    property color colBackgroundHover
    property color colRipple
    property color colText
}
