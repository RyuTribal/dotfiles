import QtQuick

// Stands in for modules/sidebarRight/quickToggles/QuickToggleButton.qml in tst_network_views.qml: the
// properties and the click NetworkToggle.qml sets, without GroupButton and its animations.
Item {
    property bool toggled: false
    property string buttonIcon
    property var altAction: null

    signal clicked
}
