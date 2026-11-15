pragma Singleton
pragma ComponentBehavior: Bound

import "audio.js" as A
import "backend.js" as B
import qs.modules.common
import QtQuick
import Quickshell

/**
 * The default sink and source, the device and stream lists and the volume OSD, a thin client of
 * axon's `audio` topic (section 31, "Audio"). axon owns the PipeWire connection, the volume
 * protection and the instant of the last change that shows the OSD (`osd_at`); this keeps the names
 * the widgets bind, the call functions and a one-shot timer that hides the OSD after
 * Config.options.osd.timeout.
 *
 * While the topic is ok with `sink: null` (the default names no present node, or one without a
 * readable volume), the last volume, mute and description stay and `ready` reads false; nothing is
 * filled with 0. The same holds for `source`.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "audio"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool ready: topic.status === "ok" && _dataReady && _sinkPresent
    // PipeWire is down: the engine keeps the last data and retries.
    readonly property bool unavailable: topic.status === "stale" && topic.errorCode === "source_down"
    // nodeName is the topic's sink and source `id`, the PipeWire node.name. A property cannot be named
    // `id`: qmlformat refuses the file.
    readonly property QtObject sink: QtObject {
        readonly property string nodeName: root._sinkId
        readonly property var description: root._sinkDescription // null until known
        readonly property bool ready: root.topic.status === "ok" && root._dataReady && root._sinkPresent
        readonly property QtObject audio: QtObject {
            readonly property real volume: root._sinkVolume / 100 // the UI (cube-root) scale, 0..1
            readonly property bool muted: root._sinkMuted
        }
    }
    readonly property QtObject source: QtObject {
        readonly property string nodeName: root._sourceId
        readonly property var description: root._sourceDescription // null until known
        readonly property bool ready: root.topic.status === "ok" && root._dataReady && root._sourcePresent
        readonly property QtObject audio: QtObject {
            readonly property real volume: root._sourceVolume / 100
            readonly property bool muted: root._sourceMuted
        }
    }
    readonly property string materialSymbol: A.materialSymbol(ready, unavailable, _sinkMuted, _sinkVolume / 100)
    readonly property int volumePercent: _sinkVolume
    readonly property bool isMuted: ready ? _sinkMuted : false // AU-2: never muted while not ready
    readonly property var sinks: _sinks // audio.js deviceRow
    readonly property var sources: _sources // audio.js deviceRow
    readonly property var streams: _streams // audio.js streamRow
    readonly property double osdAt: _osdAt // 0 before the first change in this daemon instance
    readonly property string osdReason: _osdReason // the protection reason of the current OSD run, or ""
    readonly property string osdMessage: A.protectionText(_osdReason)
    readonly property bool osdVisible: _osdVisible

    // The wall clock in ms, read through this one property so a test can drive it.
    property var wallClock: Date.now

    property bool _dataReady: false
    property bool _sinkPresent: false
    property string _sinkId: ""
    property var _sinkDescription: null
    property int _sinkVolume: 0
    property bool _sinkMuted: false
    property bool _sourcePresent: false
    property string _sourceId: ""
    property var _sourceDescription: null
    property int _sourceVolume: 0
    property bool _sourceMuted: false
    property var _sinks: []
    property var _sinkIndex: ({}) // the mapped rows by id, which reuseRows keeps identical while the rev holds
    property var _sources: []
    property var _sourceIndex: ({})
    property var _streams: []
    property var _streamIndex: ({})
    property var _lastTrigger: null
    property double _osdAt: 0
    property double _osdRunStart: 0
    property double _osdHiddenAt: 0
    property string _osdReason: ""
    property bool _osdVisible: false

    function setVolumePercent(p) {
        Backend.call("audio.set_volume", {
            target: "sink",
            percent: Math.round(p)
        });
    }

    function nudgeVolume(delta) {
        Backend.call("audio.nudge", {
            target: "sink",
            delta: delta
        });
    }

    function setVolumeLinear(v) { // v on the slider's 0..1 scale
        Backend.call("audio.set_volume", {
            target: "sink",
            percent: Math.round(v * 100)
        });
    }

    function toggleMute() {
        Backend.call("audio.toggle_mute", {
            target: "sink"
        });
    }

    function setMuted(m) {
        Backend.call("audio.set_mute", {
            target: "sink",
            muted: !!m
        });
    }

    function toggleSourceMute() {
        Backend.call("audio.toggle_mute", {
            target: "source"
        });
    }

    // kind is "sink" or "source", id a row id of sinks or sources.
    function setDefault(kind, id) {
        Backend.call("audio.set_default", {
            kind: kind,
            id: id
        });
    }

    function setStreamVolume(id, v) { // v on the slider's 0..1 scale
        Backend.call("audio.set_volume", {
            target: "stream",
            id: id,
            percent: Math.round(v * 100)
        });
    }

    // Hides the OSD until the next change that shows it.
    function hideOsd() {
        _osdHiddenAt = _osdAt;
        _armOsdTimer();
    }

    // The only place osdVisible is set, called when osd_at changes, from hideOsd() and when the
    // one-shot timer fires: reads the wall clock, then arms the timer for the deadline, or stops it
    // when the OSD is hidden. A deadline already passed hides the OSD at once.
    function _armOsdTimer() {
        const now = wallClock();
        const state = A.osdState(_osdAt, Config.options.osd.timeout, now, _osdRunStart, _osdHiddenAt, _lastTrigger);
        _osdRunStart = state.runStart;
        _osdReason = state.reason;
        _osdVisible = state.visible;
        if (!state.visible) {
            osdTimer.stop();
            return;
        }
        osdTimer.interval = Math.max(1, state.deadline - now);
        osdTimer.restart();
    }

    function _apply(data) {
        _dataReady = data.ready === true;
        const sink = data.sink;
        _sinkPresent = sink !== null;
        if (_sinkPresent) {
            _sinkId = sink.id;
            _sinkDescription = sink.description;
            _sinkVolume = sink.volume_percent;
            _sinkMuted = sink.muted;
        }
        const source = data.source;
        _sourcePresent = source !== null;
        if (_sourcePresent) {
            _sourceId = source.id;
            _sourceDescription = source.description;
            _sourceVolume = source.volume_percent;
            _sourceMuted = source.muted;
        }
        const sinkRows = B.reuseRows(_sinkIndex, data.sinks.map(A.deviceRow));
        _sinkIndex = sinkRows.index;
        _sinks = sinkRows.rows;
        const sourceRows = B.reuseRows(_sourceIndex, data.sources.map(A.deviceRow));
        _sourceIndex = sourceRows.index;
        _sources = sourceRows.rows;
        const streamRows = B.reuseRows(_streamIndex, data.streams.map(A.streamRow));
        _streamIndex = streamRows.index;
        _streams = streamRows.rows;
        _lastTrigger = data.protection.last_trigger;
        const osdAt = data.osd_at ?? 0;
        if (osdAt !== _osdAt) {
            // Settle the OSD run of the old osd_at at this instant first, so a run that ended while
            // the timer had not fired yet (after a suspend) does not carry over into the new one.
            _armOsdTimer();
            _osdAt = osdAt;
            _armOsdTimer();
        }
    }

    // One-shot, armed only by _armOsdTimer, never by a binding: a binding on `running` or `interval`
    // does not re-evaluate when the deadline stays the same, and a stopped Timer is not restarted by
    // a new interval (the notify popup rule).
    Timer {
        id: osdTimer
        repeat: false
        onTriggered: root._armOsdTimer()
    }
}
