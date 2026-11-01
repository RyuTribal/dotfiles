pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import "hypr.js" as H
import QtQuick
import Quickshell

/**
 * Hyprland's monitors, workspaces and windows, Game Mode and zoom, a thin client of axon's `hypr`
 * topic (section 30, "hypr"). A window id is the row id axon publishes ("stableid:..."), and a
 * monitor row's id is the monitor's name. Every lookup gives the row or null, never undefined.
 * Workspaces are looked up by number (`ws_id`). A special or named workspace has no number (ruling
 * D1), so `workspace()` and `iconWindow()` never find one; a monitor row names its shown special
 * workspace in `special_workspace`.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "hypr"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool available: topic.status === "ok"
    readonly property string focusedMonitorName: _focusedMonitorName // "" until the first snapshot
    readonly property var monitors: _monitors // [{ id (the name), rev, x, y, width, height, scale, active_workspace, special_workspace, ... }]
    readonly property var workspaces: _workspaces // [{ id (Hyprland's address), rev, ws_id, name, monitor, windows, has_fullscreen, icon_window }]
    readonly property var windows: _windows // [{ id, rev, address, class, title, pid, workspace, workspace_name, monitor, at, size, ... }]
    readonly property var activeWindow: _activeWindow // the active window's row, or null
    readonly property bool gameMode: _gameMode
    readonly property real zoom: _zoom
    // [{ id, rev, monitor, level, namespace, x, y, w, h }], or null while no `hypr.geometry`
    // subscriber arms axon's geometry check; window rows carry `at` and `size` only while it is armed.
    readonly property var layers: _layers

    property string _focusedMonitorName: ""
    property var _monitors: []
    property var _workspaces: []
    property var _windows: []
    property var _activeWindow: null
    property bool _gameMode: false
    property real _zoom: 1
    property var _layers: null
    // Rows by row id (reuseRows keeps these), and by the other keys the lookups use. None has a
    // prototype, so no id finds an inherited member.
    property var _monitorIndex: Object.create(null)
    property var _workspaceIndex: Object.create(null)
    property var _windowIndex: Object.create(null)
    property var _layerIndex: Object.create(null)
    property var _workspaceByNumber: Object.create(null)
    property var _windowByAddress: Object.create(null)

    function monitorFor(screen) {
        return screen ? (_monitorIndex[screen.name] ?? null) : null;
    }

    // A null number finds nothing: indexBy leaves out the workspaces without one.
    function workspace(number) {
        return _workspaceByNumber[number] ?? null;
    }

    function window(id) {
        return _windowIndex[id] ?? null;
    }

    function windowByAddress(address) {
        return _windowByAddress[address] ?? null;
    }

    // The window that stands for workspace `number` (axon's `icon_window`), or null.
    function iconWindow(number) {
        const iconId = workspace(number)?.icon_window ?? null;
        return iconId === null ? null : window(iconId);
    }

    function focusWorkspace(number) {
        Backend.call("hypr.focus_workspace", {
            id: number
        });
    }

    function focusWorkspaceRelative(offset) {
        Backend.call("hypr.focus_workspace", {
            relative: offset
        });
    }

    function toggleSpecial(name) {
        Backend.call("hypr.toggle_special", {
            name: name
        });
    }

    // The three window calls send an id taken from the data, so they wait for the topic to be synced
    // (section 20): until the subscription's first frame the id may be from an older snapshot. hypr
    // is always active, so this skips a call only while axon is down, where the call would fail with
    // `disconnected` anyway, or between a reconnect and that first frame.
    function focusWindow(id) {
        if (!topic.synced)
            return;
        Backend.call("hypr.focus_window", {
            window: id
        });
    }

    function moveWindow(id, workspaceNumber) {
        if (!topic.synced)
            return;
        Backend.call("hypr.move_window", {
            window: id,
            workspace: workspaceNumber,
            follow: false
        });
    }

    function closeWindow(id) {
        if (!topic.synced)
            return;
        Backend.call("hypr.close_window", {
            window: id
        });
    }

    // `replied` gets the reply, whatever it says (P21: the sidebar's reload button reloads the shell
    // in it, so that reload cannot drop the request before it is written).
    function reload(replied) {
        Backend.call("hypr.reload", {
            token: "user"
        }, replied);
    }

    function setGameMode(on) {
        Backend.call("hypr.set_game_mode", {
            on: on
        });
    }

    // Lists and their indexes change only when a row did. A frame carries the whole state, and most
    // frames change one window row (a title) or none, so the monitor and workspace lists usually stay
    // the same object and nothing bound to them runs again.
    function _apply(data) {
        _focusedMonitorName = data.focused_monitor ?? "";
        _gameMode = data.game_mode === true;
        _zoom = data.zoom;

        const reusedWindows = B.reuseRows(_windowIndex, data.windows);
        if (!_sameRows(reusedWindows.rows, _windows)) {
            _windowIndex = reusedWindows.index;
            _windowByAddress = H.indexBy(reusedWindows.rows, "address");
            _windows = reusedWindows.rows;
        }
        // The snapshot signal hands the data over as a QVariantMap, and a row written alone to a var
        // property is stored as a copy, so every write would notify. Write only when the row changed.
        const active = data.active_window === null ? null : window(data.active_window);
        if (active?.id !== _activeWindow?.id || active?.rev !== _activeWindow?.rev)
            _activeWindow = active;

        const reusedWorkspaces = B.reuseRows(_workspaceIndex, data.workspaces);
        if (!_sameRows(reusedWorkspaces.rows, _workspaces)) {
            _workspaceIndex = reusedWorkspaces.index;
            _workspaceByNumber = H.indexBy(reusedWorkspaces.rows, "ws_id");
            _workspaces = reusedWorkspaces.rows;
        }

        const reusedMonitors = B.reuseRows(_monitorIndex, data.monitors);
        if (!_sameRows(reusedMonitors.rows, _monitors)) {
            _monitorIndex = reusedMonitors.index;
            _monitors = reusedMonitors.rows;
        }

        if (data.layers === null) {
            _layerIndex = Object.create(null);
            _layers = null;
        } else {
            const reusedLayers = B.reuseRows(_layerIndex, data.layers);
            if (_layers === null || !_sameRows(reusedLayers.rows, _layers)) {
                _layerIndex = reusedLayers.index;
                _layers = reusedLayers.rows;
            }
        }
    }

    // True when reuseRows kept every row of `current`, in the same order.
    function _sameRows(rows, current) {
        return rows.length === current.length && rows.every((row, index) => row === current[index]);
    }
}
