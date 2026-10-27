import QtQuick

// Stands in for Quickshell's FileView in tst_config.qml. It touches no file: the test sets `content`,
// emits loaded() and the other signals, and reads how often reload() and writeAdapter() were called.
QtObject {
    id: view

    default property list<QtObject> data
    property string path
    property bool watchChanges
    property string content: ""
    property int reloads: 0
    property int writes: 0

    signal fileChanged
    signal adapterUpdated
    signal loaded
    signal loadFailed(int error)

    function text() {
        return content;
    }

    function reload() {
        reloads++;
    }

    function writeAdapter() {
        writes++;
    }

    Component.onCompleted: FileViewProbe.view = view
}
