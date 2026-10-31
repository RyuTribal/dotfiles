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

    function sendHello(socket, instance) {
        socket.serverSends({
            t: "hello",
            proto: 1,
            server: "axond test",
            instance: instance ?? "i1"
        });
    }

    function sendStopping(socket) {
        socket.serverSends({
            t: "error",
            code: "shutting_down",
            message: "axond is stopping"
        });
    }

    // The unsub frames the client wrote, in order.
    function unsubsSent(socket) {
        return socket.sentFrames().filter(frame => frame.t === "unsub");
    }

    function sendUnsubReply(socket, unsub) {
        socket.serverSends({
            t: "reply",
            id: unsub.id,
            ok: true,
            result: {
                unsubscribed: unsub.topics
            }
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

    // F3, the attach path: a throw from the cached frame must not stop the attach. While axon is down
    // a late attacher still gets the frame from before the drop, and the next hello subscribes it.
    function test_attachPastAThrowingTopic() {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "system"
        });
        backend._attach(first);
        socket.serverSends(snapshotFrame("system", 1));
        socket.peerClose(true);
        backend._detach(first); // while down: the cached frame stays for the next attacher

        const broken = createTemporaryObject(topicComponent, tc, {
            name: "system",
            failing: true
        });
        ignoreWarning(/axon: topic system failed to apply a snapshot/);
        backend._attach(broken);
        compare(broken.revs, [1], "the cached frame was applied");
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.accept();
        sendHello(socket);
        const subscriptions = socket.sentFrames().filter(frame => frame.t === "sub");
        compare(subscriptions.length, 2, "the first attach and the hello");
        compare(subscriptions[1].topics, ["system"]);
    }

    function test_everyHelloClearsTheFrameCache_data() {
        return [
            {
                tag: "same instance",
                instance: "i1"
            },
            {
                tag: "new instance",
                instance: "i2"
            }
        ];
    }

    // Another client may have changed a topic while the connection was down, also on the same
    // instance. So an attacher after the hello gets no frame from before the drop; it waits for the
    // frame of the subscription that hello sent.
    function test_everyHelloClearsTheFrameCache(data) {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "system"
        });
        backend._attach(first);
        socket.serverSends(snapshotFrame("system", 1));
        socket.peerClose(true);
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.accept();
        sendHello(socket, data.instance);

        const late = createTemporaryObject(topicComponent, tc, {
            name: "system"
        });
        backend._attach(late);
        compare(late.revs, [], "nothing from before the drop");
        socket.serverSends(snapshotFrame("system", 2));
        compare(late.revs, [2]);
        compare(first.revs, [1, 2]);
    }

    function test_noFrameFromAnEndedSubscription_data() {
        return [
            {
                tag: "attach again after the old frame came",
                attachFirst: false
            },
            {
                tag: "attach again before the old frame came",
                attachFirst: true
            }
        ];
    }

    // axon writes the frames it had due before it reads an unsub, and they reach the client ahead of
    // the unsub reply (section 20, "Ordering guarantees" 1 and 4). Such a frame is of the subscription
    // that unsub ended, so neither the cache nor a new attacher gets it: an attacher's first frame is
    // of its own subscription, which comes after the unsub reply.
    function test_noFrameFromAnEndedSubscription(data) {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "sweep"
        });
        backend._attach(first);
        socket.serverSends(snapshotFrame("sweep", 1));
        backend._detach(first);
        const unsub = unsubsSent(socket)[0];
        compare(unsub.topics, ["sweep"]);

        const next = createTemporaryObject(topicComponent, tc, {
            name: "sweep"
        });
        if (data.attachFirst)
            backend._attach(next);
        socket.serverSends(snapshotFrame("sweep", 2)); // written before axon read the unsub
        if (!data.attachFirst)
            backend._attach(next);
        compare(next.revs, [], "no frame of the ended subscription");
        sendUnsubReply(socket, unsub);
        socket.serverSends(snapshotFrame("sweep", 3)); // the first frame of next's subscription
        compare(next.revs, [3]);
        compare(first.revs, [1]);
    }

    // Two unsubs in flight: the subscription between them ended too, so its frame, which comes after
    // the first unsub's reply, is dropped as well.
    function test_onlyTheLatestUnsubEndsTheWait() {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "sweep"
        });
        backend._attach(first);
        backend._detach(first);
        backend._attach(first);
        backend._detach(first);
        const unsubs = unsubsSent(socket);
        compare(unsubs.length, 2);

        const next = createTemporaryObject(topicComponent, tc, {
            name: "sweep"
        });
        backend._attach(next);
        sendUnsubReply(socket, unsubs[0]);
        socket.serverSends(snapshotFrame("sweep", 1)); // of the subscription the second unsub ended
        compare(next.revs, [], "no frame of the ended subscription");
        sendUnsubReply(socket, unsubs[1]);
        socket.serverSends(snapshotFrame("sweep", 2));
        compare(next.revs, [2]);
    }

    // An unsub whose reply never came, since the connection dropped, stops dropping the topic's
    // frames: the next connection's subscription delivers them.
    function test_theDropEndsTheWait() {
        const backend = newBackend();
        const socket = connectUp(backend);
        const first = createTemporaryObject(topicComponent, tc, {
            name: "sweep"
        });
        backend._attach(first);
        backend._detach(first);
        compare(unsubsSent(socket).length, 1);
        socket.peerClose(true);
        tryCompare(socket, "connectRequests", 2, 1000);
        socket.accept();
        sendHello(socket);

        backend._attach(first);
        socket.serverSends(snapshotFrame("sweep", 1));
        compare(first.revs, [1]);
    }
}
