import QtQuick
import QtQuick.Controls

// Stands in for modules/common/widgets/StyledProgressBar.qml in tst_claude_panel.qml, whose real
// file needs Quickshell.Widgets and Appearance animations the fakes do not have. It keeps the real
// bar's size hints, a 120 x 4 background, because the panel's layout is what the test measures.
ProgressBar {
    background: Item {
        implicitWidth: 120
        implicitHeight: 4
    }
    contentItem: Item {}
}
