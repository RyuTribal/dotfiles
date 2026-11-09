import QtQuick

// Stands in for modules/common/widgets/MaterialSymbol.qml in tst_spin_box.qml and
// tst_backend_spin_box.qml, so the spin box's two buttons load without the icon font and StyledText.
// tst_claude_indicator.qml loads ClaudeIndicator.qml over it, which sets fill.
Text {
    property real iconSize: 16
    property real fill: 0
}
