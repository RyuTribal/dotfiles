import QtQuick
import QtQuick.Layouts

// Stands in for modules/common/widgets/WindowDialog.qml in tst_network_views.qml: its children in
// one column, in the order WifiDialog.qml declares them, without the scrim and its animations.
ColumnLayout {
    property bool show: false

    signal dismiss
}
