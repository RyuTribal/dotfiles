pragma Singleton
pragma ComponentBehavior: Bound

import QtQuick
import Quickshell
import Quickshell.Io

/**
 * The one connection to axon (section 20, "Backend.qml"). It says hello,
 * subscribes to every topic a BackendTopic wants, sends calls and matches
 * their replies, and reconnects with backoff whenever axon goes away. Each qs
 * process (the shell, settings, screenshot) loads its own copy and holds its
 * own connection, labelled by clientName.
 */
Singleton {
    id: root

    readonly property int proto: 1
    // The label axond logs for this connection. An entry file other than the
    // shell sets it in its Component.onCompleted: the first connect waits for
    // that through Qt.callLater.
    property string clientName: "ii"
    // AXON_SOCKET for a scratch shell against a dev daemon, else the live
    // socket. Empty when neither variable is set: then the client never
    // connects and lastError says why.
    readonly property string socketPath: {
        const devSocket = Quickshell.env("AXON_SOCKET");
        if (devSocket)
            return devSocket;
        const runtimeDir = Quickshell.env("XDG_RUNTIME_DIR");
        return runtimeDir ? runtimeDir + "/axon.sock" : "";
    }

    readonly property bool up: _up                    // a server hello arrived on the current connection
    readonly property double downSince: _downSince    // when the connection last dropped, else load time
    readonly property bool downShown: _downShown      // the bar's axon-down icon: no server hello for 3 s
    readonly property string instance: _instance     // the daemon instance of the latest hello
    readonly property string lastError: _lastError   // why axon is not up, for the axon-down icon's popup
    readonly property double now: _now                // ticks just after each second, for ages and countdowns

    property bool _up: false
    property double _downSince: Date.now()
    property bool _downShown: false
    property string _instance: ""
    property string _lastError: ""
    property double _now: Date.now()

    property var _topics: ({})          // topic name -> the BackendTopics attached to it
    property var _last: ({})            // topic name -> its last snapshot frame, for late attachers
    property var _pending: ({})         // request id -> { method, done, deadline }
    property var _failureLoggedAt: ({}) // "method code" -> when that failure was last logged
    property var _loggedErrors: ({})    // connection error texts already logged
    // Where _logOnce writes; tests swap it.
    property var _writeError: text => console.error("axon:", text)
    property var _applyFailureLogged: ({}) // topic names whose _apply failure was logged
    property double _nextId: 1
    property int _backoff: _minBackoffMs // wait before the next connect attempt
    property bool _closing: false       // we closed on purpose: ignore what is still buffered
    property bool _attemptEnded: true   // the last connect attempt ended and _down handled it
    property bool _lastErrorFromServer: false // _lastError came in an error frame; a connect failure keeps it

    readonly property int _minBackoffMs: 250
    readonly property int _maxBackoffMs: 5000
    readonly property int _callDeadlineMs: 12000 // the server's 10 s deadline plus slack

    // Sends a call. `done` gets the reply frame: { ok: true, result } or
    // { ok: false, error: { code, message, ... } }. While axon is down it gets
    // a "disconnected" error at once. Returns the request id, 0 when not sent.
    function call(method, args, done) {
        if (!_up) {
            _invoke(done, _clientFailure("disconnected", "axon is not connected"));
            return 0;
        }
        const id = _track(method, done);
        _send({ t: "call", id: id, method: method, args: args ?? {} });
        return id;
    }

    // A BackendTopic became active. The first one for a name subscribes it;
    // a later one gets the cached last frame at once.
    function _attach(topic) {
        const attached = _topics[topic.name] ?? [];
        if (attached.indexOf(topic) >= 0)
            return;
        attached.push(topic);
        _topics[topic.name] = attached;
        const last = _last[topic.name];
        if (last !== undefined)
            _deliver(topic, last);
        if (_up && attached.length === 1)
            _subscription("sub", [topic.name]);
    }

    // A BackendTopic became inactive or was destroyed. The last one for a
    // name unsubscribes it.
    function _detach(topic) {
        const attached = _topics[topic.name] ?? [];
        if (attached.indexOf(topic) < 0)
            return;
        const remaining = attached.filter(other => other !== topic);
        _topics[topic.name] = remaining;
        if (_up && remaining.length === 0) {
            delete _last[topic.name];
            _subscription("unsub", [topic.name]);
        }
    }

    function _connect() {
        _attemptEnded = false;
        sock.connected = true;
    }

    function _send(frame) {
        sock.write(JSON.stringify(frame) + "\n");
        sock.flush();
    }

    function _track(method, done) {
        const id = _nextId++;
        _pending[id] = { method: method, done: done ?? null, deadline: Date.now() + _callDeadlineMs };
        return id;
    }

    function _subscription(kind, names) { // kind is "sub" or "unsub"
        const id = _track(kind, root._onSubscribed);
        _send({ t: kind, id: id, topics: names });
    }

    function _onSubscribed(reply) {
        for (const name of reply.result?.unknown ?? []) {
            console.error("axon: unknown topic", name);
            for (const topic of _topics[name] ?? [])
                topic._unknown();
        }
    }

    // Exactly once per request: its reply, its client timeout, or the drop.
    function _finish(id, reply) {
        const request = _pending[id];
        if (request === undefined)
            return; // a late reply after a client timeout
        delete _pending[id];
        if (!reply.ok)
            _logFailure(request.method, reply.error);
        _invoke(request.done, reply);
    }

    // A throwing callback must not stop the loop that called it: _down would
    // otherwise never reach its retry.
    function _invoke(done, reply) {
        if (!done)
            return;
        try {
            done(reply);
        } catch (error) {
            console.error("axon: reply callback failed:", error);
        }
    }

    function _clientFailure(code, message) {
        return { ok: false, error: { code: code, message: message } };
    }

    function _onLine(line) {
        if (_closing)
            return;
        let frame = null;
        try {
            frame = JSON.parse(line);
        } catch (error) {
            frame = null;
        }
        if (frame === null || typeof frame !== "object" || Array.isArray(frame)) {
            // A daemon bug. Closing and reconnecting resubscribes, which is a full resync.
            console.error("axon: unparseable frame", line.slice(0, 200));
            _close();
            return;
        }
        if (frame.t === "snapshot")
            _onSnapshot(frame);
        else if (frame.t === "reply")
            _finish(frame.id, frame);
        else if (frame.t === "hello")
            _onHello(frame);
        else if (frame.t === "error")
            _onError(frame);
        // Any other "t" comes from a newer daemon and is ignored (section 20, "Framing").
    }

    function _onSnapshot(frame) {
        _last[frame.topic] = frame;
        for (const topic of _topics[frame.topic] ?? [])
            _deliver(topic, frame);
    }

    // A topic whose _apply throws must not keep the frame from the other
    // BackendTopics of its name, nor stop _attach before it subscribes.
    // Logged once per topic name. A throwing snapshot handler does not get
    // here: QML reports a signal handler's exception itself and goes on.
    function _deliver(topic, frame) {
        try {
            topic._apply(frame);
        } catch (error) {
            if (_applyFailureLogged[topic.name] === true)
                return;
            _applyFailureLogged[topic.name] = true;
            console.warn("axon: topic", topic.name, "failed to apply a snapshot:", error);
        }
    }

    function _onHello(frame) {
        helloTimeout.stop();
        _backoff = _minBackoffMs;
        if (frame.instance !== _instance)
            _last = ({}); // a new daemon: every rev restarted, and each BackendTopic resets itself
        _instance = frame.instance;
        _lastError = "";
        _lastErrorFromServer = false;
        _up = true;
        _downShown = false;
        downDelay.stop();
        const wanted = Object.keys(_topics).filter(name => _topics[name].length > 0);
        if (wanted.length > 0)
            _subscription("sub", wanted);
    }

    function _onError(frame) { // a connection error: the server closes next
        _lastError = frame.code + ": " + frame.message;
        _lastErrorFromServer = true;
        _logOnce(_lastError);
        if (frame.code === "shutting_down")
            _backoff = _minBackoffMs; // a restart is expected
        else if (frame.code === "proto_mismatch")
            _backoff = _maxBackoffMs; // retries every 5 s until install.sh restarts a matching build
    }

    function _onConnected() {
        _closing = false;
        _send({ t: "hello", proto: proto, client: clientName });
        helloTimeout.restart();
    }

    function _close() {
        _closing = true;
        sock.connected = false;
    }

    // The connect attempt ended: the connection dropped or the connect failed.
    // Keeps every topic's last data, fails the pending calls with
    // "disconnected" and backs off. Runs once per attempt, whichever of the
    // socket's error and disconnect signals comes first.
    function _down() {
        if (_attemptEnded)
            return;
        _attemptEnded = true;
        helloTimeout.stop();
        if (_up) {
            _up = false;
            _downSince = Date.now();
            downDelay.restart();
        }
        if (_lastError === "")
            _lastError = "the connection to axon closed";
        const dropped = _pending;
        _pending = ({});
        for (const id in dropped)
            _invoke(dropped[id].done, _clientFailure("disconnected", "connection to axon lost"));
        _retry();
    }

    // onError while not connected: a failed connect, or the tail of a drop
    // that _down already handled.
    function _connectFailed(socketError) {
        if (_attemptEnded)
            return;
        if (!_lastErrorFromServer)
            _lastError = "cannot connect to " + socketPath + " (socket error " + socketError + ")";
        _down();
    }

    // Backoff from 250 ms doubling to 5 s, reset by a server hello. Retries
    // come only from this timer, never from inside onError.
    function _retry() {
        if (retry.running || socketPath === "")
            return;
        retry.interval = _backoff;
        _backoff = Math.min(_backoff * 2, _maxBackoffMs);
        retry.start();
    }

    function _expire() { // client-side call timeout, checked on every tick
        for (const id in _pending) {
            if (_pending[id].deadline < _now)
                _finish(id, _clientFailure("timeout", "no reply from axon in 12 s"));
        }
    }

    // Warns once per method and code per minute, never for "disconnected",
    // which the axon-down icon covers. The message stays out of the log
    // because it can repeat argument values.
    function _logFailure(method, error) {
        const code = error?.code ?? "unknown";
        if (code === "disconnected")
            return;
        const key = method + " " + code;
        const at = Date.now();
        const loggedAt = _failureLoggedAt[key];
        if (loggedAt !== undefined && at >= loggedAt && at - loggedAt < 60000)
            return;
        _failureLoggedAt[key] = at;
        console.warn("axon:", method, "failed:", code);
    }

    function _logOnce(text) { // errors once per distinct text
        if (_loggedErrors[text] === true)
            return;
        _loggedErrors[text] = true;
        _writeError(text);
    }

    Component.onCompleted: {
        if (socketPath === "") {
            _lastError = "XDG_RUNTIME_DIR is not set, so the axon socket cannot be found";
            console.error("axon:", _lastError);
            return;
        }
        if (Quickshell.env("AXON_SOCKET"))
            console.info("axon: using AXON_SOCKET", socketPath);
        Qt.callLater(root._connect);
    }

    Socket {
        id: sock
        path: root.socketPath
        connected: false
        parser: SplitParser {
            onRead: line => root._onLine(line)
        }
        onConnectionStateChanged: {
            if (connected)
                root._onConnected();
            else
                root._down();
        }
        // A failed connect emits only this, with no state change, and nothing
        // retries by itself. A peer close emits it next to the disconnect, in
        // either order: while still connected it is ignored, and after the
        // disconnect _down has already ended the attempt.
        onError: socketError => {
            if (!connected)
                root._connectFailed(socketError);
        }
    }

    Timer {
        id: retry
        onTriggered: root._connect()
    }

    Timer {
        id: helloTimeout
        interval: 5000
        onTriggered: {
            root._lastError = "axon accepted the connection but sent no hello within 5 s";
            root._lastErrorFromServer = false;
            root._close();
        }
    }

    // Runs from load, and again from every drop of an up connection.
    Timer {
        id: downDelay
        interval: 3000
        running: true
        onTriggered: root._downShown = true
    }

    Timer {
        id: tick
        running: true
        interval: 1000 - Date.now() % 1000 + 1
        onTriggered: {
            root._now = Date.now();
            root._expire();
            interval = 1000 - root._now % 1000 + 1;
            start();
        }
    }
}
