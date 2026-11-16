pragma ComponentBehavior: Bound

import "osd.js" as O
import qs.modules.common
import QtQuick

/**
 * How long an OSD stays on screen, for a facade whose engine publishes `osd_at`, the instant of the
 * last change that shows the OSD (section 20, the one-shot OSD timer). The facade calls moveTo() when
 * its `osd_at` changes and hide() when the pointer dismisses the OSD; `visible` holds for
 * Config.options.osd.timeout after `osdAt`. Audio and Brightness each hold one.
 */
QtObject {
    id: root

    // The wall clock in ms, read through this one property so a test can drive it.
    property var wallClock: Date.now
    // The audio protection's last trigger {reason, at}, or null: its reason shows inside the run.
    property var lastTrigger: null

    readonly property bool visible: _visible
    readonly property string reason: _reason // the protection reason of the current run, or ""
    readonly property double osdAt: _osdAt // 0 before the first change

    property double _osdAt: 0
    property double _runStart: 0
    property double _hiddenAt: 0
    property string _reason: ""
    property bool _visible: false

    // One-shot, armed only by _arm(), never by a binding: a binding on `running` or `interval` does not
    // re-evaluate when the deadline stays the same, and a stopped Timer is not restarted by a new
    // interval (the notify popup rule).
    readonly property Timer _timer: Timer {
        repeat: false
        onTriggered: root._arm()
    }

    // Takes a new osd_at. The run of the old one is settled at this instant first, so a run that
    // ended while the timer had not fired yet (after a suspend) does not carry over into the new one.
    function moveTo(osdAt) {
        _arm();
        _osdAt = osdAt;
        _arm();
    }

    // Hides the OSD until the next change that shows it.
    function hide() {
        _hiddenAt = _osdAt;
        _arm();
    }

    // The only place `visible` is set, called from moveTo(), hide() and when the one-shot timer fires:
    // reads the wall clock, then arms the timer for the deadline, or stops it when the OSD is hidden. A
    // deadline already passed hides the OSD at once.
    function _arm() {
        const now = wallClock();
        const state = O.osdState(_osdAt, Config.options.osd.timeout, now, _runStart, _hiddenAt, lastTrigger);
        _runStart = state.runStart;
        _reason = state.reason;
        _visible = state.visible;
        if (!state.visible) {
            _timer.stop();
            return;
        }
        _timer.interval = Math.max(1, state.deadline - now);
        _timer.restart();
    }
}
