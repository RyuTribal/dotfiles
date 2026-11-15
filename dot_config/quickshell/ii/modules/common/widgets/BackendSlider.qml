import QtQuick

/**
 * A StyledSlider for a value axon owns (section 20, the dragged-slider rule). While dragged, the
 * handle keeps its own value and `send` goes out at most once per throttleMs, with the rest of a
 * burst sent when that time is up and a final `send` on release while a moved value is unsent. A
 * click that never moves the handle sends nothing. While not pressed, the slider shows serviceValue.
 * Connect `send` to the service function that writes the value.
 */
StyledSlider {
    id: root

    property real serviceValue: 0
    property int throttleMs: 33
    // The wall clock in ms, read through this one property so a test can drive it.
    property var wallClock: Date.now

    signal send(real value)

    property double _lastSendAt: -Infinity
    // A moved value not sent yet. Release sends only then, so a click that never moves the handle
    // writes nothing (a sink above 100 % keeps its volume).
    property bool _pending: false

    // A drag step: sends now when throttleMs has passed since the last send, else arms the one-shot
    // timer for the rest of it (once; later steps inside the same wait only move the value it sends).
    function _moved() {
        _pending = true;
        const wait = _lastSendAt + throttleMs - wallClock();
        if (wait <= 0) {
            _flush();
            return;
        }
        if (!throttleTimer.running) {
            throttleTimer.interval = Math.max(1, Math.ceil(wait));
            throttleTimer.start();
        }
    }

    // Sends the handle's value now.
    function _flush() {
        throttleTimer.stop();
        _pending = false;
        _lastSendAt = wallClock();
        send(value);
    }

    onMoved: _moved()
    onPressedChanged: {
        if (!pressed && _pending)
            _flush();
    }

    // RestoreNone: at a press the handle keeps the value under it; the default would put back the
    // value from before the Binding first took effect.
    Binding {
        target: root
        property: "value"
        value: root.serviceValue
        when: !root.pressed
        restoreMode: Binding.RestoreNone
    }

    // One-shot, armed only by _moved().
    Timer {
        id: throttleTimer
        repeat: false
        onTriggered: root._flush()
    }
}
