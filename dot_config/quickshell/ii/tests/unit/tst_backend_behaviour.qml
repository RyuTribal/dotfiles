import QtQuick
import QtTest
import ShellUnderTest

// BackendTopic.qml and BackendList.qml against the stand-in Backend of fakes/Backend.qml.
// Run it through backend_tests.sh, which builds the ShellUnderTest module.
TestCase {
    id: tc
    name: "BackendBehaviour"

    property string query: "a"
    property double rev: 1
    property bool panelOpen: true

    Component {
        id: listComponent
        BackendList {
            method: "testkit.list"
            collection: "items"
            args: ({
                    query: tc.query
                })
            collectionRev: tc.rev
            active: tc.panelOpen
        }
    }

    Component {
        id: topicComponent
        BackendTopic {
            name: "testkit"
            active: tc.panelOpen
        }
    }

    function rows(from, count, rowRev) {
        const out = [];
        for (let index = from; index < from + count; index++)
            out.push({
                id: String(index),
                rev: rowRev
            });
        return out;
    }

    function reply(callIndex, result) {
        Backend.calls[callIndex].done({
            ok: true,
            result: result
        });
    }

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 0;
        Backend.downSince = 0;
        tc.query = "a";
        tc.rev = 1;
        tc.panelOpen = true;
    }

    function test_firstPageOnceAtCreation() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "testkit.list");
        compare(Backend.calls[0].args, {
            query: "a",
            collection: "items",
            offset: 0,
            limit: 100
        });
        verify(list.loading);
    }

    function test_pagesAppendAndKeepObjects() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 250,
            offset: 0,
            next: 100,
            items: rows(0, 100, 1),
            node: {
                path: "/x"
            }
        });
        compare(list.items.length, 100);
        compare(list.total, 250);
        compare(list.extra, {
            node: {
                path: "/x"
            }
        });
        verify(!list.loading);
        const firstRow = list.items[0];

        list.fetchMore();
        list.fetchMore(); // ignored while loading
        compare(Backend.calls.length, 2);
        compare(Backend.calls[1].args.offset, 100);
        reply(1, {
            rev: 5,
            total: 250,
            offset: 100,
            next: 200,
            items: rows(100, 100, 1)
        });
        compare(list.items.length, 200);
        verify(list.items[0] === firstRow);
        compare(list.extra, {
            node: {
                path: "/x"
            }
        });
    }

    function test_extraIsEmptyWithoutExtraFields() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 1,
            offset: 0,
            next: null,
            items: rows(0, 1, 1)
        });
        compare(list.extra, {});
        list.fetchMore();
        compare(Backend.calls.length, 1, "no next page");
    }

    function test_olderArgsReplyIsDropped() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 250,
            offset: 0,
            next: 100,
            items: rows(0, 100, 1)
        });
        list.fetchMore();
        tc.query = "b";
        wait(20);
        compare(Backend.calls.length, 3);
        compare(Backend.calls[2].args.query, "b");
        compare(Backend.calls[2].args.offset, 0);
        reply(1, {
            rev: 5,
            total: 250,
            offset: 100,
            next: 200,
            items: rows(100, 100, 1)
        });
        compare(list.items.length, 100, "the old fetchMore reply is dropped, old items stay");
        reply(2, {
            rev: 5,
            total: 3,
            offset: 0,
            next: null,
            items: rows(0, 3, 1)
        });
        compare(list.items.length, 3);
    }

    // Review F2: a restart while inactive used to leave loading true until the list was active again.
    function test_staleReplyWhileInactiveLeavesLoadingFalse() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 250,
            offset: 0,
            next: 100,
            items: rows(0, 100, 1)
        });
        list.fetchMore();
        verify(list.loading);
        tc.panelOpen = false;
        tc.query = "b";
        wait(20);
        compare(Backend.calls.length, 2, "an inactive list does not fetch");
        reply(1, {
            rev: 5,
            total: 250,
            offset: 100,
            next: 200,
            items: rows(100, 100, 1)
        });
        compare(list.items.length, 100, "the reply for the old args is dropped");
        verify(!list.loading, "nothing for the current args is in flight");

        tc.panelOpen = true;
        wait(20);
        compare(Backend.calls.length, 3);
        compare(Backend.calls[2].args.query, "b");
        compare(Backend.calls[2].args.offset, 0);
        verify(list.loading);
    }

    // The other half of F2: dropping a stale reply must not hide the current request.
    function test_staleReplyKeepsLoadingForTheCurrentRequest() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        tc.query = "b";
        wait(20);
        compare(Backend.calls.length, 2);
        reply(0, {
            rev: 5,
            total: 3,
            offset: 0,
            next: null,
            items: rows(0, 3, 1)
        });
        compare(list.items.length, 0, "the reply for the old args is dropped");
        verify(list.loading, "page 0 for the new args is still in flight");
        reply(1, {
            rev: 5,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        verify(!list.loading);
        compare(list.items.length, 2);
    }

    function test_revChangeOnLaterPageRestarts() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 250,
            offset: 0,
            next: 100,
            items: rows(0, 100, 1)
        });
        list.fetchMore();
        reply(1, {
            rev: 6,
            total: 251,
            offset: 100,
            next: 200,
            items: rows(100, 100, 1)
        });
        compare(list.items.length, 100);
        wait(20);
        compare(Backend.calls.length, 3);
        compare(Backend.calls[2].args.offset, 0);
        list.fetchMore();
        compare(Backend.calls.length, 3, "no fetchMore while page 0 is due");
    }

    function test_collectionRevRefetchesKeepingItems() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        const kept = list.items[1];
        tc.rev = 2;
        wait(20);
        compare(Backend.calls.length, 2);
        compare(list.items.length, 2, "old items stay until page 0 arrives");
        reply(1, {
            rev: 6,
            total: 2,
            offset: 0,
            next: null,
            items: [
                {
                    id: "0",
                    rev: 2
                },
                {
                    id: "1",
                    rev: 1
                }
            ]
        });
        verify(list.items[1] === kept);
        verify(list.items[0].rev === 2);
    }

    // Review F5: a hello and the topic's new rev in the same turn send one page-0 request.
    function test_helloAndRevChangeInOneTickFetchOnce() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 1,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        Backend.up = false;
        Backend.instance = "two";
        Backend.up = true;
        tc.rev = 2;
        wait(20);
        compare(Backend.calls.length, 2, "one request for the hello and the rev together");
        compare(Backend.calls[1].args.offset, 0);
    }

    // Review F5: the rev that arrives after page 0 was sent is checked against the reply, not refetched.
    function test_revArrivingWhilePageZeroIsInFlightFetchesOnce() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        compare(Backend.calls.length, 1);
        tc.rev = 2;
        wait(20);
        compare(Backend.calls.length, 1, "no second request while page 0 is in flight");
        reply(0, {
            rev: 2,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 1, "the reply is as new as the topic");
        compare(list.items.length, 2);
    }

    // P19: a collection rev is a hash, so a reply whose rev is larger but not the topic's is stale too.
    function test_replyWithAnotherHashRevIsRefetchedOnce() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        tc.rev = 5;
        wait(20);
        reply(0, {
            rev: 9,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 2, "one refetch for the rev the topic holds");
        compare(Backend.calls[1].args.offset, 0);
        reply(1, {
            rev: 5,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 2, "the refetched page is kept");
        compare(list.items.length, 2);
    }

    function test_replyOlderThanTheTopicRevIsRefetched() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        tc.rev = 3;
        wait(20);
        reply(0, {
            rev: 2,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 2, "the page is behind the topic");
        compare(Backend.calls[1].args.offset, 0);
    }

    // A topic restored from a persisted snapshot can carry a collection rev the running engine never
    // reaches (testkit counts from 1 again in each generation). The refetch for a rev that moved while
    // page 0 was in flight happens once; the reply to it is kept even when it is still behind.
    function test_replyStillBehindAfterTheRefetchIsKept() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        tc.rev = 2;
        wait(20);
        reply(0, {
            rev: 1,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 2, "one refetch for the rev that moved");
        reply(1, {
            rev: 1,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 2, "no request after the refetch");
        compare(list.items.length, 2);
        verify(!list.loading);
    }

    // The same persisted rev, already there when the list is created: page 0 was asked for after it, so
    // the reply is current whatever its rev.
    function test_replyBehindARevSetBeforeTheRequestIsKept() {
        tc.rev = 2;
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 1,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        wait(20);
        compare(Backend.calls.length, 1, "no refetch");
        compare(list.items.length, 2);
    }

    function test_inactiveListDoesNotFetch() {
        tc.panelOpen = false;
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        compare(Backend.calls.length, 0);
        tc.panelOpen = true;
        wait(20);
        compare(Backend.calls.length, 1);
    }

    function test_newInstanceClearsAndUpRefetches() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        reply(0, {
            rev: 5,
            total: 2,
            offset: 0,
            next: null,
            items: rows(0, 2, 1)
        });
        Backend.up = false;
        Backend.instance = "two";
        compare(list.items.length, 0);
        compare(list.total, 0);
        Backend.up = true;
        wait(20);
        compare(Backend.calls.length, 2);
        compare(Backend.calls[1].args.offset, 0);
    }

    function test_failureSetsErrorCode() {
        const list = createTemporaryObject(listComponent, tc);
        wait(20);
        Backend.calls[0].done({
            ok: false,
            error: {
                code: "disconnected",
                message: "x"
            }
        });
        compare(list.errorCode, "disconnected");
        verify(!list.loading);
    }

    function test_topicAttachesOnce() {
        const topic = createTemporaryObject(topicComponent, tc);
        compare(Backend.attachments, ["+testkit"]);
        tc.panelOpen = false;
        tc.panelOpen = true;
        compare(Backend.attachments, ["+testkit", "-testkit", "+testkit"]);
    }

    function test_topicApply() {
        const topic = createTemporaryObject(topicComponent, tc);
        let seen = null;
        let hasDataDuringSnapshot = null;
        topic.snapshot.connect(data => {
            seen = data;
            hasDataDuringSnapshot = topic.hasData;
        });
        compare(topic.status, "waiting");
        topic._apply({
            t: "snapshot",
            topic: "testkit",
            rev: 3,
            status: "stale",
            source: "live",
            observed_at: 1000,
            error: {
                code: "source_down",
                message: "gone",
                details: {
                    reason: "x"
                }
            },
            data: {
                n: 1
            }
        });
        compare(topic.status, "stale");
        compare(topic.errorCode, "source_down");
        compare(topic.errorReason, "x");
        compare(seen, {
            n: 1
        });
        compare(hasDataDuringSnapshot, false);
        verify(topic.hasData);

        topic._apply({
            rev: 2,
            status: "ok",
            source: "live",
            observed_at: 2000,
            data: {
                n: 2
            }
        });
        compare(topic.status, "stale", "an older rev is dropped");

        topic._apply({
            rev: 4,
            status: "starting",
            source: "live",
            observed_at: 3000,
            data: null
        });
        compare(topic.status, "starting");
        compare(topic.errorCode, "");
        verify(topic.hasData, "a data-null frame keeps the data");
        compare(seen, {
            n: 1
        });
    }

    function test_topicResetsOnNewInstance() {
        const topic = createTemporaryObject(topicComponent, tc);
        let resets = 0;
        topic.reset.connect(() => resets++);
        topic._apply({
            rev: 40,
            status: "ok",
            source: "live",
            observed_at: 1000,
            data: {
                n: 1
            }
        });
        compare(resets, 0, "the first instance is no restart");
        Backend.instance = "two";
        topic._apply({
            rev: 1,
            status: "ok",
            source: "live",
            observed_at: 2000,
            data: {
                n: 2
            }
        });
        compare(resets, 1);
        compare(topic.observedAt, 2000, "rev 1 of the new instance applies");
    }

    function test_topicAge() {
        const topic = createTemporaryObject(topicComponent, tc);
        compare(topic.ageMs, 0);
        Backend.now = 10000;
        topic._apply({
            rev: 1,
            status: "ok",
            source: "persisted",
            observed_at: 4000,
            data: {}
        });
        compare(topic.ageMs, 6000);
        verify(!topic.live, "persisted is not live");
        topic._apply({
            rev: 2,
            status: "ok",
            source: "live",
            observed_at: 4000,
            data: {}
        });
        verify(topic.live);
        Backend.downSince = 8000;
        Backend.up = false;
        compare(topic.knownAt, 8000);
        compare(topic.ageMs, 2000);
        verify(!topic.live);
    }

    function frameOfRev(frameRev) {
        return {
            rev: frameRev,
            status: "ok",
            source: "live",
            observed_at: 1000 * frameRev,
            data: {
                n: frameRev
            }
        };
    }

    // On subscribe axon sends the topic's current frame, which has the rev already applied while
    // nothing changed. Dropped as not newer, it still says the data is current.
    function test_topicSyncedAtTheFirstFrameEvenOfAnEqualRev() {
        const topic = createTemporaryObject(topicComponent, tc);
        verify(!topic.synced, "no frame yet");
        let syncedDuringSnapshot = null;
        topic.snapshot.connect(() => syncedDuringSnapshot = topic.synced);
        topic._apply(frameOfRev(3));
        verify(topic.synced);
        compare(syncedDuringSnapshot, false, "synced only after the service took the data");

        tc.panelOpen = false;
        verify(!topic.synced, "inactive");
        tc.panelOpen = true;
        verify(!topic.synced, "a new subscription waits for its own frame");
        topic._apply(frameOfRev(3));
        compare(topic.observedAt, 3000, "the equal rev is dropped");
        verify(topic.synced, "and still syncs the topic");
    }

    function test_topicSyncedClearsWhileAxonIsDown() {
        const topic = createTemporaryObject(topicComponent, tc);
        topic._apply(frameOfRev(3));
        verify(topic.synced);
        Backend.up = false;
        verify(!topic.synced, "down");
        Backend.up = true;
        verify(!topic.synced, "the hello is not the subscription's frame");
        topic._apply(frameOfRev(3));
        verify(topic.synced);
    }

    // Backend hands a late attacher its cached frame also while axon is down. That frame is from
    // before the drop.
    function test_topicFrameTakenWhileDownLeavesItUnsynced() {
        Backend.up = false;
        const topic = createTemporaryObject(topicComponent, tc);
        topic._apply(frameOfRev(3));
        compare(topic.observedAt, 3000, "applied");
        verify(!topic.synced);
    }

    function test_topicUnknown() {
        const topic = createTemporaryObject(topicComponent, tc);
        topic._unknown();
        compare(topic.status, "error");
        compare(topic.errorCode, "unknown_topic");
    }
}
