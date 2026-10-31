import QtQuick

/**
 * The envelope of one axon topic (section 20, "BackendTopic and the service
 * singletons"): status, source, age and error, and the interest switch. A
 * service holds one and takes `data` from the snapshot signal. Bind `active`
 * to visibility for an interest-driven topic. `name` is fixed for the life of
 * the object.
 */
QtObject {
    id: root

    required property string name
    property bool active: true

    readonly property string status: _status // "waiting" before the first snapshot, else the wire status
    readonly property string source: _source
    readonly property double observedAt: _observedAt
    readonly property string errorCode: _errorCode
    readonly property string errorMessage: _errorMessage
    readonly property string errorReason: _errorReason // "not_present": the widget hides
    readonly property bool hasData: _hasData
    readonly property var ops: _ops
    readonly property bool live: Backend.up && _status === "ok" && _source !== "persisted"
    // While axon is down, an ok topic was known true until the connection dropped.
    readonly property double knownAt: !Backend.up && _status === "ok"
        ? Math.max(_observedAt, Backend.downSince) : _observedAt
    readonly property double ageMs: _hasData ? Backend.now - knownAt : 0
    // The data is as new as axon's: true from the first frame of the current subscription, applied
    // or dropped as not newer; false while the topic is inactive or axon is down. A service that
    // asks axon about something taken from the data, such as a node id, waits for it.
    readonly property bool synced: _synced

    signal snapshot(var data)
    signal reset() // a new daemon instance: drop lazy pages, op ids and node ids

    property string _status: "waiting"
    property string _source: ""
    property double _observedAt: 0
    property string _errorCode: ""
    property string _errorMessage: ""
    property string _errorReason: ""
    property bool _hasData: false
    property var _ops: []
    property double _rev: 0
    property string _instance: "" // the daemon instance _rev counts in
    property bool _synced: false
    property bool _completed: false
    readonly property bool _backendUp: Backend.up

    // Called by Backend with every snapshot frame of this topic. A frame whose
    // rev is not newer than the last one applied from the same instance is
    // dropped. A data-null frame updates the envelope and keeps the data.
    function _apply(frame) {
        if (_instance !== Backend.instance) {
            // Also reached by a topic that was inactive when the daemon restarted.
            const restarted = _instance !== "";
            _instance = Backend.instance;
            _rev = 0;
            if (restarted)
                reset();
        }
        if (frame.rev > _rev) {
            _rev = frame.rev;
            _status = frame.status;
            _source = frame.source;
            _observedAt = frame.observed_at ?? 0;
            _errorCode = frame.error?.code ?? "";
            _errorMessage = frame.error?.message ?? "";
            _errorReason = frame.error?.details?.reason ?? "";
            _ops = frame.ops ?? [];
            if (frame.data !== null && frame.data !== undefined) {
                snapshot(frame.data);
                _hasData = true; // after the service applied the data, so hasData never shows its empty defaults
            }
        }
        // Last, so a service sees synced only after it took the data. A frame from Backend's cache,
        // given while axon is down, leaves the topic unsynced.
        _synced = Backend.up;
    }

    function _unknown() { // the daemon listed this topic as unknown
        _status = "error";
        _errorCode = "unknown_topic";
        _errorMessage = "axon does not know topic " + name;
        _errorReason = "";
    }

    // Change handlers of bound properties run during creation, before every
    // property is set, so attaching waits for Component.onCompleted.
    onActiveChanged: {
        if (!active)
            _synced = false;
        if (!_completed)
            return;
        if (active)
            Backend._attach(root);
        else
            Backend._detach(root);
    }

    on_BackendUpChanged: {
        if (!_backendUp)
            _synced = false;
    }

    Component.onCompleted: {
        _completed = true;
        if (active)
            Backend._attach(root);
    }

    Component.onDestruction: Backend._detach(root)
}
