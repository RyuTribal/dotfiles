pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import "notifications.js" as N
import qs
import QtQuick
import Quickshell

/**
 * The notifications for the sidebar, the popups and the bar, a thin client of axon's `notify` engine
 * (section 31). axond owns the bus name, the history, the popup deadlines, Do Not Disturb and the
 * sounds; this holds the mapped rows, the per-app groups the widgets read, the sidebar rule, and a
 * one-shot timer that shows and ends popups at the instants the engine does not publish.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "notify"
        onSnapshot: data => root._apply(data)
    }

    readonly property var list: _list // mapped rows, newest first (notifications.js mapRow)
    // The rows showing as popups now: the sidebar hides them all (popupRows).
    readonly property var popupList: N.popupRows(_list, _popupNow, GlobalStates.sidebarRightOpen)
    readonly property bool silent: _dnd // Do Not Disturb; setSilent changes it
    readonly property bool serverOwned: _serverOwned // axond owns org.freedesktop.Notifications
    readonly property var latestTimeForApp: N.latestTimes(_list)
    readonly property var groupsByAppName: N.groupsForList(_list, latestTimeForApp)
    readonly property var popupGroupsByAppName: N.groupsForList(popupList, latestTimeForApp)
    readonly property var appNameList: N.appNameListForGroups(groupsByAppName)
    readonly property var popupAppNameList: N.appNameListForGroups(popupGroupsByAppName)

    // The wall clock in ms, read through this one property so a test can drive it. Backend.now ticks
    // once a second, which would show a new popup up to half a second after the engine's pop-time
    // sound (section 20, "Client-side extrapolation").
    property var wallClock: Date.now

    property var _list: []
    property var _rowIndex: ({}) // the mapped rows by id, which reuseRows keeps identical while the rev holds
    property bool _dnd: false
    property bool _serverOwned: false
    // The wall time popupList is judged at: the wall clock as last read, at each frame, each timer fire
    // and each Backend.now tick. Never held at an earlier high value, so a backward clock jump shows
    // the rows the engine stamps with the new time.
    property double _popupNow: 0
    property bool _firstSnapshotSeen: false

    // The only place _popupNow is set, called on every frame, every timer fire and every Backend.now
    // tick: reads the wall clock, then arms the timer for the next instant a popup starts or ends
    // (the engine publishes at neither), or stops it when there is none. A fire that read a time
    // before the deadline finds the deadline still ahead and arms the timer for what is left.
    function _armPopupTimer() {
        _popupNow = wallClock();
        const deadline = N.nextDeadline(_list, _popupNow);
        if (deadline < 0) {
            popupTimer.stop();
            return;
        }
        popupTimer.interval = Math.max(1, deadline - _popupNow);
        popupTimer.restart();
    }

    function _apply(data) {
        const items = data.items ?? [];
        const reused = B.reuseRows(_rowIndex, items.map(row => N.mapRow(row, name => Quickshell.iconPath(name))));
        _rowIndex = reused.index;
        _list = reused.rows;
        _dnd = data.dnd ?? false;
        _serverOwned = data.server?.owned ?? false;
        _armPopupTimer();
        if (!_firstSnapshotSeen) {
            _firstSnapshotSeen = true;
            _releaseHeldPopups(items);
        }
    }

    // A hold ends with the client that made it, so a popup the engine still lists as held belongs to
    // a shell that is gone: resume them all once, from the first snapshot (D17).
    function _releaseHeldPopups(items) {
        const heldIds = N.heldIds(items);
        if (heldIds.length > 0)
            holdPopups(heldIds, false);
    }

    function discardNotification(id) {
        Backend.call("notify.dismiss", {
            id: id
        });
    }

    function discardAllNotifications() {
        Backend.call("notify.dismiss_all", {});
    }

    // Ends every popup now.
    function timeoutAll() {
        Backend.call("notify.expire_popups", {});
    }

    function attemptInvokeAction(id, action) {
        Backend.call("notify.invoke_action", {
            id: id,
            action: action
        });
    }

    // Freezes the popups of `ids` while the pointer is over them; `held` false resumes them.
    function holdPopups(ids, held) {
        Backend.call("notify.hold_popups", {
            ids: ids,
            held: held
        });
    }

    function setSilent(on) {
        Backend.call("notify.set_dnd", {
            on: on
        });
    }

    // Every open and every close of the sidebar ends the popups, so notifications that arrived while
    // it was open never pop later (D21).
    Connections {
        target: GlobalStates

        function onSidebarRightOpenChanged() {
            root.timeoutAll();
        }
    }

    // The engine sends no frame at a popup's start or end, so after a wall-clock jump or a missed
    // fire nothing else would correct the popups: every Backend.now tick reads the wall clock again
    // and arms the timer from that reading (_armPopupTimer).
    Connections {
        target: Backend

        function onNowChanged() {
            root._armPopupTimer();
        }
    }

    // One-shot, armed only by _armPopupTimer, never by a binding: a binding on `running` or
    // `interval` does not re-evaluate when the deadline stays the same, which is the case after a
    // fire that read the clock before the deadline, and a stopped Timer is not restarted by a new
    // interval.
    Timer {
        id: popupTimer
        repeat: false
        onTriggered: root._armPopupTimer()
    }
}
