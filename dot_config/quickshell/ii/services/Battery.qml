pragma Singleton
pragma ComponentBehavior: Bound

import "power.js" as P
import QtQuick
import Quickshell

/**
 * The battery for the bar, the lock screen and the battery popup, a thin client of axon's
 * `power.battery` topic (section 30, "power"). The low and critical notices and the automatic suspend
 * run in axon, so they keep working while the shell is down. With axon down every property keeps its
 * last value, and `available` follows the last status.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "power.battery"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool available: P.isAvailable(topic.status)
    readonly property int chargeState: P.chargeStateNumber(_state) // UPower's numbers: 1 charging, 4 full
    readonly property bool isCharging: _state === "charging"
    readonly property bool isPluggedIn: P.isPluggedIn(_state)
    readonly property real percentage: _percent / 100 // 0..1
    readonly property real energyRate: _energyRate // W
    readonly property real timeToEmpty: _timeToEmpty // s
    readonly property real timeToFull: _timeToFull // s
    readonly property bool isLow: _isLow // axon's threshold, false while on AC

    property string _state: "unknown"
    property real _percent: 100
    property real _energyRate: 0
    property real _timeToEmpty: 0
    property real _timeToFull: 0
    property bool _isLow: false

    function _apply(data) {
        _state = data.state;
        _percent = data.percent;
        _energyRate = data.energy_rate_w;
        _timeToEmpty = data.time_to_empty_s;
        _timeToFull = data.time_to_full_s;
        _isLow = data.is_low;
    }
}
