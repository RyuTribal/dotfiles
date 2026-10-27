import QtQuick
import QtTest

// services/Backend.qml, loaded in place against the stand-in Quickshell and Quickshell.Io of fakes/.
// The test plays axond through the fake Socket. Run it through backend_tests.sh, or with
// qmltestrunner -import tests/unit/fakes.
TestCase {
    id: tc
    name: "BackendConnection"

    property Component backendComponent: null

    readonly property string stoppingText: "shutting_down: axond is stopping"

    Component {
        id: topicComponent
        // Stands in for a BackendTopic: records the revs it was given, and throws when failing is set.
        QtObject {
            property string name
            property bool failing: false
            property var revs: []

            function _apply(frame) {
                revs.push(frame.rev);
                if (failing)
                    throw new Error("handler broke");
            }

            function _unknown() {
            }
        }
    }

    function initTestCase() {
        backendComponent = Qt.createComponent(Qt.resolvedUrl("../../services/Backend.qml"));
        compare(backendComponent.status, Component.Ready, backendComponent.errorString());
    }

    // What the Backend under test would have logged as errors, in order. Its sink is swapped so the
    // run prints nothing for the expected ones and the tests can assert that each text goes out once.
    property var logged: []

    function newBackend() {
        logged = [];
        const backend = createTemporaryObject(backendComponent, tc, {
            _writeError: text => tc.logged.push(text)
        });
        verify(backend !== null);
        return backend;
    }

    function socketOf(backend) {
        for (let index = 0; index < backend.data.length; index++) {
            if (backend.data[index].connectRequests !== undefined)
                return backend.data[index];
        }
        fail("Backend has no Socket");
    }

    // Connects, answers the client's hello and returns the fake socket.
    function connectUp(backend) {
        const socket = socketOf(backend);
        tryCompare(socket, "connectRequests", 1, 1000);
        socket.accept();
        compare(socket.sentFrames()[0], {
            t: "hello",
            proto: 1,
            client: "ii"
        });
        sendHello(socket);
        verify(backend.up);
        return socket;
    }

    function sendHello(socket) {
        socket.serverSends({
            t: "hello",
            proto: 1,
            server: "axond test",
            instance: "i1"
        });
    }

    function sendStopping(socket) {
        socket.serverSends({
            t: "error",
            code: "shutting_down",
            message: "axond is stopping"
        });
    }

    function snapshotFrame(topic, rev) {
        return {
            t: "snapshot",
            topic: topic,
            rev: rev,
            status: "ok",
            source: "live",
            observed_at: 1000,
            data: {
                rev: rev
            }
        };
    }

    function test_connectFailureNamesThePathAndBacksOff() {
        const backend = newBackend();
        const socket = socketOf(backend);
        tryCompare(socket, "connectRequests", 1, 1000);
        socket.refuse();
        compare(backend.lastError, "cannot connect to /run/user/test/axon.sock (socket error 0)");
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.refuse();
        wait(300);
        compare(socket.connectRequests, 2, "the second wait is 500 ms");
        tryCompare(socket, "connectRequests", 3, 1000);
    }

    // Review F1: the drop ends the attempt once, whichever of error and disconnect comes first.
    function test_dropWithoutErrorFrame_data() {
        return [
            {
                tag: "error first",
                errorFirst: true
            },
            {
                tag: "disconnect first",
                errorFirst: false
            }
        ];
    }

    function test_dropWithoutErrorFrame(data) {
        const backend = newBackend();
        const socket = connectUp(backend);
        socket.peerClose(data.errorFirst);
        verify(!backend.up);
        compare(backend.lastError, "the connection to axon closed");
        tryCompare(socket, "connectRequests", 2, 1000);
        wait(600);
        compare(socket.connectRequests, 2, "exactly one reconnect");
    }

    function test_serverErrorOutlivesTheDrop_data() {
        return test_dropWithoutErrorFrame_data();
    }

    function test_serverErrorOutlivesTheDrop(data) {
        const backend = newBackend();
        const socket = connectUp(backend);
        sendStopping(socket);
        socket.peerClose(data.errorFirst);
        compare(backend.lastError, stoppingText);
        compare(logged, [stoppingText], "the server's reason is logged once");
        tryCompare(socket, "connectRequests", 2, 1000);
        wait(600);
        compare(socket.connectRequests, 2, "exactly one reconnect");
        socket.refuse();
        compare(backend.lastError, stoppingText, "a connect failure keeps the server's reason");
        compare(logged.filter(text => text === stoppingText).length, 1, "still logged once");
        tryCompare(socket, "connectRequests", 3, 1000);
    }

    function test_helloClearsTheServerError() {
        const backend = newBackend();
        const socket = connectUp(backend);
        sendStopping(socket);
        socket.peerClose(true);
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.accept();
        sendHello(socket);
        compare(backend.lastError, "");
        socket.peerClose(true);
        compare(backend.lastError, "the connection to axon closed");
        tryCompare(socket, "connectRequests", 3, 1000);
        socket.refuse();
        compare(backend.lastError, "cannot connect to /run/user/test/axon.sock (socket error 0)");
    }

    // Review F3: one topic whose _apply throws must not keep the frame from the others.
    function test_throwingTopicDoesNotStopTheOthers() {
        const backend = newBackend();
        const socket = connectUp(backend);
        const broken = createTemporaryObject(topicComponent, tc, {
            name: "system",
            failing: true
        });
        const healthy = createTemporaryObject(topicComponent, tc, {
            name: "system"
        });
        backend._attach(broken);
        backend._attach(healthy);
        ignoreWarning(/axon: topic system failed to apply a snapshot: Error: handler broke/);
        failOnWarning(/axon: topic system failed/); // logged once per topic
        socket.serverSends(snapshotFrame("system", 1));
        socket.serverSends(snapshotFrame("system", 2));
        compare(healthy.revs, [1, 2]);
        compare(broken.revs, [1, 2]);
    }

    // F3, the attach path: a throw from the cached frame must not stop the subscription.
    function test_attachSubscribesPastAThrowingTopic() {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "system"
        });
        backend._attach(first);
        socket.serverSends(snapshotFrame("system", 1));
        socket.peerClose(true);
        backend._detach(first); // while down: the cached frame stays for the next attacher
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.accept();
        sendHello(socket);

        const broken = createTemporaryObject(topicComponent, tc, {
            name: "system",
            failing: true
        });
        ignoreWarning(/axon: topic system failed to apply a snapshot/);
        backend._attach(broken);
        compare(broken.revs, [1], "the cached frame was applied");
        const subscriptions = socket.sentFrames().filter(frame => frame.t === "sub");
        compare(subscriptions.length, 2, "the first attach and this one");
        compare(subscriptions[1].topics, ["system"]);
    }
}
