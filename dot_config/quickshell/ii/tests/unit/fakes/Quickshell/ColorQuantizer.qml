import QtQuick

// Stands in for Quickshell's ColorQuantizer under the real Appearance.qml in tst_theme.qml: it reads no
// image and offers no colours.
QtObject {
    property var source
    property int depth
    property real rescaleSize
    readonly property list<color> colors: []
}
