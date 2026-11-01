import QtQuick
import QtTest
import "../../services/hypr.js" as H

// The pure parts of services/Hypr.qml. Needs nothing else:
// QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/unit/tst_hypr.qml
TestCase {
    name: "Hypr"

    // Window rows as the hypr topic publishes them (section 30, ruling D1), trimmed to the fields
    // these functions read.
    readonly property var kitty: ({
            id: "stableid:18000009",
            rev: 3,
            class: "kitty",
            title: "window 1",
            monitor: "eDP-1",
            workspace: 6,
            workspace_name: "6"
        })
    readonly property var browser: ({
            id: "stableid:1800000d",
            rev: 1,
            class: "brave-browser",
            title: "window 2",
            monitor: "eDP-1",
            workspace: 6,
            workspace_name: "6"
        })
    readonly property var discord: ({
            id: "stableid:18000008",
            rev: 7,
            class: "discord",
            title: "window 3",
            monitor: "HDMI-A-1",
            workspace: 5,
            workspace_name: "5"
        })
    readonly property var lostWindow: ({
            id: "stableid:1800000e",
            rev: 1,
            class: "kitty",
            title: "window 4",
            // The batch listed no monitor with the client's id.
            monitor: null,
            workspace: 6,
            workspace_name: "6"
        })

    function test_indexByFindsRowsByKey() {
        const workspaces = [
            {
                id: "5",
                ws_id: 5,
                name: "5"
            },
            {
                id: "notes",
                ws_id: null,
                name: "notes"
            },
            {
                id: "special:magic",
                ws_id: null,
                name: "special:magic"
            }
        ];
        const byNumber = H.indexBy(workspaces, "ws_id");
        verify(byNumber[5] === workspaces[0]);
        compare(byNumber[6], undefined);
        compare(byNumber[null], undefined, "a null number never matches (ruling D1)");
        compare(Object.keys(byNumber).length, 1, "rows without the key are left out");
        const byId = H.indexBy(workspaces, "id");
        verify(byId["special:magic"] === workspaces[2]);
        verify(byId["notes"] === workspaces[1]);
        compare(byId["constructor"], undefined, "the index has no prototype keys");
    }

    function test_barTitleUsesTheActiveWindowOnThisMonitor() {
        compare(H.barTitle(kitty, "eDP-1", browser, "Desktop", "Workspace 6"), {
            subtitle: "kitty",
            title: "window 1"
        });
    }

    function test_barTitleFallsBackToTheIconWindow() {
        compare(H.barTitle(discord, "eDP-1", browser, "Desktop", "Workspace 6"), {
            subtitle: "brave-browser",
            title: "window 2"
        }, "the active window is on another monitor");
    }

    function test_barTitleDesktopAndWorkspace() {
        const desktop = {
            subtitle: "Desktop",
            title: "Workspace 6"
        };
        compare(H.barTitle(null, "eDP-1", null, "Desktop", "Workspace 6"), desktop);
        compare(H.barTitle(discord, "eDP-1", null, "Desktop", "Workspace 6"), desktop, "the active window is on another monitor");
        // The bar passes monitor?.id, undefined until the first snapshot; a window without a
        // monitor has null there, which is no match.
        compare(H.barTitle(lostWindow, undefined, null, "Desktop", "Workspace 6"), desktop);
    }

    function test_inGroup() {
        for (let workspace = 1; workspace <= 10; workspace++)
            verify(H.inGroup(workspace, 0, 10), `workspace ${workspace} is in group 0`);
        verify(!H.inGroup(11, 0, 10));
        verify(!H.inGroup(0, 0, 10));
        verify(H.inGroup(11, 1, 10));
        verify(!H.inGroup(10, 1, 10));
        verify(!H.inGroup(null, 0, 10), "a scratchpad window's null number is in no group (E5)");
    }
}
