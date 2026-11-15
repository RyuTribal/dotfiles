import QtQuick
import QtTest
import "../../services/audio.js" as AudioJs

// services/audio.js: the pure helpers the Audio facade and the bar build their Parity names from
// (section 31 audio). Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "Audio"

    function test_materialSymbol() {
        compare(AudioJs.materialSymbol(false, false, false, 0.7), "volume_off", "not ready");
        compare(AudioJs.materialSymbol(true, true, false, 0.7), "volume_off", "unavailable");
        compare(AudioJs.materialSymbol(true, false, true, 0.7), "volume_off", "muted");
        compare(AudioJs.materialSymbol(true, false, false, 0.001), "volume_off");
        compare(AudioJs.materialSymbol(true, false, false, 0.49), "volume_down");
        compare(AudioJs.materialSymbol(true, false, false, 0.5), "volume_up");
        compare(AudioJs.materialSymbol(true, false, false, 0.9), "volume_up");
    }

    function test_scrollStep() {
        compare(AudioJs.scrollStep(8), 1);
        compare(AudioJs.scrollStep(9), 1);
        compare(AudioJs.scrollStep(10), 2);
        compare(AudioJs.scrollStep(50), 2);
    }

    function test_protectionText() {
        compare(AudioJs.protectionText("max_step"), "Illegal increment");
        compare(AudioJs.protectionText("max_volume"), "Exceeded max allowed");
        compare(AudioJs.protectionText(null), "");
    }

    function test_osdState() {
        // osd 1000, timeout 1000: shown until 2000.
        const shown = AudioJs.osdState(1000, 1000, 1500, 0, 0, null);
        compare(shown.visible, true);
        compare(shown.deadline, 2000);
        compare(shown.runStart, 1000);
        compare(shown.reason, "");
        const ended = AudioJs.osdState(1000, 1000, 2000, shown.runStart, 0, null);
        compare(ended.visible, false);
        compare(ended.runStart, 0);

        // A new osd after the run ended starts a new run.
        const next = AudioJs.osdState(2500, 1000, 2500, ended.runStart, 0, null);
        compare(next.visible, true);
        compare(next.runStart, 2500);
        compare(next.deadline, 3500);

        // A trigger inside the run gives its reason, one before the run gives none.
        const inside = AudioJs.osdState(1200, 1000, 1200, 1000, 0, {
            reason: "max_step",
            at: 1200
        });
        compare(inside.visible, true);
        compare(inside.runStart, 1000);
        compare(inside.reason, "max_step");
        const before = AudioJs.osdState(1200, 1000, 1200, 1000, 0, {
            reason: "max_volume",
            at: 900
        });
        compare(before.visible, true);
        compare(before.reason, "");

        // hiddenAt hides that osd and every earlier one; a later osd shows again.
        const hidden = AudioJs.osdState(1000, 1000, 1100, 1000, 1000, null);
        compare(hidden.visible, false);
        compare(hidden.runStart, 0);
        const again = AudioJs.osdState(1300, 1000, 1300, hidden.runStart, 1000, null);
        compare(again.visible, true);
        compare(again.runStart, 1300);

        // Before the first osd of this daemon instance nothing shows.
        compare(AudioJs.osdState(0, 1000, 1500, 0, 0, null).visible, false);
        compare(AudioJs.osdState(null, 1000, 1500, 0, 0, null).visible, false);
    }

    function test_deviceRow() {
        const row = AudioJs.deviceRow({
            id: "alsa_output.speaker",
            rev: 4,
            description: "Speaker",
            default: true
        });
        compare(row.id, "alsa_output.speaker");
        compare(row.rev, 4);
        compare(row.description, "Speaker");
        compare(row.isDefault, true);
        compare(AudioJs.deviceRow({
            id: "hdmi",
            rev: 1,
            description: "HDMI",
            default: false
        }).isDefault, false);
    }

    function test_streamRow() {
        const row = AudioJs.streamRow({
            id: "4127",
            rev: 9,
            app_name: "Brave",
            media_name: null,
            icon_name: "brave-browser",
            node_name: "Brave",
            volume_percent: 50,
            muted: true
        });
        compare(row.id, "4127");
        compare(row.rev, 9);
        compare(row.appName, "Brave");
        compare(row.mediaName, null);
        compare(row.iconName, "brave-browser");
        compare(row.nodeName, "Brave");
        compare(row.audio.volume, 0.5);
        compare(row.audio.muted, true);
    }

    function test_tooltip() {
        compare(AudioJs.tooltip(true, false, false, 42), "Audio unavailable");
        // Not unavailable and not ready: the topic is ok with `sink: null` (K4).
        compare(AudioJs.tooltip(false, false, false, 42), "Audio unavailable");
        compare(AudioJs.tooltip(false, true, true, 42), "Muted");
        compare(AudioJs.tooltip(false, true, false, 42), "Volume 42%");
    }
}
