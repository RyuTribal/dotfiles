pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import qs.modules.common
import QtQuick
import Quickshell

/**
 * Date and time strings, and the uptime, from axon's `system` topic (section
 * 30, "system"). The clock Timer ticks on each wall-clock minute boundary; the uptime
 * Timer ticks where the uptime itself rolls over, at boot_at + k minutes, so the displayed
 * minutes change when the real uptime does. Both are re-armed when the system resumes or
 * the clock steps, because a timer set before a suspend or a step no longer lands on its
 * boundary.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "system"
        onSnapshot: data => root._apply(data)
    }

    // The start of the current minute; Hyprsunset and the clock tooltip read it.
    readonly property QtObject clock: QtObject {
        readonly property date date: root._minute
        readonly property int hours: root._minute.getHours()
        readonly property int minutes: root._minute.getMinutes()
    }
    // These read _minute, not clock.date: qmllint cannot see the members of a property typed QtObject.
    readonly property string time: Qt.locale().toString(_minute, Config.options?.time.format ?? "hh:mm")
    readonly property string shortDate: Qt.locale().toString(_minute, Config.options?.time.shortDateFormat ?? "dd/MM")
    readonly property string date: Qt.locale().toString(_minute, Config.options?.time.dateFormat ?? "dddd, dd/MM")
    readonly property string collapsedCalendarFormat: Qt.locale().toString(_minute, "dd MMMM yyyy")
    readonly property string uptime: _uptime // "--" until axon knows the boot instant

    // Counts re-arms of the minute timer. Tests read it to see that a resume or a clock step re-arms it.
    readonly property int minuteTicks: _ticks

    readonly property int _minuteMs: 60000

    property date _minute: new Date(0)
    property int _ticks: 0
    property string _uptime: "--"
    property double _bootAt: NaN
    property double _resumedAt: NaN
    property double _clockChangedAt: NaN

    function _apply(data) {
        const bootAt = data.boot_at ?? NaN;
        const resumedAt = data.resumed_at ?? NaN;
        const clockChangedAt = data.clock_changed_at ?? NaN;
        const clockMoved = !Object.is(resumedAt, _resumedAt) || !Object.is(clockChangedAt, _clockChangedAt);
        _resumedAt = resumedAt;
        _clockChangedAt = clockChangedAt;
        if (!Object.is(bootAt, _bootAt)) {
            _bootAt = bootAt;
            _updateUptime();
        }
        if (clockMoved)
            _tick();
    }

    // Takes the minute from the clock now and arms the timer for the next boundary.
    // Zone offsets are whole minutes, so a UTC minute boundary is a local one.
    function _tick() {
        const now = Date.now();
        _ticks += 1;
        _minute = new Date(now - now % _minuteMs);
        minuteTimer.stop();
        minuteTimer.interval = _minuteMs - now % _minuteMs;
        minuteTimer.start();
        _updateUptime();
    }

    // Shows the uptime now and arms the timer for the next whole minute of it. With no boot_at the
    // uptime is unknown and nothing is armed. A boot_at in the future shows 0m and waits for it.
    function _updateUptime() {
        uptimeTimer.stop();
        const now = Date.now();
        const elapsed = B.uptime(_bootAt, now);
        _uptime = B.uptimeText(elapsed);
        if (elapsed === null)
            return;
        uptimeTimer.interval = elapsed < 0 ? -elapsed : _minuteMs - elapsed % _minuteMs;
        uptimeTimer.start();
    }

    Timer {
        id: minuteTimer
        repeat: false
        onTriggered: root._tick()
    }

    Timer {
        id: uptimeTimer
        repeat: false
        onTriggered: root._updateUptime()
    }

    Component.onCompleted: _tick()
}
