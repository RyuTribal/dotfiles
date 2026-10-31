import QtQuick
import QtTest
import "../../services/sweep.js" as S

// The pure parts of services/Sweep.qml. Needs nothing else:
// QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/unit/tst_sweep.qml
TestCase {
    name: "Sweep"

    readonly property var tree: ({
            rev: 9,
            count: 61,
            gen: 3,
            root: "3:0",
            path: "/home/ryutribal"
        })
    readonly property var runningScan: ({
            id: "sweep.scan-4",
            method: "sweep.scan",
            state: "running",
            started_at: 1792922292000,
            progress: {
                done: 412803,
                total: null,
                text: "412803 files, 98.1 G"
            }
        })
    readonly property var failedScan: ({
            id: "sweep.scan-4",
            method: "sweep.scan",
            state: "failed",
            started_at: 1792922292000,
            finished_at: 1792922293000,
            error: {
                code: "permission_denied",
                message: "/root: Permission denied (os error 13)"
            }
        })

    function test_human() {
        compare(S.human(0), "0 B");
        compare(S.human(1023), "1023 B");
        compare(S.human(1024), "1.0 K");
        compare(S.human(1280), "1.2 K", "an exact tie rounds to even, as Rust's {:.1} does");
        compare(S.human(1331), "1.3 K");
        compare(S.human(3328), "3.2 K", "an exact tie rounds to even, as Rust's {:.1} does");
        compare(S.human(9126805504), "8.5 G");
        compare(S.human(535032197120), "498.3 G");
    }

    function test_scanOp() {
        const doneScan = Object.assign({}, runningScan, {
            state: "done"
        });
        const runningCommit = Object.assign({}, runningScan, {
            id: "sweep.commit-1",
            method: "sweep.commit"
        });
        // A scan queued behind the running commit: it has no progress yet.
        const queuedScan = {
            id: "sweep.scan-5",
            method: "sweep.scan",
            state: "queued",
            started_at: 1792922292000
        };

        compare(S.scanOp([runningCommit, runningScan]), runningScan);
        compare(S.scanOp([runningCommit, queuedScan]), queuedScan, "a queued scan counts");
        compare(S.statusText(true, "ok", "", queuedScan, "", true), "scanning…");
        compare(S.scanOp([doneScan]), null, "a scan that ended");
        compare(S.scanOp([runningCommit]), null, "a commit is no scan");
        compare(S.scanOp([]), null);
    }

    function test_autoScanWanted() {
        verify(S.autoScanWanted(true, true, "ok", null, null, false), "everything clear");
        verify(S.autoScanWanted(true, true, "error", null, null, false), "a journal error still allows a scan");
        verify(!S.autoScanWanted(false, true, "ok", null, null, false), "the panel is closed");
        verify(!S.autoScanWanted(true, false, "ok", null, null, false), "not synced: a reopen holds what the view saw before it closed");
        verify(!S.autoScanWanted(true, false, "ok", null, null, true), "not synced, and the auto-scan already ran");
        verify(!S.autoScanWanted(true, true, "starting", null, null, false), "the journal replay runs");
        verify(!S.autoScanWanted(true, true, "waiting", null, null, false), "no snapshot yet");
        verify(!S.autoScanWanted(true, true, "unconfigured", null, null, false), "the engine is disabled");
        verify(!S.autoScanWanted(true, true, "failed", null, null, false), "the engine failed");
        verify(!S.autoScanWanted(true, true, "ok", tree, null, false), "a tree is loaded");
        verify(!S.autoScanWanted(true, true, "ok", null, runningScan, false), "another client's scan dropped the tree and runs");
        verify(!S.autoScanWanted(true, true, "ok", null, null, true), "the auto-scan already ran");
    }

    // The evaluations Sweep.qml makes, in the order they happen. Opening the panel evaluates nothing.
    // A subscription's first snapshot is applied before the topic is synced, and the topic syncing
    // evaluates again.
    function test_autoScanWanted_on_open_and_reopen() {
        // First open on a released tree.
        verify(!S.autoScanWanted(true, false, "ok", null, null, false), "first open, the first snapshot");
        verify(S.autoScanWanted(true, true, "ok", null, null, false), "first open, synced");

        // Reopen after the release: the view held the tree from before the panel closed.
        verify(!S.autoScanWanted(true, false, "ok", null, null, false), "reopen, the fresh snapshot");
        verify(S.autoScanWanted(true, true, "ok", null, null, false), "reopen, synced; closing cleared the flag");

        // Reopen after an auto-scan the view never saw run: it held no tree and no scan, and the
        // subscription's frame brings the tree that scan loaded.
        verify(!S.autoScanWanted(true, false, "ok", tree, null, false), "reopen, the frame's snapshot");
        verify(!S.autoScanWanted(true, true, "ok", tree, null, false), "reopen, synced");

        // The auto-scan set the flag, and its op failed.
        verify(!S.autoScanWanted(true, true, "ok", null, S.scanOp([failedScan]), true), "after the auto-scan failed");
    }

    function test_statusText() {
        compare(S.statusText(false, "ok", "", null, "", true), "axon not reachable");
        compare(S.statusText(true, "starting", "", null, "", false), "restoring staged items");
        compare(S.statusText(true, "failed", "", null, "", false), "sweep engine failed: run axon engine.restart name=sweep");
        compare(S.statusText(true, "error", "the journal was quarantined", null, "", false), "the journal was quarantined");
        compare(S.statusText(true, "unconfigured", "sweep is disabled", null, "", false), "sweep is disabled");
        compare(S.statusText(true, "ok", "", runningScan, "", false), "scanning… 412803 files, 98.1 G");
        compare(S.statusText(true, "ok", "", null, "restored 2 item(s)", true), "restored 2 item(s)");
        compare(S.statusText(true, "ok", "", null, "", true), "ready");
        compare(S.statusText(true, "ok", "", null, "", false), "no scan loaded");
    }

    function test_stagedLine() {
        compare(S.stagedLine(2, "1.2 K"), "2 staged (1.2 K) — in trash until you delete");
        compare(S.stagedLine(0, "0 B"), "", "nothing staged");
    }

    // The footer's order, case by case: each case shows while every case above it is clear.
    function test_footerLine() {
        const failure = "a scan is running";
        const staged = "2 staged (1.2 K) — in trash until you delete";
        compare(S.footerLine(false, "failed", "", failure, staged, "ready"), "axon not reachable");
        compare(S.footerLine(true, "starting", "", failure, staged, "ready"), "restoring staged items");
        compare(S.footerLine(true, "failed", "", failure, staged, "ready"), "sweep engine failed: run axon engine.restart name=sweep");
        compare(S.footerLine(true, "error", "the journal was quarantined", failure, staged, "ready"), "the journal was quarantined");
        compare(S.footerLine(true, "unconfigured", "sweep is disabled", failure, staged, "ready"), "sweep is disabled");
        compare(S.footerLine(true, "ok", "", failure, staged, "ready"), failure, "this view's own failure");
        compare(S.footerLine(true, "ok", "", "", staged, "ready"), staged);
        compare(S.footerLine(true, "ok", "", "", "", "restored 2 item(s)"), "restored 2 item(s)", "the status");
    }

    function test_commitMessage() {
        const result = {
            deleted: 2,
            failed: 0,
            freed_bytes: 9126805504
        };
        compare(S.commitMessage(result, null), "freed 8.5 G");
        compare(S.commitMessage(result, {
            failed: 0,
            errors: []
        }), "freed 8.5 G");

        const failedOne = {
            failed: 1,
            errors: [
                {
                    message: "Permission denied (os error 13)"
                }
            ]
        };
        compare(S.commitMessage({
            deleted: 0,
            failed: 1,
            freed_bytes: 0
        }, failedOne), "freed 0 B · 1 failed: Permission denied (os error 13)");
    }
}
