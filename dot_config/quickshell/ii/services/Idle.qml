pragma Singleton
pragma ComponentBehavior: Bound

import "power.js" as P
import QtQuick
import Quickshell

/**
 * Keeping the system awake, a thin client of axon's `power.idle` topic (section 30, "power"). axon
 * holds the inhibit on hypridle over D-Bus and keeps the wish across reboots.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "power.idle"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool inhibit: _inhibit // the wish, also while hypridle is not running
    readonly property bool available: P.isAvailable(topic.status)

    property bool _inhibit: false

    function toggleInhibit() {
        Backend.call("power.set_idle_inhibit", {
            inhibit: !root.inhibit
        });
    }

    function _apply(data) {
        _inhibit = data.inhibit;
    }
}
