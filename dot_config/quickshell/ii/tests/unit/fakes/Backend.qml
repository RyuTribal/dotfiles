pragma Singleton

import QtQuick

// Stands in for services/Backend.qml in tst_backend_behaviour.qml: records calls and attachments,
// and the test sets up, instance, now and downSince itself. backend_tests.sh puts it next to
// copies of BackendTopic.qml and BackendList.qml.
QtObject {
    property bool up: true
    property string instance: "one"
    property double now: 0
    property double downSince: 0
    property var calls: []       // { method, args, done } per call, in order
    property var attachments: [] // "+name" per _attach, "-name" per _detach

    function call(method, args, done) {
        calls.push({
            method: method,
            args: args,
            done: done
        });
        return calls.length;
    }

    function _attach(topic) {
        attachments.push("+" + topic.name);
    }

    function _detach(topic) {
        attachments.push("-" + topic.name);
    }
}
