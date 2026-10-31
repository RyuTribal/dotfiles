import QtQuick

// Stands in for Quickshell's LazyLoader in tst_sweep_panel.qml: makes its component while `active`.
Item {
    id: root

    property bool active: false
    default property Component component
    readonly property QtObject item: inner.item

    Loader {
        id: inner
        active: root.active
        sourceComponent: root.component
    }
}
