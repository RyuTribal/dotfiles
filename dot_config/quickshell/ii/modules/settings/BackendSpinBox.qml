import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick

/**
 * A settings spin box for one axon.toml key (section 50.1, backend controls).
 * It shows the effective value from axon's `config` topic and writes a value
 * the user picks with `config.set`. A written value stays shown until the
 * topic shows it, so the old value never comes back for the moment between
 * axon's reply and its next snapshot. A refused write snaps back to the topic
 * at once, and the reply's error shows beside it. An entry typed back to the
 * value it started from writes nothing: the control follows the topic again,
 * or keeps showing a write that still waits for its reply. Until the first
 * `config` snapshot it shows "--" and "axon unreachable", never a default.
 */
ConfigSpinBox {
    id: root

    required property string key // a dotted axon.toml key, such as "resources.interval_ms"
    property string errorText: "" // the last write's error; empty once axon took a write

    // While the control shows a write the topic does not show yet: the values the topic can still show
    // from before that write, which are its value before the first write and every earlier write, then
    // that write, last. Empty while the control follows the topic.
    property var _writes: []

    enabled: AxonConfig.writable
    value: root._topicValue()
    textFromValue: number => AxonConfig.known ? String(number) : "--"
    onValueModified: root._write(root.value)
    // Typing drops the value's binding to the topic. An entry that ends unchanged writes nothing,
    // so no reply comes to bind it again: this handler does. While a write is pending the control
    // keeps showing that write, and its reply binds it.
    onEntryEndedUnchanged: {
        if (root._writes.length === 0)
            root._followTopic();
    }

    // `from` stands in until the first snapshot, shown as "--".
    function _topicValue() {
        return AxonConfig.known ? AxonConfig.value(root.key) : root.from;
    }

    function _write(written) {
        _writes = (_writes.length === 0 ? [_topicValue()] : _writes).concat([written]);
        value = written; // a plain value: the topic's snapshots no longer move it
        AxonConfig.set(key, written, error => {
            root.errorText = error?.message ?? "";
            // A refused value snaps back. A taken one may already show: the snapshot can come first.
            if (error)
                root._followTopic();
            else
                root._followTopicOnceCurrent();
        });
    }

    // The topic shows the latest write, or a value that is neither from before it nor one of the
    // writes: someone else changed the key, and the value axon uses is the one to show.
    function _followTopicOnceCurrent() {
        if (_writes.length === 0)
            return;
        const current = _topicValue();
        if (current === _writes[_writes.length - 1] || !_writes.includes(current))
            _followTopic();
    }

    function _followTopic() {
        _writes = [];
        value = Qt.binding(() => root._topicValue());
    }

    Connections {
        target: AxonConfig
        function onValuesChanged() {
            root._followTopicOnceCurrent();
        }
    }

    StyledText {
        objectName: "errorRow" // the tests find the row by this name
        visible: text !== ""
        text: AxonConfig.known ? root.errorText : "axon unreachable"
        font.pixelSize: Appearance.font.pixelSize.small
        color: Appearance.colors.colError
    }
}
