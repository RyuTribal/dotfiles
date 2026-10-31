pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import "sweep.js" as S
import QtQuick
import Quickshell

/**
 * The disk sweep for SweepPanel, a thin client of axon's `sweep` engine (section 33.1). It subscribes
 * only while the panel is open, so the engine can release its tree once nobody looks, pages the open
 * directory through `sweep.list` and scans `scanPath` by itself when the panel opens on nothing
 * loaded. The engine sends bytes; sizes for people are formatted here.
 */
Singleton {
    id: root

    property bool panelOpen: false
    property string scanPath: Quickshell.env("HOME")

    readonly property BackendTopic topic: BackendTopic {
        name: "sweep"
        active: root.panelOpen
        onSnapshot: data => root._onSnapshot(data)
        onReset: root._onReset()
    }

    readonly property bool scanning: _scanOp !== null
    readonly property bool ready: _tree !== null && !scanning // a tree is loaded and no scan runs
    // A disconnected view keeps its buttons: a click fails with `disconnected` and the status line
    // says "axon not reachable" (section 33.1, Offline).
    readonly property bool canStage: topic.status === "ok"
    readonly property string curId: _curId // the listed directory's node id, "" while nothing is loaded
    readonly property string curPath: _node?.path ?? ""
    readonly property var parentId: _node?.parent ?? null // a node id; null at the root and while none is listed
    readonly property string totalHuman: _node !== null ? S.human(_node.size) : ""
    readonly property int stagedCount: _stagedCount
    readonly property string stagedHuman: S.human(_stagedBytes)
    readonly property string status: S.statusText(Backend.up, topic.status, topic.errorMessage, _scanOp, _message, ready)
    readonly property string stagedLine: S.stagedLine(_stagedCount, stagedHuman) // "" while nothing is staged
    // The first that applies of axon down, the engine's own status, this view's own failure, the
    // staged line and the status (sweep.js footerLine).
    readonly property string footer: S.footerLine(Backend.up, topic.status, topic.errorMessage, _failure, stagedLine, status)
    readonly property var entries: _entries // [{ id, rev, name, size, disk, dir, marked, human }]

    readonly property var _scanOp: S.scanOp(topic.ops)
    property var _tree: null // the `tree` stub of the latest snapshot
    // The daemon instance _tree came from: scan numbers and node ids count within one instance.
    property string _treeInstance: ""
    property int _stagedCount: 0
    property double _stagedBytes: 0
    property var _lastCommit: null
    property string _curId: ""
    property string _message: ""
    // The message of this view's own failed or refused call or op. The footer shows it over the
    // staged line, since a failed commit or restore_all can leave items staged, until the next
    // click (every function the panel's buttons and rows call clears it) or scan (ruling,
    // 2026-10-31).
    property string _failure: ""
    property bool _autoScanned: false // set by the auto-scan, so a failing scan does not run again and again
    property var _followers: [] // { id, ended } per op this view started and waits for
    property var _entries: []
    property var _entryIndex: ({})
    // The listing holds a page of the tree loaded now. False from every change of tree, including to
    // none, until the listing's next page 0: BackendList keeps its last page while it is inactive and
    // until that page arrives, and that page is of a scan no longer loaded.
    property bool _listingCurrent: false
    readonly property var _node: _listingCurrent ? listing.extra.node ?? null : null // the listed directory
    readonly property bool _backendUp: Backend.up
    readonly property string _backendInstance: Backend.instance

    // Scans `path`, restoring every staged item first. `done(op)` gets the op once it ended.
    function scan(path, done) {
        _message = "";
        _failure = "";
        Backend.call("sweep.scan", {
            path: path,
            restore: true
        }, reply => root._follow(reply, op => {
                if (op.state === "failed")
                    root._fail(op.error.message);
                done?.(op);
            }));
    }

    function open(id) {
        _failure = "";
        _curId = id;
    }

    function up() {
        _failure = "";
        if (parentId !== null)
            _curId = parentId;
    }

    function mark(id) {
        _failure = "";
        Backend.call("sweep.mark", {
            id: id
        }, root._replyOk);
    }

    function unmark(id) {
        _failure = "";
        Backend.call("sweep.unmark", {
            id: id
        }, root._replyOk);
    }

    function restoreAll() {
        _failure = "";
        Backend.call("sweep.restore_all", {}, reply => {
            if (root._replyOk(reply))
                root._message = "restored " + reply.result.restored + " item(s)";
        });
    }

    // Deletes every staged item for good. `done(op)` gets the op once it ended.
    function commit(done) {
        _failure = "";
        Backend.call("sweep.commit", {
            confirm: true
        }, reply => root._follow(reply, op => {
                const message = op.state === "done" ? S.commitMessage(op.result, root._lastCommit) : op.error.message;
                // A commit that failed, or failed to delete an item, says so as a failure.
                if (op.state === "failed" || root._lastCommit?.failed > 0)
                    root._fail(message);
                else
                    root._message = message;
                done?.(op);
            }));
    }

    function fetchMore() {
        listing.fetchMore();
    }

    function human(bytes) {
        return S.human(bytes);
    }

    // True for an ok reply. A failed one is this view's failure, except `disconnected`, which the
    // status line and the footer already show as "axon not reachable".
    function _replyOk(reply) {
        if (reply.ok)
            return true;
        if (reply.error.code !== "disconnected")
            _fail(reply.error.message);
        return false;
    }

    // A failure of this view's own call or op: the status line and the footer show its message.
    function _fail(message) {
        _message = message;
        _failure = message;
    }

    // Section 20's op rule, which Backend leaves to the service: an op call replies with the op's id,
    // and `ended(op)` runs once, when this view sees that op `done` or `failed` in the topic's ops,
    // first in the latest snapshot and then in later ones.
    function _follow(reply, ended) {
        if (!_replyOk(reply))
            return;
        _followers = _followers.concat([
            {
                id: reply.result.op.id,
                ended: ended
            }
        ]);
        _followOps();
    }

    function _followOps() {
        const endings = [];
        const waiting = [];
        for (const follower of _followers) {
            const op = topic.ops.find(candidate => candidate.id === follower.id);
            if (op !== undefined && (op.state === "done" || op.state === "failed"))
                endings.push({
                    follower: follower,
                    op: op
                });
            else
                waiting.push(follower);
        }
        if (endings.length === 0)
            return;
        _followers = waiting;
        for (const ending of endings) {
            try { // a throwing callback must not keep the others from running
                ending.follower.ended(ending.op);
            } catch (error) {
                console.error("sweep: op callback failed:", error);
            }
        }
    }

    // Op ids belong to one daemon instance: an op this view waits for never appears in a new instance,
    // and a later op could reuse its id. So a lost connection and a new instance drop every follower
    // uncalled (ruling, 2026-10-27).
    function _dropFollowers() {
        _followers = [];
    }

    function _onSnapshot(data) {
        _applyTree(data.tree ?? null);
        _stagedCount = data.staged.count;
        _stagedBytes = data.staged.bytes;
        _lastCommit = data.last_commit ?? null;
        _followOps();
        if (_scanOp !== null) { // a scan started, this view's or another client's: what it said before is void
            _message = "";
            _failure = "";
        }
        _autoScanIfWanted();
    }

    // A new scan, none, or the first tree of a new daemon instance: the listing goes back to the root
    // of the tree now loaded, and what it holds until then is of the tree before. A new instance's
    // scan number can equal the old one's, so the instance counts too.
    function _applyTree(tree) {
        const newInstance = _treeInstance !== Backend.instance;
        const changed = newInstance || (tree?.gen ?? null) !== (_tree?.gen ?? null);
        _treeInstance = Backend.instance;
        if (changed)
            _listingCurrent = false;
        _tree = tree;
        if (!changed)
            return;
        _curId = tree?.root ?? "";
        if (tree !== null)
            _autoScanned = false;
    }

    // reset() fires before the new instance's first frame is applied: the status, ops and tree here are
    // still the old instance's. So the auto-scan is checked once that frame is in, and the tree comes
    // with the frame's snapshot. The listed node went at the new instance's hello.
    function _onReset() {
        _dropFollowers();
        _autoScanned = false;
        Qt.callLater(root._autoScanIfWanted);
    }

    function _autoScanIfWanted() {
        if (!S.autoScanWanted(panelOpen, topic.status, _tree, _scanOp, _autoScanned))
            return;
        _autoScanned = true;
        scan(scanPath, null);
    }

    // The listing's rows with their size for people. A row keeps its object while id and rev match, so
    // the panel's ScriptModel keeps its delegate. No rows while no scan is loaded, as before axon, nor
    // while the listing still holds a page of an earlier scan.
    function _applyEntries() {
        const rows = _listingCurrent ? listing.items.map(row => Object.assign({
                human: S.human(row.size)
            }, row)) : [];
        const reused = B.reuseRows(_entryIndex, rows);
        _entryIndex = reused.index;
        _entries = reused.rows;
    }

    on_ListingCurrentChanged: _applyEntries()

    onPanelOpenChanged: {
        if (panelOpen)
            _autoScanIfWanted();
        else
            _autoScanned = false;
    }

    on_BackendUpChanged: {
        if (!_backendUp)
            _dropFollowers();
    }

    // Node ids of an old daemon instance are void (section 20, "Reconnect and resubscribe"). The
    // listed node goes at the new instance's hello: its first frame can carry no data (a starting
    // engine), which syncs the topic with no new tree to take the root from. The tree stays on
    // screen until the new instance's snapshot replaces it.
    on_BackendInstanceChanged: _curId = ""

    BackendList {
        id: listing
        method: "sweep.list"
        collection: "tree"
        args: ({
                node: root._curId
            })
        collectionRev: root._tree?.rev ?? null
        pageSize: 100
        // The listed node is of the tree loaded now only once the topic is synced: after a reopen or a
        // reconnect the held tree may be one the engine released or another client replaced.
        active: root.panelOpen && root.topic.synced && root._curId !== ""
        onItemsChanged: root._applyEntries()
        // Page 0 arrived (BackendList sets its items first, then this). A change of tree changed the
        // listed node, which drops every reply to a request made before, so this page is of the tree
        // loaded now. A new daemon instance clears the listing, which lands here too, holding nothing.
        onExtraChanged: root._listingCurrent = true
        // The directory is gone (a commit removed it) or its id is from a scan no longer loaded.
        onErrorCodeChanged: {
            if (errorCode === "not_found" || errorCode === "conflict")
                root._curId = root._tree?.root ?? "";
        }
    }
}
