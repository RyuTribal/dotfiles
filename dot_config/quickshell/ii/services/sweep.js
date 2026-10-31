.pragma library

// Pure helpers for services/Sweep.qml: the byte format, the scan op, the status line, the footer.

const UNITS = ["B", "K", "M", "G", "T", "P"];
const NOT_REACHABLE = "axon not reachable";

// A byte count as the sweep engine's own human() writes it, and mach's old daemon before it:
// "<n> B" below 1 KiB, else 1024-based with one decimal and a unit letter, "498.3 G". Rust's
// {:.1} rounds an exact tie to even (1280 B is "1.2 K"), while toFixed rounds it up, so a tie is
// rounded here first.
function human(bytes) {
    if (bytes < 1024)
        return bytes + " B";
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < UNITS.length - 1) {
        value /= 1024;
        unit++;
    }
    return oneDecimal(value) + " " + UNITS[unit];
}

// `value` with one decimal, an exact halfway case rounded to the even tenth. value * 20 and
// value * 10 are exact for every byte count below 1.6 PiB, because value is the count divided by a
// power of two.
function oneDecimal(value) {
    const halfway = Number.isInteger(value * 20) && !Number.isInteger(value * 10);
    if (!halfway)
        return value.toFixed(1);
    const tenthsBelow = Math.floor(value * 10);
    const evenTenths = tenthsBelow % 2 === 0 ? tenthsBelow : tenthsBelow + 1;
    return (evenTenths / 10).toFixed(1);
}

// The sweep.scan op that is queued or running in a topic's `ops`, else null.
function scanOp(ops) {
    return ops.find(op => op.method === "sweep.scan" && (op.state === "queued" || op.state === "running")) ?? null;
}

// Whether the view starts its own scan: the panel is open, the topic is synced, the engine can scan,
// nothing is loaded and no scan runs, and the auto-scan has not already run since the panel opened or
// a tree loaded. Until the topic is synced, the status, tree and ops are what the view held before a
// reopen or a reconnect, and axon may have loaded a tree or started a scan since (BackendTopic.synced).
function autoScanWanted(panelOpen, synced, topicStatus, tree, scanOp, autoScanned) {
    const canScan = topicStatus === "ok" || topicStatus === "error";
    return panelOpen && synced && canScan && tree === null && scanOp === null && !autoScanned;
}

// The panel's status line. The first case that applies wins.
function statusText(up, topicStatus, errorMessage, scanOp, message, ready) {
    if (!up)
        return NOT_REACHABLE;
    const engine = engineText(topicStatus, errorMessage);
    if (engine !== null)
        return engine;
    if (scanOp !== null)
        return scanOp.progress ? "scanning… " + scanOp.progress.text : "scanning…";
    if (message !== "")
        return message;
    return ready ? "ready" : "no scan loaded";
}

// What the engine's own status says while it starts, failed, reports an error or is disabled, else
// null (section 33.1, Offline and failure).
function engineText(topicStatus, errorMessage) {
    if (topicStatus === "starting")
        return "restoring staged items";
    if (topicStatus === "failed")
        return "sweep engine failed: run axon engine.restart name=sweep";
    if (topicStatus === "error" || topicStatus === "unconfigured")
        return errorMessage;
    return null;
}

// The footer's staged line, or "" when nothing is staged.
function stagedLine(count, bytesHuman) {
    if (count <= 0)
        return "";
    return count + " staged (" + bytesHuman + ") — in trash until you delete";
}

// The panel's footer. The first case that applies wins: axon down, the engine's own status, the
// message of this view's own failed or refused call or op, the staged line, the status line. So
// neither "axon not reachable" nor a failure hides behind the staged items (section 33.1, Offline;
// ruling, 2026-10-31, from section 33's "a failure is shown once with its cause").
function footerLine(up, topicStatus, errorMessage, failure, staged, status) {
    if (!up)
        return NOT_REACHABLE;
    const engine = engineText(topicStatus, errorMessage);
    if (engine !== null)
        return engine;
    if (failure !== "")
        return failure;
    if (staged !== "")
        return staged;
    return status;
}

// What a finished commit says: the bytes it freed and, when an item could not be deleted, how many
// failed and the first error (SW5). `lastCommit` is the topic's summary of the same commit.
function commitMessage(result, lastCommit) {
    const freed = "freed " + human(result.freed_bytes);
    if (!(lastCommit?.failed > 0))
        return freed;
    return freed + " · " + lastCommit.failed + " failed: " + lastCommit.errors[0].message;
}
