import QtQuick

// Stands in for Quickshell's Process in tst_favicon_cache.qml. It starts nothing: a Process that is
// set running is added to ProcessProbe.started, and the test ends it with exited().
QtObject {
    id: process

    default property list<QtObject> data
    property var command: []
    property bool running: false

    property bool _listed: false

    signal exited(int exitCode, int exitStatus)

    // `running: true` at creation can reach both handlers, so a Process is listed once.
    function _list() {
        if (running && !_listed) {
            _listed = true;
            ProcessProbe.started.push(process);
        }
    }

    onRunningChanged: _list()
    Component.onCompleted: _list()
}
