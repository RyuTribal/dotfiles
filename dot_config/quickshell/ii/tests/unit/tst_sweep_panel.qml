import QtQuick
import QtTest
import qs.modules.common
import qs.services
import SweepPanelUnderTest

// The real SweepPanel.qml over the real services/Sweep.qml and the stand-in Backend of
// fakes/Backend.qml: every click in the panel dismisses this view's failure, and the click still
// reaches the control under it (section 33.0). Run it through backend_tests.sh, which builds the
// modules it imports.
TestCase {
    id: tc
    name: "SweepPanel"
    when: windowShown
    // A TestCase is invisible by default, and an item under it would take no click.
    visible: true
    width: 800
    height: 600

    readonly property string twoStaged: "2 staged (1.2 K) — in trash until you delete"
    readonly property string refusal: "id is from scan 2, current scan is 3"
    property double rev: 0

    Component {
        id: panelComponent
        SweepPanel {}
    }

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
    }

    function callsOf(method) {
        return Backend.calls.filter(call => call.method === method);
    }

    // Has the latest sweep.mark refused, which is this view's failure.
    function refuseLastMark() {
        const marks = callsOf("sweep.mark");
        marks[marks.length - 1].done({
            ok: false,
            error: {
                code: "conflict",
                message: tc.refusal
            }
        });
        compare(Sweep.footer, tc.refusal);
    }

    // Opens the panel on a loaded tree with two items staged, with a refused mark showing.
    function openWithAFailure() {
        const panel = createTemporaryObject(panelComponent, tc);
        panel.panelVisible = true;
        tc.rev += 1;
        Sweep.topic._apply({
            t: "snapshot",
            topic: "sweep",
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                tree: {
                    rev: 1,
                    count: 3,
                    gen: 3,
                    root: "3:0",
                    path: "/home/test",
                    scanned_at: 1,
                    files: 2,
                    bytes: 3,
                    errors: 0,
                    skipped_mounts: 0
                },
                staged: {
                    count: 2,
                    bytes: 1280
                },
                freed_bytes: 0,
                last_commit: null,
                last_restore: null
            }
        });
        Sweep.mark("3:5");
        refuseLastMark();
        return panel;
    }

    // The delete button only opens the confirm overlay, and its cancel button only closes it.
    function test_aClickOnAControlThatCallsNothingDismissesTheFailure() {
        const panel = openWithAFailure();
        mouseClick(findChild(panel, "deleteButton"));
        verify(panel.confirming, "the click reached the button");
        compare(Sweep.footer, tc.twoStaged);

        Sweep.mark("3:6");
        refuseLastMark();
        mouseClick(findChild(panel, "cancelButton"));
        verify(!panel.confirming, "the click reached the button");
        compare(Sweep.footer, tc.twoStaged);
    }

    function test_aClickOnNoControlDismissesTheFailure_data() {
        return [
            {
                tag: "left",
                button: Qt.LeftButton
            },
            {
                tag: "right",
                button: Qt.RightButton
            },
            {
                tag: "middle",
                button: Qt.MiddleButton
            }
        ];
    }

    function test_aClickOnNoControlDismissesTheFailure(data) {
        const panel = openWithAFailure();
        mouseClick(findChild(panel, "panelWindow"), 4, 4, data.button); // the window's margin
        compare(Sweep.footer, tc.twoStaged);
    }

    // The pointer still hovers the control under it, and the click still makes its call.
    function test_aClickStillReachesAControlThatCallsSweep() {
        const panel = openWithAFailure();
        const rescan = findChild(panel, "rescanButton");
        mouseMove(rescan);
        verify(Qt.colorEqual(rescan.color, Appearance.m3colors.m3primaryContainer), "hovered");
        mouseClick(rescan);
        compare(callsOf("sweep.scan").length, 1);
        compare(callsOf("sweep.scan")[0].args, {
            path: Sweep.scanPath,
            restore: true
        });
    }
}
