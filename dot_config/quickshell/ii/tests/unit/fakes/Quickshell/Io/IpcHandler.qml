import QtQuick

// Stands in for Quickshell.Io's IpcHandler in tst_sweep_panel.qml and tst_sweep_panel_toggle.qml. It
// only holds its target and the functions declared on it, which tst_sweep_panel_toggle.qml calls as
// an IPC call would.
QtObject {
    property string target: ""
}
