import QtQuick
import QtQuick.Controls

// Stands in for modules/common/widgets/DialogButton.qml in tst_network_views.qml: the click and the
// properties WifiDialog.qml and WifiNetworkItem.qml set, without RippleButton.
AbstractButton {
    property string buttonText
    property color colBackground
    property color colBackgroundHover
    property color colRipple
}
