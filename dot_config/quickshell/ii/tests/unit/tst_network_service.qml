import QtQuick
import QtTest
import ShellUnderTest

// The real services/Network.qml over the stand-in Backend of fakes/Backend.qml: the rows it maps from
// the `network` topic, the ids of the row being joined and of the row asking for a password, and the
// calls its functions send (section 31 network, D12). backend_tests.sh copies it without its
// `pragma Singleton` line into ShellUnderTest as NetworkService, beside backend.js, so each test makes
// its own instance. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "NetworkService"

    Component {
        id: networkComponent
        NetworkService {}
    }

    // Stands in for NetworkToggle in the sidebar's alt action: only openDetails(), counted.
    Component {
        id: toggleComponent
        QtObject {
            property int details: 0
            function openDetails() {
                details += 1;
            }
        }
    }

    property double rev: 0

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 1792922460000;
        Backend.downSince = 0;
        tc.rev = 0;
    }

    function frame(topic, status, data, source, error) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: source ?? "live",
            observed_at: 1792922458200,
            error: error ?? null,
            ops: [],
            data: data
        });
    }

    // Section 31's example rows.
    function hemma5(overrides) {
        return Object.assign({
            id: "ssid:48656d6d615f3547",
            rev: 31,
            ssid: "Hemma_5G",
            bssid: "A4:2B:B0:11:22:33",
            strength: 82,
            frequency_mhz: 5180,
            security: "wpa2",
            secure: true,
            active: true,
            saved: true
        }, overrides ?? {});
    }

    function hemma(overrides) {
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

    function granne(overrides) {
        return Object.assign({
            id: "ssid:4772616e6e652d47756573",
            rev: 3,
            ssid: "Granne-Gues",
            bssid: "3C:84:6A:9A:BC:DE",
            strength: 24,
            frequency_mhz: 2462,
            security: "open",
            secure: false,
            active: false,
            saved: false
        }, overrides ?? {});
    }

    // Section 31's example data; `wifi` overrides go into the wifi object, the rest into the top.
    function networkData(overrides, wifi, rows) {
        return Object.assign({
            state: "connected_global",
            connectivity: "full",
            portal_url: "http://ping.archlinux.org/nm-check.txt",
            primary: {
                kind: "wifi",
                name: "Hemma_5G",
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
                ssid: "Hemma_5G",
                ap_id: "ssid:48656d6d615f3547",
                strength: 82,
                connecting: null,
                secret_request: null,
                last_error: null
            }, wifi ?? {}),
            access_points: rows ?? [hemma5(), hemma(), granne()]
        }, overrides ?? {});
    }

    function request(apId, ssid) {
        return {
            id: "r7",
            ap_id: apId,
            ssid: ssid,
            reason: "new",
            expires_at: 1792922580000
        };
    }

    function sent() {
        return Backend.calls.map(call => call.method + " " + JSON.stringify(call.args));
    }

    function test_rowsMapFieldNames() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        compare(network.wifiNetworks.length, 3);
        const row = network.wifiNetworks[0];
        compare(row.id, "ssid:48656d6d615f3547");
        compare(row.rev, 31);
        compare(row.ssid, "Hemma_5G");
        compare(row.bssid, "A4:2B:B0:11:22:33");
        compare(row.strength, 82);
        compare(row.frequency, 5180);
        compare(row.active, true);
        compare(row.saved, true);
        compare(row.security, "wpa2");
        compare(row.isSecure, true);
        const open = network.wifiNetworks[2];
        compare(open.ssid, "Granne-Gues");
        compare(open.frequency, 2462);
        compare(open.security, "");
        compare(open.isSecure, false);
        compare(open.saved, false);
        compare(network.networkName, "Hemma_5G");
        compare(network.wifiEnabled, true);
        compare(network.wifiPresent, true);
        compare(network.ethernet, false);
    }

    function test_reuseRowsKeepsObjects() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        const first = network.wifiNetworks[0];
        const second = network.wifiNetworks[1];
        frame(network.topic, "ok", networkData({}, {}, [hemma5(), hemma({
                rev: 13,
                strength: 58
            }), granne()]));
        verify(network.wifiNetworks[0] === first);
        verify(network.wifiNetworks[1] !== second);
        compare(network.wifiNetworks[1].strength, 58);
    }

    // A delegate compares its row's id with connectingApId and passwordApId: neither may name an id
    // whose row is not in wifiNetworks at any change notification.
    function test_noIdWithoutRow() {
        const network = createTemporaryObject(networkComponent, tc);
        const missing = [];
        const check = () => {
            for (const id of [network.connectingApId, network.passwordApId]) {
                if (id !== "" && !network.wifiNetworks.some(row => row.id === id))
                    missing.push(id);
            }
        };
        network.wifiNetworksChanged.connect(check);
        network.connectingApIdChanged.connect(check);
        network.passwordApIdChanged.connect(check);
        const hemmaId = "ssid:48656d6d61";
        const asking = {
            connecting: {
                ap_id: hemmaId,
                ssid: "Hemma",
                since: 1792922455000
            },
            secret_request: request(hemmaId, "Hemma")
        };
        frame(network.topic, "ok", networkData({}, asking));
        compare(network.passwordApId, hemmaId);
        compare(network.connectingApId, hemmaId);
        frame(network.topic, "ok", networkData({}, asking, [hemma5(), granne()]));
        compare(network.passwordApId, "");
        compare(network.connectingApId, "");
        frame(network.topic, "ok", networkData({}, asking));
        frame(network.topic, "ok", networkData({}, {}, []));
        network.wifiNetworksChanged.disconnect(check);
        network.connectingApIdChanged.disconnect(check);
        network.passwordApIdChanged.disconnect(check);
        compare(missing, []);
        compare(network.wifiNetworks, []);
    }

    function test_materialSymbolThresholds() {
        const network = createTemporaryObject(networkComponent, tc);
        const cases = [[81, "signal_wifi_4_bar"], [80, "network_wifi_3_bar"], [61, "network_wifi_3_bar"], [60, "network_wifi_2_bar"], [41, "network_wifi_2_bar"], [40, "network_wifi_1_bar"], [21, "network_wifi_1_bar"], [20, "signal_wifi_0_bar"], [null, "signal_wifi_0_bar"]];
        for (const [strength, symbol] of cases) {
            frame(network.topic, "ok", networkData({}, {
                strength: strength
            }));
            compare(network.materialSymbol, symbol, "strength " + strength);
        }
        frame(network.topic, "ok", networkData({}, {
            enabled: false,
            strength: null
        }));
        compare(network.materialSymbol, "signal_wifi_off");
        frame(network.topic, "ok", networkData({
            primary: {
                kind: "ethernet",
                name: "Wired",
                device: "enp4s0"
            },
            ethernet: {
                present: true,
                connected: true,
                name: "Wired"
            }
        }, {
            enabled: false
        }));
        compare(network.ethernet, true);
        compare(network.materialSymbol, "lan");
        compare(network.networkName, "Wired");
    }

    function test_passwordApIdFromSecretRequest() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        compare(network.passwordApId, "");
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        compare(network.passwordApId, "ssid:48656d6d61");
        frame(network.topic, "ok", networkData());
        compare(network.passwordApId, "");
    }

    // A request in a persisted snapshot belongs to a daemon that no longer runs (D11).
    function test_persistedSecretRequestIgnored() {
        const network = createTemporaryObject(networkComponent, tc);
        const pending = {
            connecting: {
                ap_id: "ssid:48656d6d61",
                ssid: "Hemma",
                since: 1792922455000
            },
            secret_request: request("ssid:48656d6d61", "Hemma")
        };
        frame(network.topic, "starting", networkData({}, pending), "persisted");
        compare(network.wifiNetworks.length, 3);
        compare(network.passwordApId, "");
        compare(network.connectingApId, "");
        network.changePassword(network.wifiNetworks[1], "hunter2-axon");
        compare(sent(), ['network.connect {"ap":"ssid:48656d6d61","password":"hunter2-axon"}']);
        Backend.calls = [];
        network.cancelPassword("ssid:48656d6d61");
        compare(sent(), []);
        frame(network.topic, "ok", networkData({}, pending));
        compare(network.passwordApId, "ssid:48656d6d61");
        compare(network.connectingApId, "ssid:48656d6d61");
    }

    function test_connectingApId() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        compare(network.connectingApId, "");
        frame(network.topic, "ok", networkData({}, {
            connecting: {
                ap_id: "ssid:4772616e6e652d47756573",
                ssid: "Granne-Gues",
                since: 1792922455000
            }
        }));
        compare(network.connectingApId, "ssid:4772616e6e652d47756573");
        frame(network.topic, "ok", networkData());
        compare(network.connectingApId, "");
    }

    function test_changePasswordSubmitsWhenPrompted() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        network.changePassword(network.wifiNetworks[1], "hunter2-axon");
        compare(sent(), ['network.submit_secret {"request":"r7","password":"hunter2-axon"}']);
    }

    function test_changePasswordConnectsOtherwise() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        network.changePassword(network.wifiNetworks[2], "hunter2-axon");
        compare(sent(), ['network.connect {"ap":"ssid:4772616e6e652d47756573","password":"hunter2-axon"}']);
    }

    function test_cancelPasswordCancelsTheRequest() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        network.cancelPassword("ssid:4772616e6e652d47756573");
        compare(sent(), []);
        network.cancelPassword("ssid:48656d6d61");
        compare(sent(), ['network.cancel_secret {"request":"r7"}']);
    }

    function test_activeRowClickSendsNothing() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        network.connectToWifiNetwork(network.wifiNetworks[0]);
        compare(sent(), []);
        network.connectToWifiNetwork(network.wifiNetworks[1]);
        compare(sent(), ['network.connect {"ap":"ssid:48656d6d61"}']);
    }

    function test_toggleWifiNoOpWithoutHardware() {
        const network = createTemporaryObject(networkComponent, tc);
        network.toggleWifi();
        compare(sent(), []);
        frame(network.topic, "ok", networkData({}, {
            present: false,
            enabled: false,
            hardware_enabled: false,
            device: null,
            ssid: null,
            ap_id: null,
            strength: null
        }, []));
        compare(network.wifiPresent, false);
        network.toggleWifi();
        compare(sent(), []);
        frame(network.topic, "ok", networkData());
        network.toggleWifi();
        compare(sent(), ['network.set_wifi {"on":false}']);
    }

    function test_portalFromConnectivity() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        compare(network.connectivity, "full");
        compare(network.portalDetected, false);
        frame(network.topic, "ok", networkData({
            connectivity: "portal",
            portal_url: "http://portal.example/login"
        }));
        compare(network.connectivity, "portal");
        compare(network.portalDetected, true);
        compare(network.portalUrl, "http://portal.example/login");
    }

    function test_serviceFunctionsCallNetwork() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        const rows = network.wifiNetworks;
        network.enableWifi();
        network.enableWifi(false);
        network.toggleWifi();
        network.rescanWifi();
        network.connectToWifiNetwork(rows[2]);
        network.changePassword(rows[1], "hunter2-axon");
        network.changePassword(rows[2], "hunter2-axon");
        network.cancelPassword(rows[1].id);
        compare(sent(), ['network.set_wifi {"on":true}', 'network.set_wifi {"on":false}', 'network.set_wifi {"on":false}', 'network.rescan {}', 'network.connect {"ap":"ssid:4772616e6e652d47756573"}', 'network.submit_secret {"request":"r7","password":"hunter2-axon"}', 'network.connect {"ap":"ssid:4772616e6e652d47756573","password":"hunter2-axon"}', 'network.cancel_secret {"request":"r7"}']);
    }

    function test_nothingBeforeFirstSnapshot() {
        const network = createTemporaryObject(networkComponent, tc);
        compare(network.topic.status, "waiting");
        compare(network.wifiNetworks, []);
        compare(network.networkName, "");
        compare(network.wifiEnabled, false);
        compare(network.wifiScanning, false);
        compare(network.wifiPresent, false);
        compare(network.ethernet, false);
        compare(network.connectingApId, "");
        compare(network.passwordApId, "");
        compare(network.lastError, null);
        compare(network.connectivity, "unknown");
        compare(network.portalDetected, false);
        compare(network.nmRunning, true);
        compare(network.materialSymbol, "signal_wifi_off");
        compare(sent(), []);
    }

    function test_dataNullKeepsRows() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        const row = network.wifiNetworks[0];
        frame(network.topic, "starting", null);
        compare(network.wifiNetworks.length, 3);
        verify(network.wifiNetworks[0] === row);
        compare(network.networkName, "Hemma_5G");
    }

    function test_tooltipWhenNmNotRunning() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData());
        compare(network.nmRunning, true);
        compare(network.statusLine, "Network: Hemma_5G");
        frame(network.topic, "stale", networkData({
            connectivity: "unknown"
        }), "live", {
            code: "source_down",
            message: "NetworkManager is not on the system bus"
        });
        compare(network.nmRunning, false);
        compare(network.statusLine, "NetworkManager not running");
        compare(network.networkName, "Hemma_5G");
        frame(network.topic, "ok", networkData());
        compare(network.nmRunning, true);
        compare(network.statusLine, "Network: Hemma_5G");
    }

    function test_lastErrorFromWifi() {
        const network = createTemporaryObject(networkComponent, tc);
        const error = {
            ap_id: "ssid:48656d6d61",
            ssid: "Hemma",
            code: "wrong_password",
            message: "the password was refused",
            at: 1792922470000
        };
        frame(network.topic, "ok", networkData({}, {
            last_error: error
        }));
        compare(network.lastError, error);
        frame(network.topic, "ok", networkData());
        compare(network.lastError, null);
    }

    // The sidebar's alt action: with Wi-Fi, radio on, rescan and the dialog; without, the toggle's
    // own Details launcher and no call (section 31 Offline and failure).
    function test_sidebarAltWithoutWifiRunsToggleDetails() {
        const network = createTemporaryObject(networkComponent, tc);
        const toggle = createTemporaryObject(toggleComponent, tc);
        let dialogs = 0;
        const showDialog = () => {
            dialogs += 1;
        };
        frame(network.topic, "ok", networkData({
            ethernet: {
                present: true,
                connected: true,
                name: "Wired"
            }
        }, {
            present: false,
            enabled: false,
            hardware_enabled: false,
            device: null,
            ssid: null,
            ap_id: null,
            strength: null
        }, []));
        network.sidebarAltAction(toggle, showDialog);
        compare(toggle.details, 1);
        compare(dialogs, 0);
        compare(sent(), []);
        frame(network.topic, "ok", networkData());
        network.sidebarAltAction(toggle, showDialog);
        compare(toggle.details, 1);
        compare(dialogs, 1);
        compare(sent(), ['network.set_wifi {"on":true}', 'network.rescan {}']);
    }

    function test_loadingSendsNothing() {
        const network = createTemporaryObject(networkComponent, tc);
        frame(network.topic, "ok", networkData({}, {
            secret_request: request("ssid:48656d6d61", "Hemma")
        }));
        compare(sent(), []);
        compare(Backend.attachments, ["+network"]);
    }
}
