import QtQuick
import QtTest
import "../../services/osd.js" as OsdJs

// services/osd.js: the OSD deadline rule the volume and brightness OSDs share (section 20, the one-shot
// OSD timer). Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "Osd"

    function test_osdState() {
        // osd 1000, timeout 1000: shown until 2000.
        const shown = OsdJs.osdState(1000, 1000, 1500, 0, 0, null);
        compare(shown.visible, true);
        compare(shown.deadline, 2000);
        compare(shown.runStart, 1000);
        compare(shown.reason, "");
        const ended = OsdJs.osdState(1000, 1000, 2000, shown.runStart, 0, null);
        compare(ended.visible, false);
        compare(ended.runStart, 0);

        // A new osd after the run ended starts a new run.
        const next = OsdJs.osdState(2500, 1000, 2500, ended.runStart, 0, null);
        compare(next.visible, true);
        compare(next.runStart, 2500);
        compare(next.deadline, 3500);

        // A trigger inside the run gives its reason, one before the run gives none.
        const inside = OsdJs.osdState(1200, 1000, 1200, 1000, 0, {
            reason: "max_step",
            at: 1200
        });
        compare(inside.visible, true);
        compare(inside.runStart, 1000);
        compare(inside.reason, "max_step");
        const before = OsdJs.osdState(1200, 1000, 1200, 1000, 0, {
            reason: "max_volume",
            at: 900
        });
        compare(before.visible, true);
        compare(before.reason, "");

        // hiddenAt hides that osd and every earlier one; a later osd shows again.
        const hidden = OsdJs.osdState(1000, 1000, 1100, 1000, 1000, null);
        compare(hidden.visible, false);
        compare(hidden.runStart, 0);
        const again = OsdJs.osdState(1300, 1000, 1300, hidden.runStart, 1000, null);
        compare(again.visible, true);
        compare(again.runStart, 1300);

        // Before the first osd of this daemon instance nothing shows.
        compare(OsdJs.osdState(0, 1000, 1500, 0, 0, null).visible, false);
        compare(OsdJs.osdState(null, 1000, 1500, 0, 0, null).visible, false);
    }
}
