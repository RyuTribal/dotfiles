import QtQuick

/**
 * The write-and-follow logic of a settings control over an axon topic value (section 50.1), shared
 * by BackendSpinBox and ThemeKnobControl. The control shows `shown`: the topic's value, or a write
 * of its own that the topic does not show yet, so the old value never comes back for the moment
 * between axon's reply and its next snapshot. A refused write snaps back to the topic at once, and
 * the reply's error is `errorText`.
 *
 * The control binds `topicValue` to the value its topic shows, sets `send` to the call that writes
 * a value and passes the reply's error (or null) to `done`, and calls write() on user interaction
 * only. Typing or a click drops a binding on the control's value: it binds that value to `shown`
 * again after each interaction.
 */
QtObject {
    id: root

    property var topicValue
    property var send // (value, done) => a write that calls done(error or null)
    // The latest write while the topic does not show it yet, else the topic's value.
    readonly property var shown: _writes.length > 0 ? _writes[_writes.length - 1] : topicValue
    property string errorText: "" // the last write's error; empty once axon took a write

    // While a write is not shown by the topic yet: the values the topic can still show from before
    // it, which are its value before the first write and every earlier write, then that write, last.
    // Empty while `shown` follows the topic.
    property var _writes: []

    function write(value) {
        _writes = (_writes.length === 0 ? [topicValue] : _writes).concat([value]);
        send(value, error => {
            root.errorText = error?.message ?? "";
            // A refused value snaps back. A taken one may already show: the snapshot can come first.
            if (error)
                root._writes = [];
            else
                root._followTopicOnceCurrent();
        });
    }

    // The topic shows the latest write, or a value that is neither from before it nor one of the
    // writes: someone else changed the value, and the one axon uses is the one to show.
    function _followTopicOnceCurrent() {
        if (_writes.length === 0)
            return;
        if (topicValue === _writes[_writes.length - 1] || !_writes.includes(topicValue))
            _writes = [];
    }

    onTopicValueChanged: _followTopicOnceCurrent()
}
