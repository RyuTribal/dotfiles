import QtQuick

// Stands in for modules/bar/StyledPopup.qml in tst_claude_indicator.qml: the content item laid out in
// place at its own size, with no layer-shell window, so the test can measure what the real popup
// would size itself from (its content's implicitWidth).
Item {
    id: root

    property Item hoverTarget
    default property Item contentItem
    property bool active: false

    implicitWidth: root.contentItem.implicitWidth
    implicitHeight: root.contentItem.implicitHeight
    children: [root.contentItem]
}
