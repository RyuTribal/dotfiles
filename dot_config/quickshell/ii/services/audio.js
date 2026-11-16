.pragma library

// Pure helpers for Audio.qml, the thin client of axon's audio engine (section 31, "Audio"), and for
// the bar, which reads the tooltip and the scroll step from here.

// The volume icon: off when the sink is not ready, PipeWire is down, the sink is muted or at about
// zero; down below half; up from half. `volume` is the linear 0..1 of sink.audio.volume.
function materialSymbol(ready, unavailable, muted, volume) {
    if (!ready || unavailable || muted || volume <= 0.001)
        return "volume_off";
    return volume < 0.5 ? "volume_down" : "volume_up";
}

// The percent one scroll tick moves the volume: 1 below 10 %, else 2.
function scrollStep(volumePercent) {
    return volumePercent < 10 ? 1 : 2;
}

// The OSD's message for a protection trigger reason; "" when there is none.
function protectionText(reason) {
    switch (reason) {
    case "max_step":
        return "Illegal increment";
    case "max_volume":
        return "Exceeded max allowed";
    default:
        return "";
    }
}

// The bar's volume tooltip. "Audio unavailable" while PipeWire is down and also while the topic is
// ok with no default sink (`ready` false, K4).
function tooltip(unavailable, ready, muted, volumePercent) {
    if (unavailable || !ready)
        return "Audio unavailable";
    return muted ? "Muted" : `Volume ${volumePercent}%`;
}

// A `sinks` or `sources` row as the mixer reads it.
function deviceRow(wire) {
    return {
        id: wire.id,
        rev: wire.rev,
        description: wire.description,
        isDefault: wire.default
    };
}

// A `streams` row as the mixer reads it. `audio.volume` is on the slider's 0..1 scale; mediaName and
// iconName stay null when PipeWire has none.
function streamRow(wire) {
    return {
        id: wire.id,
        rev: wire.rev,
        appName: wire.app_name,
        mediaName: wire.media_name,
        iconName: wire.icon_name,
        nodeName: wire.node_name,
        audio: {
            volume: wire.volume_percent / 100,
            muted: wire.muted
        }
    };
}
