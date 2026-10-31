import QtQuick
import QtTest
import ShellUnderTest

// The real services/Sweep.qml over the stand-in Backend of fakes/Backend.qml: the auto-scan, the op
// follower, the listing and the status line. backend_tests.sh copies Sweep.qml without its `pragma
// Singleton` line into ShellUnderTest as SweepService, so each test makes its own instance. Run it
// through backend_tests.sh.
TestCase {
    id: tc
    name: "SweepService"

    Component {
        id: sweepComponent
        SweepService {
            scanPath: "/home/test"
        }
    }

    property double rev: 0
    readonly property string twoStaged: "2 staged (1.2 K) — in trash until you delete"

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 0;
        Backend.downSince = 0;
        tc.rev = 0;
    }

    function tree(gen, treeRev) {
        return {
            rev: treeRev,
            count: 3,
            gen: gen,
            root: gen + ":0",
            path: "/home/test",
            scanned_at: 1,
            files: 2,
            bytes: 3,
            errors: 0,
            skipped_mounts: 0
        };
    }

    function topicData(treeStub, staged, lastCommit) {
        return {
            tree: treeStub,
            staged: staged ?? {
                count: 0,
                bytes: 0
            },
            freed_bytes: 0,
            last_commit: lastCommit ?? null,
            last_restore: null
        };
    }

    function frame(sweep, status, snapshotData, ops) {
        tc.rev += 1;
        sweep.topic._apply({
            t: "snapshot",
            topic: "sweep",
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1,
            ops: ops ?? [],
            data: snapshotData
        });
    }

    // The data of tree 3 with two items staged.
    function twoStagedData() {
        return topicData(tree(3, 1), {
            count: 2,
            bytes: 1280
        });
    }

    function op(id, method, state, extra) {
        return Object.assign({
            id: id,
            method: method,
            state: state,
            started_at: 1
        }, extra ?? {});
    }

    function callsOf(method) {
        return Backend.calls.filter(call => call.method === method);
    }

    function replyOp(call, id) {
        call.done({
            ok: true,
            result: {
                op: {
                    id: id,
                    topic: "sweep"
                }
            }
        });
    }

    function fail(call, code, message) {
        call.done({
            ok: false,
            error: {
                code: code,
                message: message
            }
        });
    }

    function test_firstOpenScansOnceFromTheFirstOkSnapshot() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        compare(Backend.attachments, [], "closed: not subscribed");
        sweep.panelOpen = true;
        compare(Backend.attachments, ["+sweep"]);
        compare(callsOf("sweep.scan").length, 0, "at visibility the view holds waiting");
        compare(sweep.status, "no scan loaded");

        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1);
        compare(callsOf("sweep.scan")[0].args, {
            path: "/home/test",
            restore: true
        });
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1, "the flag holds until the op shows");

        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "running", {
                progress: {
                    done: 7,
                    total: null,
                    text: "7 files, 1.0 K"
                }
            })]);
        verify(sweep.scanning);
        compare(sweep.status, "scanning… 7 files, 1.0 K");
        compare(callsOf("sweep.scan").length, 1);

        frame(sweep, "ok", topicData(tree(3, 1)), [op("sweep.scan-1", "sweep.scan", "done")]);
        verify(!sweep.scanning);
        verify(sweep.ready);
        compare(sweep.status, "ready");
        compare(sweep.curId, "3:0");
    }

    function test_failedAutoScanShowsItsErrorAndDoesNotLoop() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "failed", {
                error: {
                    code: "permission_denied",
                    message: "/root: Permission denied (os error 13)"
                }
            })]);
        compare(sweep.footer, "/root: Permission denied (os error 13)");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "failed", {
                error: {
                    code: "permission_denied",
                    message: "/root: Permission denied (os error 13)"
                }
            })]);
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1, "no second auto-scan");

        sweep.panelOpen = false;
        compare(Backend.attachments, ["+sweep", "-sweep"]);
        sweep.panelOpen = true;
        compare(callsOf("sweep.scan").length, 1, "nothing on the held state before the subscription's frame");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "failed", {
                error: {
                    code: "permission_denied",
                    message: "/root: Permission denied (os error 13)"
                }
            })]);
        compare(callsOf("sweep.scan").length, 2, "closing cleared the flag; the frame shows nothing loaded");
    }

    function test_reopenAfterReleaseScansFromTheFreshSnapshot() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        compare(callsOf("sweep.scan").length, 0);
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        compare(callsOf("sweep.scan").length, 0, "at visibility the pre-close tree is held");
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1, "the fresh snapshot shows the release");
        compare(sweep.curId, "");
        compare(sweep.entries, []);
    }

    // The panel opened on a released tree and closed before a frame showed its auto-scan, and that
    // scan then loaded a tree. At the reopen the view holds no tree and no running scan, which is not
    // what axon has: it scans nothing until the subscription's frame, which brings that tree.
    function test_aReopenScansNothingBeforeTheSubscriptionsFrame() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1, "the auto-scan of the released tree");
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        compare(callsOf("sweep.scan").length, 1, "nothing scanned on the held state");
        frame(sweep, "ok", topicData(tree(3, 1)), [op("sweep.scan-1", "sweep.scan", "done")]);
        wait(20);
        compare(callsOf("sweep.scan").length, 1, "the frame brings the tree that scan loaded");
        compare(callsOf("sweep.list").map(call => call.args.node), ["3:0"]);
        compare(sweep.curId, "3:0");
    }

    // axon lists a scan's op before it replies to sweep.scan, so a subscription made before the
    // auto-scan's reply can get a frame older than the scan: no tree and no op. A close and a reopen
    // in that time send no second sweep.scan, which axon would refuse while the first runs.
    function test_aReopenBeforeTheAutoScansReplySendsNoSecondScan() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 1, "the auto-scan of the released tree");
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        wait(20);
        compare(callsOf("sweep.scan").length, 1, "the frame is older than the scan");

        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "running")]);
        verify(sweep.scanning);
        frame(sweep, "ok", topicData(tree(3, 1)), [op("sweep.scan-1", "sweep.scan", "done")]);
        compare(sweep.status, "ready");
        compare(callsOf("sweep.scan").length, 1);

        sweep.panelOpen = false;
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "done")]);
        compare(callsOf("sweep.scan").length, 2, "as before: a reopen on a released tree scans once");
    }

    // The reopened panel takes the auto-scan still waiting for its reply as its own: when that scan
    // fails, the failure shows and nothing scans again.
    function test_aReopenBeforeTheAutoScansReplyThenAFailureScansNoMore() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "failed", {
                error: {
                    code: "permission_denied",
                    message: "/root: Permission denied (os error 13)"
                }
            })]);
        wait(20);
        compare(sweep.footer, "/root: Permission denied (os error 13)");
        compare(callsOf("sweep.scan").length, 1, "no second auto-scan");
    }

    // A call that ends while the panel is closed ends the wait: the reopen is a new open and scans
    // the tree it finds released.
    function test_anAutoScanRefusedWhileClosedLeavesTheReopenToScan() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        sweep.panelOpen = false;
        fail(callsOf("sweep.scan")[0], "timeout", "no reply from axon in 12 s");
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 2);
    }

    // The panel closed while its auto-scan ran and stayed closed: the scan ended, axon dropped the
    // ended op after 5 minutes and released the tree. The reopen scans the released tree, as every
    // open on one does, although this view never saw that op end.
    function test_aReopenLongAfterTheAutoScanEndedScansTheReleasedTree() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "running")]);
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 2);
    }

    function test_anotherClientsScanIsNoReasonToScanAndClearsTheMessage() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.restoreAll();
        callsOf("sweep.restore_all")[0].done({
            ok: true,
            result: {
                restored: 2,
                renamed: []
            }
        });
        compare(sweep.status, "restored 2 item(s)");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-9", "sweep.scan", "running")]);
        compare(callsOf("sweep.scan").length, 0);
        compare(sweep.status, "scanning…");
        frame(sweep, "ok", topicData(tree(4, 2)), [op("sweep.scan-9", "sweep.scan", "done")]);
        compare(sweep.status, "ready", "the message from before the scan is gone");
        compare(callsOf("sweep.scan").length, 0);
    }

    function test_commitDoneOnceWithItsMessage() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1), {
            count: 1,
            bytes: 1280
        }));
        compare(sweep.stagedCount, 1);
        compare(sweep.stagedHuman, "1.2 K");
        const ended = [];
        sweep.commit(endedOp => ended.push(endedOp));
        compare(callsOf("sweep.commit")[0].args, {
            confirm: true
        });
        replyOp(callsOf("sweep.commit")[0], "sweep.commit-1");
        frame(sweep, "ok", topicData(tree(3, 2), {
            count: 1,
            bytes: 1280
        }), [op("sweep.commit-1", "sweep.commit", "running")]);
        compare(ended.length, 0);
        const lastCommit = {
            deleted: 0,
            failed: 1,
            freed_bytes: 0,
            errors: [
                {
                    path: "/home/test/locked-dir",
                    message: "Permission denied (os error 13)"
                }
            ],
            finished_at: 2
        };
        const doneOp = op("sweep.commit-1", "sweep.commit", "done", {
            result: {
                deleted: 0,
                failed: 1,
                freed_bytes: 0
            }
        });
        frame(sweep, "ok", topicData(tree(3, 3), null, lastCommit), [doneOp]);
        compare(ended.length, 1);
        compare(ended[0].state, "done");
        compare(sweep.footer, "freed 0 B · 1 failed: Permission denied (os error 13)");
        compare(sweep.stagedCount, 0);
        frame(sweep, "ok", topicData(tree(3, 3), null, lastCommit), [doneOp]);
        compare(ended.length, 1, "called once");
    }

    function test_opEndedBeforeTheReplyIsCalledAtTheReply() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        const ended = [];
        sweep.commit(endedOp => ended.push(endedOp));
        frame(sweep, "ok", topicData(tree(3, 2), null, {
            deleted: 1,
            failed: 0,
            freed_bytes: 1280,
            errors: [],
            finished_at: 2
        }), [op("sweep.commit-1", "sweep.commit", "done", {
                result: {
                    deleted: 1,
                    failed: 0,
                    freed_bytes: 1280
                }
            })]);
        compare(ended.length, 0);
        replyOp(callsOf("sweep.commit")[0], "sweep.commit-1");
        compare(ended.length, 1);
        compare(sweep.status, "freed 1.2 K");
    }

    // A failed journal save ends the commit with its entries still staged.
    function test_failedCommitShowsTheOpErrorOverTheStagedLine() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        sweep.commit(null);
        replyOp(callsOf("sweep.commit")[0], "sweep.commit-1");
        frame(sweep, "ok", twoStagedData(), [op("sweep.commit-1", "sweep.commit", "failed", {
                error: {
                    code: "write_failed",
                    message: "/state/journal.json: No space left on device (os error 28)"
                }
            })]);
        compare(sweep.footer, "/state/journal.json: No space left on device (os error 28)");
    }

    // A refused commit leaves the items staged, so its message shows over the staged line until the
    // next click (ruling, 2026-10-31).
    function test_aRefusedCommitShowsItsMessageUntilTheNextClick() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        compare(sweep.footer, tc.twoStaged);
        sweep.commit(null);
        fail(callsOf("sweep.commit")[0], "conflict", "a scan is running");
        compare(sweep.footer, "a scan is running");
        frame(sweep, "ok", twoStagedData());
        compare(sweep.footer, "a scan is running", "a snapshot keeps it");
        sweep.open("3:1");
        compare(sweep.footer, tc.twoStaged, "the next click");
    }

    // A failure is shown once (section 33.0): in the footer, never as the status too, so once a click
    // dismissed it and the other client's commit took the staged line away, the footer shows the real
    // status.
    function test_aDismissedFailureLeavesNoStatusBehind() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData(), [op("sweep.commit-7", "sweep.commit", "running")]);
        sweep.commit(null);
        fail(callsOf("sweep.commit")[0], "conflict", "a commit is running");
        compare(sweep.footer, "a commit is running");
        compare(sweep.status, "ready", "shown once, in the footer");
        sweep.open("3:1");
        compare(sweep.footer, tc.twoStaged, "the next click");
        frame(sweep, "ok", topicData(tree(3, 2), null, {
            deleted: 2,
            failed: 0,
            freed_bytes: 1280,
            errors: [],
            finished_at: 2
        }), [op("sweep.commit-7", "sweep.commit", "done", {
                result: {
                    deleted: 2,
                    failed: 0,
                    freed_bytes: 1280
                }
            })]);
        compare(sweep.status, "ready");
        compare(sweep.footer, "ready");
    }

    // restore_all restores what it can and names the first item it could not restore, which stays
    // staged.
    function test_aPartlyFailedRestoreAllShowsItsMessageUntilTheNextClick() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        sweep.restoreAll();
        const message = "could not restore 1 item(s); first: /home/test/locked: Permission denied (os error 13)";
        fail(callsOf("sweep.restore_all")[0], "failed", message);
        frame(sweep, "ok", topicData(tree(3, 2), {
            count: 1,
            bytes: 1024
        }));
        compare(sweep.footer, message);
        sweep.mark("3:2");
        compare(sweep.footer, "1 staged (1.0 K) — in trash until you delete", "the next click");
    }

    // An item the commit could not delete goes back to where it was; when that fails too, it stays
    // staged, and the commit's message shows over the staged line.
    function test_aCommitThatFailedToDeleteAnItemShowsItsMessageOverTheStagedLine() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        sweep.commit(null);
        replyOp(callsOf("sweep.commit")[0], "sweep.commit-1");
        const lastCommit = {
            deleted: 1,
            failed: 1,
            freed_bytes: 256,
            errors: [
                {
                    path: "/home/test/locked-dir",
                    message: "Permission denied (os error 13)"
                }
            ],
            finished_at: 2
        };
        const doneOp = op("sweep.commit-1", "sweep.commit", "done", {
            result: {
                deleted: 1,
                failed: 1,
                freed_bytes: 256
            }
        });
        frame(sweep, "ok", topicData(tree(3, 2), {
            count: 1,
            bytes: 1024
        }, lastCommit), [doneOp]);
        compare(sweep.footer, "freed 256 B · 1 failed: Permission denied (os error 13)");
    }

    // Every click ends the failure's turn in the footer.
    function test_everyClickReturnsTheStagedLine() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        const clicks = {
            up: () => sweep.up(),
            open: () => sweep.open("3:1"),
            mark: () => sweep.mark("3:1"),
            unmark: () => sweep.unmark("3:2"),
            restoreAll: () => sweep.restoreAll(),
            commit: () => sweep.commit(null),
            scan: () => sweep.scan(sweep.scanPath, null)
        };
        for (const name in clicks) {
            sweep.mark("3:5");
            const marks = callsOf("sweep.mark");
            fail(marks[marks.length - 1], "conflict", "id is from scan 2, current scan is 3");
            compare(sweep.footer, "id is from scan 2, current scan is 3", name);
            clicks[name]();
            compare(sweep.footer, tc.twoStaged, name);
        }
    }

    // Axon down and the engine's own status show before this view's failure, and a scan, this view's
    // or another client's, clears it.
    function test_theFooterOrderAroundAFailureAndAScanClearingIt() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", twoStagedData());
        sweep.unmark("3:2");
        fail(callsOf("sweep.unmark")[0], "failed", "Permission denied (os error 13)");
        frame(sweep, "starting", twoStagedData());
        compare(sweep.footer, "restoring staged items");
        frame(sweep, "failed", twoStagedData());
        compare(sweep.footer, "sweep engine failed: run axon engine.restart name=sweep");
        Backend.up = false;
        compare(sweep.footer, "axon not reachable");
        Backend.up = true;
        frame(sweep, "ok", twoStagedData());
        compare(sweep.footer, "Permission denied (os error 13)", "the failure again, no click since");

        frame(sweep, "ok", topicData(null), [op("sweep.scan-9", "sweep.scan", "running")]);
        frame(sweep, "ok", topicData(tree(4, 2)), [op("sweep.scan-9", "sweep.scan", "done")]);
        compare(sweep.footer, "ready");
        frame(sweep, "ok", topicData(tree(4, 3), {
            count: 2,
            bytes: 1280
        }), [op("sweep.scan-9", "sweep.scan", "done")]);
        compare(sweep.footer, tc.twoStaged, "another client staged items after the scan");
    }

    function test_droppedConnectionDropsPendingDone() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        const ended = [];
        sweep.commit(endedOp => ended.push(endedOp));
        replyOp(callsOf("sweep.commit")[0], "sweep.commit-1");
        Backend.up = false;
        compare(sweep.status, "axon not reachable");
        Backend.up = true;
        frame(sweep, "ok", topicData(tree(3, 2)), [op("sweep.commit-1", "sweep.commit", "done", {
                result: {
                    deleted: 1,
                    failed: 0,
                    freed_bytes: 1280
                }
            })]);
        compare(ended.length, 0, "dropped when up turned false");
        compare(sweep.status, "ready", "no message from an op this view stopped following");
    }

    // The new instance's first frame carries only the envelope, with a status that allows a scan.
    // The auto-scan is checked once that frame syncs the topic, and only the tree held from the old
    // instance keeps it from scanning: the data stays until a snapshot replaces it (section 20).
    function test_resetDropsPendingDoneAndKeepsTheHeldTreeUntilTheNewSnapshot() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        const ended = [];
        sweep.scan("/home/test", endedOp => ended.push(endedOp));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        Backend.up = false;
        Backend.instance = "two"; // the new instance's hello, then up
        Backend.up = true;
        compare(sweep.curId, "", "node ids of the old instance are void");
        tc.rev = 0;
        frame(sweep, "ok", null);
        verify(sweep.topic.synced);
        wait(20);
        compare(callsOf("sweep.scan").length, 1, "the synced check sees the tree held from before the restart");
        verify(sweep.ready);
        frame(sweep, "starting", null);
        compare(sweep.status, "restoring staged items");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "done")]);
        compare(ended.length, 0, "an op id of the old instance");
        compare(callsOf("sweep.scan").length, 2, "the new instance's first ok snapshot with no tree");
        compare(sweep.curId, "");
    }

    function test_callFailuresShowInTheFooterExceptDisconnected() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.mark("3:5");
        compare(callsOf("sweep.mark")[0].args, {
            id: "3:5"
        });
        fail(callsOf("sweep.mark")[0], "disconnected", "axon is not connected");
        compare(sweep.footer, "ready");
        sweep.unmark("3:5");
        fail(callsOf("sweep.unmark")[0], "conflict", "id is from scan 2, current scan is 3");
        compare(sweep.footer, "id is from scan 2, current scan is 3");
        sweep.scan("/home/test", null);
        compare(sweep.footer, "ready", "a scan starting clears the failure");
    }

    function test_listingFollowsCurIdAndResetsOnErrors() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        wait(20);
        let lists = callsOf("sweep.list");
        compare(lists.length, 1);
        compare(lists[0].args, {
            node: "3:0",
            collection: "tree",
            offset: 0,
            limit: 100
        });
        lists[0].done({
            ok: true,
            result: {
                rev: 1,
                total: 2,
                offset: 0,
                next: null,
                node: {
                    id: "3:0",
                    name: "/home/test",
                    path: "/home/test",
                    parent: null,
                    size: 9126805504,
                    disk: 1,
                    dir: true
                },
                items: [
                    {
                        id: "3:1",
                        rev: 11,
                        name: "big",
                        size: 1280,
                        disk: 1,
                        dir: true,
                        marked: false
                    },
                    {
                        id: "3:2",
                        rev: 12,
                        name: "small.txt",
                        size: 1024,
                        disk: 1,
                        dir: false,
                        marked: false
                    }
                ]
            }
        });
        compare(sweep.curPath, "/home/test");
        compare(sweep.parentId, null);
        compare(sweep.totalHuman, "8.5 G");
        compare(sweep.entries.length, 2);
        compare(sweep.entries[0].human, "1.2 K");
        const firstRow = sweep.entries[0];

        // A mark bumps the tree rev: page 0 again, and the unchanged row keeps its object.
        frame(sweep, "ok", topicData(tree(3, 2)));
        wait(20);
        lists = callsOf("sweep.list");
        compare(lists.length, 2);
        lists[1].done({
            ok: true,
            result: {
                rev: 2,
                total: 2,
                offset: 0,
                next: null,
                node: {
                    id: "3:0",
                    name: "/home/test",
                    path: "/home/test",
                    parent: null,
                    size: 9126805504,
                    disk: 1,
                    dir: true
                },
                items: [
                    {
                        id: "3:1",
                        rev: 11,
                        name: "big",
                        size: 1280,
                        disk: 1,
                        dir: true,
                        marked: false
                    },
                    {
                        id: "3:2",
                        rev: 13,
                        name: "small.txt",
                        size: 1024,
                        disk: 1,
                        dir: false,
                        marked: true
                    }
                ]
            }
        });
        verify(sweep.entries[0] === firstRow, "same id and rev keeps the object");
        verify(sweep.entries[1].marked);

        sweep.open("3:1");
        compare(sweep.curId, "3:1");
        wait(20);
        lists = callsOf("sweep.list");
        compare(lists.length, 3);
        compare(lists[2].args.node, "3:1");
        fail(lists[2], "not_found", "no node 3:1");
        compare(sweep.curId, "3:0", "back to the root");

        // A new scan: the root of the new tree.
        frame(sweep, "ok", topicData(tree(4, 3)));
        compare(sweep.curId, "4:0");

        // No scan loaded: no rows, the listing stops.
        frame(sweep, "ok", topicData(null), [op("sweep.scan-2", "sweep.scan", "running")]);
        compare(sweep.curId, "");
        compare(sweep.entries, []);
    }

    function test_upGoesToTheParent() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.open("3:1");
        wait(20);
        const lists = callsOf("sweep.list");
        lists[lists.length - 1].done({
            ok: true,
            result: {
                rev: 1,
                total: 0,
                offset: 0,
                next: null,
                node: {
                    id: "3:1",
                    name: "big",
                    path: "/home/test/big",
                    parent: "3:0",
                    size: 1280,
                    disk: 1,
                    dir: true
                },
                items: []
            }
        });
        compare(sweep.parentId, "3:0");
        sweep.up();
        compare(sweep.curId, "3:0");
    }

    // A disconnected view keeps the last listing and its buttons (section 33.1, Offline).
    function test_canStageFollowsTheTopicStatus() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        verify(sweep.canStage);
        frame(sweep, "error", topicData(tree(3, 1)));
        verify(!sweep.canStage);
        frame(sweep, "ok", topicData(tree(3, 1)));
        verify(sweep.canStage);
        Backend.up = false; // the topic keeps its last status, ok
        compare(sweep.topic.status, "ok");
        verify(sweep.canStage, "the buttons stay while axon is down");
    }

    // While axon is down the status line says so, also with items staged, and a click that fails
    // with `disconnected` leaves it showing. Once axon is back the footer shows the staged line
    // again.
    function test_offlineStatusShowsOverTheStagedLine() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1), {
            count: 2,
            bytes: 1280
        }));
        compare(sweep.footer, tc.twoStaged);

        Backend.up = false;
        compare(sweep.footer, "axon not reachable", "over the staged line");
        compare(sweep.status, "axon not reachable");
        const clicks = [
            {
                method: "sweep.mark",
                click: () => sweep.mark("3:1")
            },
            {
                method: "sweep.unmark",
                click: () => sweep.unmark("3:2")
            },
            {
                method: "sweep.commit",
                click: () => sweep.commit(null)
            },
            {
                method: "sweep.restore_all",
                click: () => sweep.restoreAll()
            },
            {
                method: "sweep.scan",
                click: () => sweep.scan(sweep.scanPath, null)
            }
        ];
        for (const each of clicks) {
            each.click();
            fail(callsOf(each.method)[0], "disconnected", "axon is not connected");
            compare(sweep.status, "axon not reachable", each.method);
            compare(sweep.footer, "axon not reachable", each.method);
        }

        Backend.up = true;
        compare(sweep.footer, tc.twoStaged, "the footer rule again");
        compare(sweep.status, "ready", "a disconnected click left no message");
    }

    // The view held no tree, status ok and no scan op when the daemon restarted: its auto-scan had
    // failed. The new instance's first frame says it is starting, and a starting engine is no reason
    // to scan.
    function test_resetChecksTheAutoScanAgainstTheNewInstance() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(null));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "failed", {
                error: {
                    code: "permission_denied",
                    message: "/root: Permission denied (os error 13)"
                }
            })]);
        compare(callsOf("sweep.scan").length, 1);

        Backend.up = false;
        Backend.instance = "two"; // the new instance's hello, then up
        Backend.up = true;
        tc.rev = 0;
        frame(sweep, "starting", null);
        verify(sweep.topic.synced);
        wait(20);
        compare(callsOf("sweep.scan").length, 1, "the new instance is starting");
        compare(sweep.status, "restoring staged items");

        frame(sweep, "ok", topicData(null));
        compare(callsOf("sweep.scan").length, 2, "the new instance's first ok snapshot with no tree");
        wait(20);
        compare(callsOf("sweep.scan").length, 2, "once");
    }

    function directory(id, path, parent, size) {
        return {
            id: id,
            name: path,
            path: path,
            parent: parent,
            size: size,
            disk: 1,
            dir: true
        };
    }

    // Answers a sweep.list call with page 0 of `node`, holding the one file `fileId`.
    function answerPage(call, pageRev, node, fileId) {
        call.done({
            ok: true,
            result: {
                rev: pageRev,
                total: 1,
                offset: 0,
                next: null,
                node: node,
                items: [
                    {
                        id: fileId,
                        rev: 14,
                        name: "inner.iso",
                        size: node.size,
                        disk: 1,
                        dir: false,
                        marked: false
                    }
                ]
            }
        });
    }

    function lastList() {
        const lists = callsOf("sweep.list");
        return lists[lists.length - 1];
    }

    // BackendList keeps its last page while it is inactive and until the next page 0 arrives. That
    // page is of a scan no longer loaded, so the header, "up" and the rows show nothing from it.
    function test_nothingListedFromAnEarlierScan() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        compare(sweep.totalHuman, "", "no page yet");
        sweep.open("3:1");
        wait(20);
        answerPage(lastList(), 1, directory("3:1", "/home/test/big", "3:0", 9126805504), "3:4");
        compare(sweep.curPath, "/home/test/big");
        compare(sweep.parentId, "3:0");
        compare(sweep.totalHuman, "8.5 G");
        compare(sweep.entries.length, 1);

        // A scan drops the tree.
        frame(sweep, "ok", topicData(null), [op("sweep.scan-2", "sweep.scan", "running")]);
        compare(sweep.curPath, "");
        compare(sweep.parentId, null, "up is disabled");
        compare(sweep.totalHuman, "");
        compare(sweep.entries, []);

        // The new tree loads; until its page 0 the listing still holds scan 3's page.
        frame(sweep, "ok", topicData(tree(4, 2)), [op("sweep.scan-2", "sweep.scan", "done")]);
        compare(sweep.curPath, "");
        compare(sweep.parentId, null);
        compare(sweep.totalHuman, "");
        compare(sweep.entries, []);
        wait(20);
        compare(lastList().args.node, "4:0");
        answerPage(lastList(), 2, directory("4:0", "/home/test", null, 1280), "4:2");
        compare(sweep.curPath, "/home/test");
        compare(sweep.parentId, null);
        compare(sweep.totalHuman, "1.2 K");
        compare(sweep.entries.length, 1);
    }

    // A scan that ends within one frame interval: the view sees scan 3's tree, then scan 4's, with no
    // frame between.
    function test_aNewTreeInOneFrameListsNothingFromTheOldOne() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.open("3:1");
        wait(20);
        answerPage(lastList(), 1, directory("3:1", "/home/test/big", "3:0", 9126805504), "3:4");
        compare(sweep.entries.length, 1);

        frame(sweep, "ok", topicData(tree(4, 2)));
        compare(sweep.curId, "4:0");
        compare(sweep.curPath, "");
        compare(sweep.parentId, null);
        compare(sweep.entries, [], "no row of scan 3 to open or mark");
        wait(20);
        compare(lastList().args.node, "4:0");
        answerPage(lastList(), 2, directory("4:0", "/home/test", null, 1280), "4:2");
        compare(sweep.curPath, "/home/test");
        compare(sweep.entries.length, 1);
        compare(sweep.entries[0].id, "4:2");
    }

    // Navigating within one tree keeps the directory on screen until the next one's page arrives, as
    // the rows do.
    function test_upKeepsTheListedDirectoryUntilItsPageArrives() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.open("3:1");
        wait(20);
        answerPage(lastList(), 1, directory("3:1", "/home/test/big", "3:0", 9126805504), "3:4");
        sweep.up();
        compare(sweep.curId, "3:0");
        compare(sweep.curPath, "/home/test/big");
        compare(sweep.entries.length, 1);
        wait(20);
        answerPage(lastList(), 1, directory("3:0", "/home/test", null, 9126805504), "3:1");
        compare(sweep.curPath, "/home/test");
        compare(sweep.parentId, null);
    }

    // Lists "3:1" of scan 3 and returns how many sweep.list calls were made so far.
    function listScan3Subdirectory(sweep) {
        frame(sweep, "ok", topicData(tree(3, 1)));
        sweep.open("3:1");
        wait(20);
        answerPage(lastList(), 1, directory("3:1", "/home/test/big", "3:0", 9126805504), "3:4");
        return callsOf("sweep.list").length;
    }

    function nodesListedSince(count) {
        return callsOf("sweep.list").slice(count).map(call => call.args.node);
    }

    // A daemon restart with a directory listed. BackendList asks for page 0 again as soon as axon
    // is up, and the new instance would answer a node of the old tree with conflict. So no
    // sweep.list names one: the listing waits for the new instance's tree and lists its root.
    function test_aRestartNeverListsANodeOfTheOldTree() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        const listedBefore = listScan3Subdirectory(sweep);

        Backend.up = false;
        Backend.instance = "two"; // the new instance's hello, then up
        Backend.up = true;
        wait(20);
        tc.rev = 0;
        frame(sweep, "starting", null);
        wait(20);
        frame(sweep, "ok", topicData(null));
        replyOp(callsOf("sweep.scan")[0], "sweep.scan-1");
        frame(sweep, "ok", topicData(null), [op("sweep.scan-1", "sweep.scan", "running")]);
        frame(sweep, "ok", topicData(tree(7, 1)), [op("sweep.scan-1", "sweep.scan", "done")]);
        wait(20);
        compare(nodesListedSince(listedBefore), ["7:0"], "the new instance's root, nothing else");
    }

    // Scan numbers count per daemon instance, so the new instance's first tree can carry the old
    // one's number. It is still a new tree: the listing goes to its root.
    function test_aNewInstancesTreeWithTheOldScanNumberIsListedFromItsRoot() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        const listedBefore = listScan3Subdirectory(sweep);

        Backend.up = false;
        Backend.instance = "two";
        Backend.up = true;
        tc.rev = 0;
        frame(sweep, "ok", topicData(tree(3, 1)));
        compare(sweep.curId, "3:0");
        wait(20);
        compare(nodesListedSince(listedBefore), ["3:0"]);
    }

    // The frame axon sends on subscribe while nothing changed: the rev this view applied last.
    function frameAgain(sweep, snapshotData) {
        tc.rev -= 1;
        frame(sweep, "ok", snapshotData);
    }

    // The engine released the tree while the panel was closed, and the view still holds it. Until the
    // subscription's frame says what is loaded, the listing asks for nothing, since axon would answer a
    // node of the released tree with conflict.
    function test_aReopenAfterAReleaseAsksForNoNodeOfTheReleasedTree() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        const listedBefore = listScan3Subdirectory(sweep);
        sweep.panelOpen = false;
        sweep.panelOpen = true;
        wait(20);
        compare(nodesListedSince(listedBefore), [], "nothing before the subscription's frame");
        frame(sweep, "ok", topicData(null));
        wait(20);
        compare(nodesListedSince(listedBefore), [], "no tree is loaded");
        compare(sweep.curId, "");
    }

    function test_theKeptDirectoryIsListedAgainOnceTheFrameArrives_data() {
        return [
            {
                tag: "reopen",
                interrupt: sweep => {
                    sweep.panelOpen = false;
                    sweep.panelOpen = true;
                }
            },
            {
                tag: "same-instance reconnect",
                interrupt: () => {
                    Backend.up = false;
                    Backend.up = true;
                }
            }
        ];
    }

    // The engine kept the tree and nobody changed it: axon sends the frame this view applied last, and
    // once it is in the listing asks for the kept directory again.
    function test_theKeptDirectoryIsListedAgainOnceTheFrameArrives(data) {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        const listedBefore = listScan3Subdirectory(sweep);
        data.interrupt(sweep);
        wait(20);
        compare(nodesListedSince(listedBefore), [], "nothing before the subscription's frame");
        frameAgain(sweep, topicData(tree(3, 1)));
        wait(20);
        compare(nodesListedSince(listedBefore), ["3:1"]);
        compare(sweep.curId, "3:1");
    }

    // A reconnect to the same instance while another client scanned again. The view still holds scan
    // 3; the listing waits for the subscription's frame, which brings scan 4, and lists its root.
    function test_aReconnectWhileAnotherClientRescannedAsksForNoOldNode() {
        const sweep = createTemporaryObject(sweepComponent, tc);
        sweep.panelOpen = true;
        const listedBefore = listScan3Subdirectory(sweep);
        Backend.up = false;
        Backend.up = true; // the same instance's hello
        wait(20);
        compare(nodesListedSince(listedBefore), [], "nothing before the subscription's frame");
        frame(sweep, "ok", topicData(tree(4, 2)));
        wait(20);
        compare(nodesListedSince(listedBefore), ["4:0"]);
    }
}
