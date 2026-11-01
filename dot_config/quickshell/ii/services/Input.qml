pragma Singleton
pragma ComponentBehavior: Bound

import "input.js" as I
import QtQuick
import Quickshell

/**
 * The input method indicator of the bar and the lock screen, a thin client of axon's `input` topic
 * (section 30, "input"): fcitx5's input methods and the current one, as the short codes the
 * indicator shows. Both properties are empty while the status is not "ok", so the indicator hides.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "input"
        onSnapshot: data => root._apply(data)
    }

    readonly property list<string> layoutCodes: I.layoutCodes(topic.status, _methods)
    readonly property string currentLayoutCode: I.currentCode(topic.status, _methods, _current)

    property var _methods: [] // [{ id, rev, name, code }]
    property string _current: "" // the current input method's id

    function _apply(data) {
        _methods = data.methods;
        _current = data.current;
    }
}
