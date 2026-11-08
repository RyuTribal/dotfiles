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
 * The write logic is FollowedWrites'.
 */
ConfigSpinBox {
    id: root

    required property string key // a dotted axon.toml key, such as "resources.interval_ms"
    property alias errorText: writes.errorText // the last write's error; empty once axon took a write

    enabled: AxonConfig.writable
    value: writes.shown
    textFromValue: number => AxonConfig.known ? String(number) : "--"
    // A step or a finished entry drops the value's binding: bind it to what the control shows again.
    onValueModified: {
        writes.write(value);
        value = Qt.binding(() => writes.shown);
    }
    // Typing drops the value's binding too. An entry that ends unchanged writes nothing, so it shows
    // the topic again, or the write that still waits for its reply.
    onEntryEndedUnchanged: value = Qt.binding(() => writes.shown)

    FollowedWrites {
        id: writes
        // `from` stands in until the first snapshot, shown as "--".
        topicValue: AxonConfig.known ? AxonConfig.value(root.key) : root.from
        send: (value, done) => AxonConfig.set(root.key, value, done)
    }

    StyledText {
        objectName: "errorRow" // the tests find the row by this name
        visible: text !== ""
        text: AxonConfig.known ? root.errorText : "axon unreachable"
        font.pixelSize: Appearance.font.pixelSize.small
        color: Appearance.colors.colError
    }
}
