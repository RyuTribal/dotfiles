import QtQuick
import QtTest
import qs.services
import SweepPanelUnderTest

// The real SweepPanel.qml's IPC toggle: it closes only a panel whose window drew a frame since it
// opened, so a second press that waited behind a busy shell does not close a panel nobody saw. The
// stand-in FloatingWindow is a Rectangle in this test's own window, which draws the frames. Run it
// through backend_tests.sh, which builds the modules it imports.
TestCase {
    id: tc
    name: "SweepPanelToggle"
    when: windowShown
    visible: true
    width: 800
    height: 600

    Component {
        id: panelComponent
        SweepPanel {}
    }

    function ipcOf(panel) {
        for (let i = 0; i < panel.resources.length; i++)
            if (panel.resources[i].target === "sweep")
                return panel.resources[i];
        return null;
    }

    // Two presses queued behind a busy shell run back to back, before any frame.
    function test_aSecondToggleBeforeThePanelWasShownKeepsItOpen() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        verify(ipc !== null, "the panel has its sweep IpcHandler");
        ipc.toggle();
        ipc.toggle();
        verify(panel.panelVisible, "the second press closed a panel that never showed");
    }

    function test_aToggleAfterThePanelWasShownClosesIt() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        ipc.toggle();
        verify(panel.panelVisible);
        waitForRendering(tc);
        wait(50);
        ipc.toggle();
        verify(!panel.panelVisible, "a press after the panel showed closes it");
    }

    // A close forgets that the panel showed: the reopened panel's new window has drawn nothing yet.
    function test_aReopenedPanelIsNotShownUntilItsWindowDraws() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        ipc.toggle();
        waitForRendering(tc);
        wait(50);
        ipc.toggle();
        verify(!panel.panelVisible);
        ipc.toggle();
        ipc.toggle();
        verify(panel.panelVisible, "the second press closed a reopened panel that never showed");
    }

    // A frame the closing window drew arrives after the close (queued from the render thread), or
    // even after a reopen. It shows neither the closed panel nor the reopened one, whose new window
    // has drawn nothing yet. The stand-in window leaves the test window at once, so the test hands
    // that late frame to the panel itself.
    function test_aFrameOfTheClosedWindowDoesNotShowTheNextOpen() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        ipc.toggle();
        waitForRendering(tc);
        wait(50);
        const closing = findChild(panel, "panelWindow");
        ipc.toggle();
        verify(!panel.panelVisible);
        panel.frameDrawn(closing);
        verify(!panel.panelShown, "a frame of the closed window showed the closed panel");
        ipc.toggle();
        panel.frameDrawn(closing);
        verify(!panel.panelShown, "a frame of the old window showed the reopened panel");
        ipc.toggle();
        verify(panel.panelVisible, "the second press closed a reopened panel that never showed");
    }

    function test_closingTheWindowClosesAPanelThatHasNotShownYet() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        ipc.toggle();
        findChild(panel, "panelWindow").visible = false;
        verify(!panel.panelVisible, "closing the window closes the panel whether it showed or not");
    }

    function test_hideClosesAPanelThatHasNotShownYet() {
        const panel = createTemporaryObject(panelComponent, tc);
        const ipc = ipcOf(panel);
        ipc.toggle();
        ipc.hide();
        verify(!panel.panelVisible, "hide closes the panel whether it showed or not");
    }
}
