import QtQuick
import QtTest
import Quickshell
import qs
import qs.services
import NetworkViewsUnderTest

// The real WifiDialog.qml, WifiNetworkItem.qml and NetworkToggle.qml over the real services/Network.qml,
// fed `network` frames through the stand-in Backend (section 31 network, D12, C6). backend_tests.sh
// gives it its own import root, as tst_notification_list.qml has, because its Network, Backend and
// widgets are the singletons and types the views import by name. Quickshell.execDetached records the
// command instead of starting it. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "NetworkViews"
    // A TestCase is invisible by default, and every item under it would read visible: false.
    visible: true
    when: windowShown
    width: 400
    height: 600

    Component {
        id: dialogComponent
        WifiDialog {
            width: 400
            height: 600
        }
    }

    Component {
        id: itemComponent
        WifiNetworkItem {}
    }

    Component {
        id: toggleComponent
        NetworkToggle {}
    }

    property double rev: 0

    function init() {
        Backend.calls = [];
        Backend.up = true;
        Backend.instance = "one";
        Quickshell.launched = [];
        GlobalStates.sidebarRightOpen = true;
    }

    // Network is one singleton for the whole file, so revs only grow.
    function frame(data, source) {
        tc.rev += 1;
        Network.topic._apply({
            t: "snapshot",
            topic: "network",
            rev: tc.rev,
            status: "ok",
            source: source ?? "live",
            observed_at: 1792922458200,
            error: null,
            ops: [],
            data: data
        });
    }

    function row(overrides) {
        return Object.assign({
            id: "ssid:48656d6d61",
            rev: 12,
            ssid: "Hemma",
            bssid: "A4:2B:B0:11:22:34",
            strength: 62,
            frequency_mhz: 2437,
            security: "wpa2",
            secure: true,
            active: false,
            saved: true
        }, overrides ?? {});
    }

    function networkData(overrides, wifi, rows) {
        return Object.assign({
            state: "connected_global",
            connectivity: "full",
            portal_url: "http://ping.archlinux.org/nm-check.txt",
            primary: {
                kind: "wifi",
                name: "Hemma",
                device: "wlan0"
            },
            ethernet: {
                present: false,
                connected: false,
                name: null
            },
            wifi: Object.assign({
                present: true,
                enabled: true,
                hardware_enabled: true,
                device: "wlan0",
                scanning: false,
                last_scan_at: 1792922395000,
                ssid: "Hemma",
                ap_id: "ssid:48656d6d61",
                strength: 62,
                connecting: null,
                secret_request: null,
                last_error: null
            }, wifi ?? {}),
            // A row's rev changes with any field, as axon's does, so reuseRows never keeps a row
            // whose `active` differs.
            access_points: rows ?? [row({
                    rev: 13,
                    active: true
                })]
        }, overrides ?? {});
    }

    // Section 31's other two example rows: the active Hemma_5G and the open Granne-Gues.
    function hemma5(overrides) {
        return row(Object.assign({
            id: "ssid:48656d6d615f3547",
            rev: 31,
            ssid: "Hemma_5G",
            bssid: "A4:2B:B0:11:22:33",
            strength: 82,
            frequency_mhz: 5180,
            active: true
        }, overrides ?? {}));
    }

    function granne(overrides) {
        return row(Object.assign({
            id: "ssid:4772616e6e652d47756573",
            rev: 3,
            ssid: "Granne-Gues",
            bssid: "3C:84:6A:9A:BC:DE",
            strength: 24,
            frequency_mhz: 2462,
            security: "open",
            secure: false,
            saved: false
        }, overrides ?? {}));
    }

    // The dialog's delegate at `index`, once the list made it. ListView applies a model's moves to its
    // items at its next layout, so it lays out first.
    function delegateAt(list, index) {
        list.forceLayout();
        tryVerify(() => list.count > index && list.itemAtIndex(index) !== null, 1000, "a delegate at " + index);
        return list.itemAtIndex(index);
    }

    // Fix round 2 of cutover 12: joining a network both changes its row (active) and moves it to the
    // top. ScriptModel moves the object it held and announces no change, so the delegate must still
    // show the new row.
    function test_joinedRowThatMovesShowsItsNewData() {
        frame(networkData({}, {}, [row({
                rev: 13,
                active: true
            }), granne()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "networkList");
        compare(delegateAt(list, 1).wifiNetwork.active, false);
        frame(networkData({}, {}, [granne({
                rev: 4,
                active: true
            }), row({
                rev: 14
            })]));
        const joined = delegateAt(list, 0);
        compare(joined.wifiNetwork.id, "ssid:4772616e6e652d47756573");
        compare(joined.wifiNetwork.active, true);
        compare(findChild(joined, "stateSymbol").text, "check");
        compare(delegateAt(list, 1).wifiNetwork.active, false);
    }

    // A password being typed stays, with its focus and its delegate, while another row's strength
    // changes in place and while a third row's strength bucket rises and moves it above (fix round 2).
    function test_passwordFieldSurvivesOtherRowUpdates() {
        const asking = {
            secret_request: {
                id: "r9",
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                reason: "new",
                expires_at: 1792922580000
            }
        };
        frame(networkData({}, asking, [hemma5(), row(), granne()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "networkList");
        const prompting = delegateAt(list, 1);
        compare(prompting.wifiNetwork.id, "ssid:48656d6d61");
        const field = findChild(prompting, "passwordField");
        compare(findChild(prompting, "passwordPrompt").visible, true);
        field.forceActiveFocus();
        field.text = "hunter2";
        verify(field.activeFocus, "the password field has focus");
        frame(networkData({}, asking, [hemma5({
                rev: 32,
                strength: 84
            }), row(), granne()]));
        verify(delegateAt(list, 1) === prompting, "Hemma keeps its delegate after an in-place change");
        compare(delegateAt(list, 0).wifiNetwork.strength, 84);
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus after an in-place change");
        frame(networkData({}, asking, [hemma5({
                rev: 32,
                strength: 84
            }), granne({
                rev: 4,
                strength: 90
            }), row()]));
        compare(delegateAt(list, 1).wifiNetwork.id, "ssid:4772616e6e652d47756573");
        compare(delegateAt(list, 1).wifiNetwork.strength, 90);
        verify(delegateAt(list, 2) === prompting, "Hemma keeps its delegate when another row moves");
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus when another row moves");
        // Granne is joined and moves to the top, past the first row: the view's current item would move.
        frame(networkData({}, asking, [granne({
                rev: 5,
                strength: 90,
                active: true
            }), hemma5({
                rev: 33,
                strength: 84,
                active: false
            }), row()]));
        compare(delegateAt(list, 0).wifiNetwork.active, true);
        verify(delegateAt(list, 2) === prompting, "Hemma keeps its delegate when a row moves to the top");
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus when a row moves to the top");
        compare(Backend.calls.length, 0);
    }

    function sent() {
        return Backend.calls.map(call => call.method + " " + JSON.stringify(call.args));
    }

    // Hemma's password request, for the fix round 3 tests.
    function askingHemma() {
        return {
            secret_request: {
                id: "r9",
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                reason: "new",
                expires_at: 1792922580000
            }
        };
    }

    // Each delegate of `list` shows the row at its index, by id.
    function compareDelegateIds(list, ids) {
        for (let i = 0; i < ids.length; i++)
            compare(delegateAt(list, i).wifiNetwork.id, ids[i], "delegate " + i);
    }

    // The review's case (fix round 3): in one update the row above changes in place while the rows
    // below reorder. ScriptModel's replace loop would rewrite the delegates position by position, and
    // Hemma's delegate, with "hunter2" typed and focused, would then show Granne and submit to it. Each
    // delegate must keep its own row, and Return must answer Hemma's request with the typed value.
    function test_swapWithInPlaceAbove() {
        frame(networkData({}, askingHemma(), [hemma5(), row(), granne()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "networkList");
        const above = delegateAt(list, 0);
        const prompting = delegateAt(list, 1);
        const below = delegateAt(list, 2);
        const field = findChild(prompting, "passwordField");
        field.forceActiveFocus();
        field.text = "hunter2";
        frame(networkData({}, askingHemma(), [hemma5({
                rev: 32,
                strength: 84
            }), granne({
                rev: 4,
                strength: 90
            }), row()]));
        compareDelegateIds(list, ["ssid:48656d6d615f3547", "ssid:4772616e6e652d47756573", "ssid:48656d6d61"]);
        verify(delegateAt(list, 0) === above, "Hemma_5G keeps its delegate");
        verify(delegateAt(list, 1) === below, "Granne keeps its delegate");
        verify(delegateAt(list, 2) === prompting, "Hemma keeps its delegate");
        compare(above.wifiNetwork.strength, 84);
        compare(below.wifiNetwork.strength, 90);
        compare(prompting.wifiNetwork.id, "ssid:48656d6d61");
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus");
        keyClick("x");
        keyClick(Qt.Key_Return);
        compare(sent(), ['network.submit_secret {"request":"r9","password":"hunter2x"}']);
    }

    // In one update the top row changes in place, a row is added, one is removed and Granne rises above
    // Hemma: every delegate still shows its own row and the typed password stays with Hemma.
    function test_addAndRemoveWithReorder() {
        const gamla = row({
            id: "ssid:47616d6c61",
            rev: 8,
            ssid: "Gamla",
            bssid: "10:20:30:40:50:61",
            strength: 10
        });
        frame(networkData({}, askingHemma(), [hemma5(), row(), granne(), gamla]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "networkList");
        const above = delegateAt(list, 0);
        const prompting = delegateAt(list, 1);
        const rising = delegateAt(list, 2);
        const field = findChild(prompting, "passwordField");
        field.forceActiveFocus();
        field.text = "hunter2";
        const kafe = row({
            id: "ssid:4b616665",
            rev: 7,
            ssid: "Kafe",
            bssid: "10:20:30:40:50:60",
            strength: 95
        });
        frame(networkData({}, askingHemma(), [hemma5({
                rev: 32,
                strength: 84
            }), kafe, granne({
                rev: 4,
                strength: 90
            }), row()]));
        compareDelegateIds(list, ["ssid:48656d6d615f3547", "ssid:4b616665", "ssid:4772616e6e652d47756573", "ssid:48656d6d61"]);
        compare(list.count, 4);
        verify(delegateAt(list, 0) === above, "Hemma_5G keeps its delegate");
        verify(delegateAt(list, 2) === rising, "Granne keeps its delegate");
        verify(delegateAt(list, 3) === prompting, "Hemma keeps its delegate");
        compare(above.wifiNetwork.strength, 84);
        compare(rising.wifiNetwork.strength, 90);
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus");
    }

    // Hemma's own row changes twice while a password is typed, and NetworkManager asks again under a new
    // request id (reason wrong_password): the text and focus stay, and Return answers the new request
    // (fix round 4).
    function test_ownRowChangeKeepsTypedValue() {
        frame(networkData({}, askingHemma(), [hemma5(), row(), granne()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "networkList");
        const prompting = delegateAt(list, 1);
        const field = findChild(prompting, "passwordField");
        field.forceActiveFocus();
        field.text = "hunter2";
        frame(networkData({}, askingHemma(), [hemma5(), row({
                rev: 13,
                strength: 64
            }), granne()]));
        verify(delegateAt(list, 1) === prompting, "Hemma keeps its delegate");
        compare(prompting.wifiNetwork.strength, 64);
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus");
        const again = askingHemma();
        again.secret_request.id = "r10";
        again.secret_request.reason = "wrong_password";
        frame(networkData({}, again, [hemma5(), row({
                rev: 14,
                strength: 70
            }), granne()]));
        compare(prompting.wifiNetwork.strength, 70);
        compare(field.text, "hunter2");
        verify(field.activeFocus, "the field keeps focus after the request is asked again");
        keyClick(Qt.Key_Return);
        compare(sent(), ['network.submit_secret {"request":"r10","password":"hunter2"}']);
    }

    // A field whose prompt is not open cannot act (fix round 3): Return and Cancel send nothing and
    // each warns once; a prompt that closes clears its field and gives up focus.
    function test_hiddenFieldCannotSubmit() {
        frame(networkData({}, askingHemma(), [hemma5(), row(), granne()]));
        const item = createTemporaryObject(itemComponent, tc, {
            wifiNetwork: Network.wifiNetworks[1]
        });
        const field = findChild(item, "passwordField");
        compare(findChild(item, "passwordPrompt").visible, true);
        field.forceActiveFocus();
        field.text = "hunter2";
        frame(networkData({}, {}, [hemma5(), row(), granne()]));
        item.wifiNetwork = Network.wifiNetworks[1];
        compare(findChild(item, "passwordPrompt").visible, false);
        compare(field.text, "", "the field is cleared when its prompt closes");
        compare(field.activeFocus, false);
        field.text = "hunter2";
        ignoreWarning(/WifiNetworkItem: .*ssid:48656d6d61.* prompt is not open/);
        field.accepted();
        ignoreWarning(/WifiNetworkItem: .*ssid:48656d6d61.* prompt is not open/);
        findChild(item, "cancelButton").clicked();
        compare(Backend.calls.length, 0);
    }

    function test_noMethodCallOnLoad() {
        frame(networkData({}, {
            secret_request: {
                id: "r1",
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                reason: "new",
                expires_at: 1792922580000
            }
        }));
        const dialog = createTemporaryObject(dialogComponent, tc);
        createTemporaryObject(toggleComponent, tc);
        createTemporaryObject(itemComponent, tc, {
            wifiNetwork: Network.wifiNetworks[0]
        });
        wait(0);
        // The dialog's delegate, a WifiNetworkItem with its required row, was made for the row.
        const list = findChild(dialog, "networkList");
        tryVerify(() => list.itemAtIndex(0) !== null, 1000, "a delegate for the row");
        compare(list.itemAtIndex(0).wifiNetwork.id, "ssid:48656d6d61");
        compare(Backend.calls.length, 0);
        compare(Quickshell.launched, []);
    }

    function test_lastErrorLineShown() {
        frame(networkData());
        const dialog = createTemporaryObject(dialogComponent, tc);
        const line = findChild(dialog, "lastErrorLine");
        verify(line !== null);
        verify(dialog.children[1] === line, "the line sits right under the title");
        compare(line.visible, false);
        frame(networkData({}, {
            last_error: {
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                code: "wrong_password",
                message: "Secrets were required, but not provided",
                at: 1792922470000
            }
        }));
        compare(line.visible, true);
        compare(line.text, "Hemma: Wrong password");
        compare(line.wrapMode, Text.NoWrap);
        compare(line.elide, Text.ElideRight);
        frame(networkData({}, {
            last_error: {
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                code: "something_new",
                message: "an NM message",
                at: 1792922470000
            }
        }));
        compare(line.text, "Hemma: an NM message");
        frame(networkData());
        compare(line.visible, false);
    }

    // WifiNetworkItem.qml:13 and :54 read passwordApId; :80, the Cancel button, cancels the request.
    function test_itemPromptFollowsPasswordApId() {
        frame(networkData());
        const item = createTemporaryObject(itemComponent, tc, {
            wifiNetwork: Network.wifiNetworks[0]
        });
        const prompt = findChild(item, "passwordPrompt");
        compare(prompt.visible, false);
        frame(networkData({}, {
            secret_request: {
                id: "r2",
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                reason: "wrong_password",
                expires_at: 1792922580000
            }
        }));
        item.wifiNetwork = Network.wifiNetworks[0];
        compare(prompt.visible, true);
        compare(item.active, true);
        findChild(item, "cancelButton").clicked();
        compare(Backend.calls.map(call => call.method + " " + JSON.stringify(call.args)), ['network.cancel_secret {"request":"r2"}']);
    }

    // WifiNetworkItem.qml:45: the row being joined shows the connecting symbol.
    function test_itemConnectingSymbol() {
        frame(networkData({}, {
            connecting: {
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                since: 1792922455000
            }
        }, [row()]));
        const item = createTemporaryObject(itemComponent, tc, {
            wifiNetwork: Network.wifiNetworks[0]
        });
        compare(findChild(item, "stateSymbol").text, "settings_ethernet");
        frame(networkData({}, {}, [row()]));
        compare(findChild(item, "stateSymbol").text, "lock");
    }

    // WifiNetworkItem.qml:96 and :106: the portal button shows on the active row while NM reports a
    // portal, whatever the security, and opens portal_url from the view.
    function test_itemPortalButton() {
        frame(networkData());
        const item = createTemporaryObject(itemComponent, tc, {
            wifiNetwork: Network.wifiNetworks[0]
        });
        const portal = findChild(item, "publicWifiPortal");
        compare(portal.visible, false);
        frame(networkData({
            connectivity: "portal",
            portal_url: "http://portal.example/login"
        }));
        item.wifiNetwork = Network.wifiNetworks[0];
        compare(portal.visible, true);
        findChild(item, "portalButton").clicked();
        compare(Quickshell.launched, [["xdg-open", "http://portal.example/login"]]);
        compare(GlobalStates.sidebarRightOpen, false);
        compare(Backend.calls.length, 0);
    }

    // NetworkToggle.openDetails(), the sidebar's fallback without Wi-Fi: apps.networkEthernet on a
    // wired connection, else apps.network.
    function test_openDetailsLaunchesByConnection() {
        frame(networkData());
        const toggle = createTemporaryObject(toggleComponent, tc);
        toggle.openDetails();
        frame(networkData({
            ethernet: {
                present: true,
                connected: true,
                name: "Wired"
            }
        }));
        toggle.openDetails();
        compare(Quickshell.launched, [["bash", "-c", "network-details"], ["bash", "-c", "ethernet-details"]]);
        compare(GlobalStates.sidebarRightOpen, false);
        compare(Backend.calls.length, 0);
    }
}
