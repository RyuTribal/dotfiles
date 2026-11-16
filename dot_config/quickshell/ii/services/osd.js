.pragma library

// The OSD deadline rule OsdDeadline.qml keeps for the volume and the brightness OSD (section 20, the
// one-shot OSD timer).

// Whether an OSD shows at `now`, and until when. The engine publishes only `osd_at`, the instant of the
// last change that shows the OSD; the view keeps it for `timeoutMs` (Config osd.timeout). A run is one
// stretch of the OSD on screen: it starts at the first osd_at while hidden (`runStart` 0) and lasts
// while it shows. The protection message shows for a trigger inside the current run. `hiddenAt` is the
// osd_at that hideOsd() hid: that one and every earlier one stay hidden. osdAt, runStart and hiddenAt
// are ms, 0 or null meaning none. Returns { visible, deadline, runStart, reason }: deadline -1 and
// runStart 0 while hidden, reason a protection reason or "".
function osdState(osdAt, timeoutMs, now, runStart, hiddenAt, lastTrigger) {
    const hidden = {
        visible: false,
        deadline: -1,
        runStart: 0,
        reason: ""
    };
    if (!(osdAt > 0) || osdAt <= (hiddenAt ?? 0))
        return hidden;
    const deadline = osdAt + timeoutMs;
    if (now >= deadline)
        return hidden;
    const start = runStart > 0 ? runStart : osdAt;
    return {
        visible: true,
        deadline: deadline,
        runStart: start,
        reason: lastTrigger && lastTrigger.at >= start ? lastTrigger.reason : ""
    };
}
