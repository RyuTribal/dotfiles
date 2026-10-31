import QtQuick

// Stands in for Quickshell's ScriptModel in tst_sweep_panel.qml: one row per item of `values`, which
// a delegate reads as `modelData`. It makes every row anew on each change, where ScriptModel keeps
// the rows whose `objectProp` stayed.
ListModel {
    id: root

    property var values: []
    property string objectProp: ""

    onValuesChanged: {
        clear();
        for (const value of values)
            append({
                modelData: value
            });
    }
}
