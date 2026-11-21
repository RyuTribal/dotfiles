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
