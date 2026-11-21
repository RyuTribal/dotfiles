import QtQuick.Controls

// Stands in for modules/common/widgets/DialogListItem.qml in tst_network_views.qml: a button with the
// `active` mark, the paddings and the content item WifiNetworkItem.qml sets, without RippleButton.
AbstractButton {
    property bool active: false

    horizontalPadding: 23
    verticalPadding: 12
}
