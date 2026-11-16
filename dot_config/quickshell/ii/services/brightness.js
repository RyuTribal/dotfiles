.pragma library

// Pure helpers for Brightness.qml and BrightnessOutput.qml, the thin client of axon's brightness engine
// (section 30, "brightness").

// The percent `brightness.set` sends for a slider value `v` on the 0..1 scale, clamped to 0..100. axon
// floors it at 1 %.
function percentFor(v) {
    return Math.max(0, Math.min(100, v * 100));
}

// What a monitor object shows for a topic row. `brightness` is 0 while the row has no reading
// (`percent` null); BrightnessOutput keeps its last value then.
function rowState(row) {
    return {
        available: true,
        ready: row.ready,
        brightness: row.percent === null ? 0 : row.percent / 100,
        isDdc: row.kind === "ddc",
        osdAt: row.osd_at ?? 0
    };
}
