pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import QtQuick
import Quickshell

/**
 * Night light, a thin client of axon's `nightlight` topic (section 30, "nightlight"). axon runs the
 * schedule on wall-clock timers, drives hyprsunset over its socket and keeps a manual toggle until
 * the next schedule boundary; this only shows the state and sends the two calls.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "nightlight"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool active: _active // the screen's tint as hyprsunset reports it
    readonly property bool automatic: _automatic
    readonly property bool available: B.isAvailable(topic.status)

    property bool _active: false
    property bool _automatic: false

    function toggle() {
        Backend.call("nightlight.toggle", {});
    }

    function setAutomatic(on) {
        Backend.call("nightlight.set_automatic", {
            on: on
        });
    }

    function _apply(data) {
        _active = data.active;
        _automatic = data.automatic;
    }
}
