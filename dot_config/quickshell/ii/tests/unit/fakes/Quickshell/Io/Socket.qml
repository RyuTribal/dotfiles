import QtQuick

// Stands in for Quickshell's Socket in tests. Setting `connected` is a request, as on the real Socket:
// a connect request leaves it down until the test calls accept() or refuse(), and a close request
// disconnects at once, as QLocalSocket does when nothing is left to write. The test plays the server.
QtObject {
    id: socket

    property string path
    property QtObject parser
    property bool connected: false
    property bool open: false     // the fake connection is up
    property int connectRequests: 0
    property int closeRequests: 0
    property var written: []      // every string written, in order

    signal error(int error)
    signal connectionStateChanged

    readonly property int connectionRefusedError: 0 // QLocalSocket::ConnectionRefusedError
    readonly property int peerClosedError: 1        // QLocalSocket::PeerClosedError

    onConnectedChanged: {
        if (connected === open)
            return; // a state change this fake made itself
        if (connected) {
            connectRequests++;
            connected = false;
        } else {
            closeRequests++;
            drop();
        }
    }

    function write(data) {
        written.push(data);
    }

    function flush() {
    }

    function accept() {
        open = true;
        connected = true;
        connectionStateChanged();
    }

    function refuse() {
        error(connectionRefusedError);
    }

    function drop() {
        open = false;
        connected = false;
        connectionStateChanged();
    }

    // The server closes its end. Qt emits the error first, while still connected, then the disconnect
    // (VERIFIED in a real qs, gate-results.md check 8: `probe.error type=number value=1 connected=true`,
    // then `probe.connected false`); errorFirst false plays the other order. The error argument is a
    // plain number, as here (`int error`).
    function peerClose(errorFirst) {
        if (errorFirst) {
            error(peerClosedError);
            drop();
        } else {
            drop();
            error(peerClosedError);
        }
    }

    // One frame from the server, as SplitParser hands it over.
    function serverSends(frame) {
        parser.read(JSON.stringify(frame));
    }

    // The frames the client wrote, parsed.
    function sentFrames() {
        return written.map(line => JSON.parse(line));
    }
}
