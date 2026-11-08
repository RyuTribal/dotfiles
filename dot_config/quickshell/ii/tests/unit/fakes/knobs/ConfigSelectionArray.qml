import QtQuick

// Stands in for modules/common/widgets/ConfigSelectionArray.qml in tst_wallpapers.qml, which loads
// ThemeKnobControl.qml: the same API, `options`, `currentValue` and the one signal `selected`, which
// a test emits as a click on an option would. The real one is built from buttons that need
// Quickshell.Hyprland.
Item {
    property list<var> options: []
    property var currentValue: null

    signal selected(var newValue)
}
