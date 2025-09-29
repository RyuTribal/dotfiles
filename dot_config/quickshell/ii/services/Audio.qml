pragma Singleton
pragma ComponentBehavior: Bound
import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Services.Pipewire

/**
 * A nice wrapper for default Pipewire audio sink and source.
 */
Singleton {
    id: root

    // ------- Core nodes -------
    property PwNode sink: Pipewire.defaultAudioSink
    property PwNode source: Pipewire.defaultAudioSource
    property bool ready: sink?.ready ?? false

    // ------- UI-facing icon (Material Symbols) -------
    // volume_off  : muted or zero
    // volume_down : low
    // volume_up   : medium & high
    property string materialSymbol: {
        if (!ready || !sink?.audio)
            return "volume_off";
        if (sink.audio.muted)
            return "volume_off";
        const v = Number(sink.audio.volume); // 0..1
        if (!isFinite(v) || v <= 0.001)
            return "volume_off";
        if (v < 0.50)
            return "volume_down";
        if (v < 0.67)
            return "volume_up";
        return "volume_up";
    }

    // Useful for sliders or OSDs
    property real volumePercent: {
        if (!ready || !sink?.audio)
            return 0;
        const v = Number(sink.audio.volume);
        return Math.max(0, Math.min(100, Math.round(v * 100)));
    }

    // Expose a simple “isMuted” boolean
    property bool isMuted: ready && sink?.audio ? sink.audio.muted : true

    signal sinkProtectionTriggered(string reason)

    // Ensure PipeWire objects are bound so audio props are valid
    PwObjectTracker {
        objects: [sink, source]
    }

    // ------- Helper API -------
    function setVolumePercent(p) {
        if (!ready || !sink?.audio)
            return;
        const clamped = Math.max(0, Math.min(100, Number(p)));
        sink.audio.volume = clamped / 100;
    }

    function nudgeVolume(deltaPercent) {
        if (!ready || !sink?.audio)
            return;
        const cur = volumePercent;
        setVolumePercent(cur + Number(deltaPercent));
    }

    function setVolumeLinear(v) { // v in [0..1]
        if (!ready || !sink?.audio)
            return;
        const clamped = Math.max(0, Math.min(1, Number(v)));
        sink.audio.volume = clamped;
    }

    function toggleMute() {
        if (!ready || !sink?.audio)
            return;
        sink.audio.muted = !sink.audio.muted;
    }

    function setMuted(m) {
        if (!ready || !sink?.audio)
            return;
        sink.audio.muted = !!m;
    }

    // ------- Protection against sudden volume changes (yours, kept) -------
    Connections {
        target: sink?.audio ?? null
        property bool lastReady: false
        property real lastVolume: 0
        function onVolumeChanged() {
            if (!Config.options.audio.protection.enable)
                return;
            if (!lastReady) {
                lastVolume = sink.audio.volume;
                lastReady = true;
                return;
            }
            const newVolume = sink.audio.volume;
            const maxAllowedIncrease = Config.options.audio.protection.maxAllowedIncrease / 100;
            const maxAllowed = Config.options.audio.protection.maxAllowed / 100;

            if (newVolume - lastVolume > maxAllowedIncrease) {
                sink.audio.volume = lastVolume;
                root.sinkProtectionTriggered("Illegal increment");
            } else if (newVolume > maxAllowed) {
                root.sinkProtectionTriggered("Exceeded max allowed");
                sink.audio.volume = Math.min(lastVolume, maxAllowed);
            }
            if (sink.ready && (isNaN(sink.audio.volume) || sink.audio.volume === undefined || sink.audio.volume === null)) {
                sink.audio.volume = 0;
            }
            lastVolume = sink.audio.volume;
        }
    }
}
