import QtQuick
import QtTest
import ShellUnderTest

// The real services/BluetoothStatus.qml over the stand-in Backend of fakes/Backend.qml: the rows it maps
// from the `bluetooth` topic, the leaves the bar, the vertical bar and the toggle read, the pairing
// request and the calls its functions send (section 31 Bluetooth, D13, C24). backend_tests.sh copies it
// without its `pragma Singleton` line into ShellUnderTest as BluetoothStatusService, beside backend.js,
// so each test makes its own instance. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "BluetoothStatusService"

    Component {
        id: bluetoothComponent
        BluetoothStatusService {}
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

    function frame(topic, status, data, error) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1792922458200,
            error: error ?? null,
            ops: [],
            data: data
        });
    }

    // The frames the facade's edges come from (section 20's status table, D12).
    function stale(topic, data) {
        frame(topic, "stale", data, {
            code: "source_down",
            message: "org.bluez is not on the system bus",
            retry_at: null
        });
    }

    function notPresent(topic) {
        frame(topic, "unconfigured", null, {
            code: "not_installed",
            message: "class/bluetooth/hci* is missing",
            details: {
                reason: "not_present"
            }
        });
    }

    function disabled(topic, data) {
        frame(topic, "unconfigured", data, {
            code: "disabled",
            message: "the bluetooth engine is disabled in axon.toml"
        });
    }

    // Section 31's example rows.
    function headphones(overrides) {
        return Object.assign({
            id: "AC:80:0A:12:34:56",
            rev: 12,
            name: "WH-1000XM4",
            icon: "audio-headset",
            paired: true,
            trusted: true,
            connected: true,
            busy: null,
            battery_percent: 80
        }, overrides ?? {});
    }

    function stranger(overrides) {
        return Object.assign({
            id: "F4:73:35:AB:CD:EF",
            rev: 2,
            name: "F4-73-35-AB-CD-EF",
            icon: "",
            paired: false,
            trusted: false,
            connected: false,
            busy: null,
            battery_percent: null
        }, overrides ?? {});
    }

    // Section 31's example data; `adapter` overrides go into the adapter object, the rest into the top.
    function bluetoothData(overrides, adapter, rows) {
        const devices = rows ?? [headphones(), stranger()];
        const connectedRows = devices.filter(row => row.connected);
        return Object.assign({
            adapter: Object.assign({
                id: "hci0",
                name: "erichthonius",
                address: "04:F0:EE:12:34:56",
                powered: true,
                blocked: false,
                discovering: false,
                pairable: true
            }, adapter ?? {}),
            connected_count: connectedRows.length,
            first_connected_name: connectedRows.length > 0 ? connectedRows[0].name : null,
            devices: devices,
            pairing: null,
            last_error: null
        }, overrides ?? {});
    }

    function pairing(kind, deviceId, overrides) {
        return Object.assign({
            id: "p3",
            device_id: deviceId,
            device_name: "WH-1000XM4",
            kind: kind,
            passkey: kind === "confirm" ? "482913" : null,
            expires_at: 1792922520000
        }, overrides ?? {});
    }

    // The bar's, the vertical bar's and the toggle's icon binding (BarContent.qml, VerticalBarContent.qml,
    // BluetoothToggle.qml), unchanged by this cutover.
    function icon(bluetooth) {
        return bluetooth.connected ? "bluetooth_connected" : bluetooth.enabled ? "bluetooth" : "bluetooth_disabled";
    }

    function sent() {
        return Backend.calls.map(call => call.method + " " + JSON.stringify(call.args));
    }

    function test_rowsMapFieldNames() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.devices.length, 2);
        const row = bluetooth.devices[0];
        compare(Object.keys(row).sort(), ["battery", "batteryAvailable", "busy", "connected", "icon", "id", "name", "paired", "rev"]);
        compare(row.id, "AC:80:0A:12:34:56");
        compare(row.rev, 12);
        compare(row.name, "WH-1000XM4");
        compare(row.icon, "audio-headset");
        compare(row.connected, true);
        compare(row.paired, true);
        compare(row.busy, null);
        compare(row.batteryAvailable, true);
        compare(row.battery, 0.8);
        const other = bluetooth.devices[1];
        compare(other.name, "F4-73-35-AB-CD-EF");
        compare(other.icon, "");
        compare(other.connected, false);
        compare(other.paired, false);
        compare(other.batteryAvailable, false);
        frame(bluetooth.topic, "ok", bluetoothData({}, {}, [stranger({
                rev: 3,
                busy: "pairing"
            })]));
        compare(bluetooth.devices[0].busy, "pairing");
    }

    function test_reuseRowsKeepsObjects() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        const first = bluetooth.devices[0];
        const second = bluetooth.devices[1];
        frame(bluetooth.topic, "ok", bluetoothData({}, {}, [headphones({
                rev: 13,
                battery_percent: 70
            }), stranger()]));
        verify(bluetooth.devices[0] !== first);
        verify(bluetooth.devices[1] === second);
        compare(bluetooth.devices[0].battery, 0.7);
    }

    // A delegate compares its row's id with pairingRequest.device_id, and firstActiveDevice is a row:
    // neither may name a device whose row is not in devices at any change notification.
    function test_noIdWithoutRow() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        const missing = [];
        const check = () => {
            const ids = [bluetooth.pairingRequest?.device_id, bluetooth.firstActiveDevice?.id];
            for (const id of ids) {
                if (id !== undefined && !bluetooth.devices.some(row => row.id === id))
                    missing.push(id);
            }
        };
        bluetooth.devicesChanged.connect(check);
        bluetooth.pairingRequestChanged.connect(check);
        bluetooth.firstActiveDeviceChanged.connect(check);
        const asking = {
            pairing: pairing("confirm", "F4:73:35:AB:CD:EF")
        };
        frame(bluetooth.topic, "ok", bluetoothData(asking));
        compare(bluetooth.pairingRequest.device_id, "F4:73:35:AB:CD:EF");
        compare(bluetooth.firstActiveDevice.id, "AC:80:0A:12:34:56");
        frame(bluetooth.topic, "ok", bluetoothData(asking, {}, [headphones()]));
        compare(bluetooth.pairingRequest, null);
        frame(bluetooth.topic, "ok", bluetoothData(asking));
        compare(bluetooth.pairingRequest.device_id, "F4:73:35:AB:CD:EF");
        frame(bluetooth.topic, "ok", bluetoothData(asking, {}, []));
        bluetooth.devicesChanged.disconnect(check);
        bluetooth.pairingRequestChanged.disconnect(check);
        bluetooth.firstActiveDeviceChanged.disconnect(check);
        compare(missing, []);
        compare(bluetooth.devices, []);
        compare(bluetooth.pairingRequest, null);
        compare(bluetooth.firstActiveDevice, null);
    }

    // The parity table's leaves (section 31): enabled, firstActiveDevice, activeDeviceCount, connected.
    function test_parityLeaves() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.enabled, true);
        compare(bluetooth.connected, true);
        compare(bluetooth.activeDeviceCount, 1);
        compare(bluetooth.firstActiveDevice.name, "WH-1000XM4");
        verify(bluetooth.firstActiveDevice === bluetooth.devices[0]);
        compare(icon(bluetooth), "bluetooth_connected");
        frame(bluetooth.topic, "ok", bluetoothData({}, {}, [headphones({
                rev: 14,
                connected: false
            }), stranger()]));
        compare(bluetooth.enabled, true);
        compare(bluetooth.connected, false);
        compare(bluetooth.activeDeviceCount, 0);
        compare(bluetooth.firstActiveDevice, null);
        compare(icon(bluetooth), "bluetooth");
        frame(bluetooth.topic, "ok", bluetoothData({}, {
            powered: false
        }, [headphones({
                rev: 14,
                connected: false
            }), stranger()]));
        compare(bluetooth.enabled, false);
        compare(icon(bluetooth), "bluetooth_disabled");
    }

    // W10: the bar's tooltip read a deviceName that did not exist and showed "BT: undefined".
    function test_deviceNameTooltip() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.deviceName, "WH-1000XM4");
        compare(bluetooth.statusLine, "BT: WH-1000XM4");
        frame(bluetooth.topic, "ok", bluetoothData({}, {}, [stranger()]));
        compare(bluetooth.deviceName, null);
        compare(bluetooth.statusLine, "Bluetooth: On");
        frame(bluetooth.topic, "ok", bluetoothData({}, {
            powered: false
        }, [stranger()]));
        compare(bluetooth.statusLine, "Bluetooth: Off");
    }

    // C24 (Ivan, review fix 18): present is false on any unconfigured topic, so neither a machine without
    // an adapter nor a disabled engine shows a Bluetooth widget.
    function test_presentFalseOnUnconfigured() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        compare(bluetooth.present, true, "before the first snapshot");
        notPresent(bluetooth.topic);
        compare(bluetooth.topic.errorReason, "not_present");
        compare(bluetooth.present, false, "not_present");
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.present, true, "ok");
        disabled(bluetooth.topic, bluetoothData());
        compare(bluetooth.topic.errorReason, "");
        compare(bluetooth.present, false, "disabled");
        stale(bluetooth.topic, bluetoothData());
        compare(bluetooth.present, true, "stale");
        frame(bluetooth.topic, "starting", null);
        compare(bluetooth.present, true, "starting");
        frame(bluetooth.topic, "failed", null, {
            code: "engine_failed",
            message: "the bluetooth engine failed"
        });
        compare(bluetooth.present, true, "failed");
    }

    function test_serviceNotRunningTooltip() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.serviceRunning, true);
        compare(bluetooth.statusLine, "BT: WH-1000XM4");
        stale(bluetooth.topic, bluetoothData());
        compare(bluetooth.serviceRunning, false);
        compare(bluetooth.statusLine, "Bluetooth service not running");
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.serviceRunning, true);
        compare(bluetooth.statusLine, "BT: WH-1000XM4");
    }

    // 31:501 and D13: while bluetoothd is gone the data keeps its last values, adapter.powered true and a
    // connected row among them, and the icons still show bluetooth_disabled; so they do while the engine
    // is disabled. The rows stay listed.
    function test_staleShowsBluetoothDisabled() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        stale(bluetooth.topic, bluetoothData());
        compare(bluetooth.enabled, false);
        compare(bluetooth.connected, false);
        compare(bluetooth.activeDeviceCount, 0);
        compare(bluetooth.firstActiveDevice, null);
        compare(bluetooth.deviceName, null);
        compare(bluetooth.statusLine, "Bluetooth service not running");
        compare(icon(bluetooth), "bluetooth_disabled");
        compare(bluetooth.devices.length, 2);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(icon(bluetooth), "bluetooth_connected");
        disabled(bluetooth.topic, bluetoothData());
        compare(bluetooth.enabled, false);
        compare(bluetooth.connected, false);
        compare(bluetooth.activeDeviceCount, 0);
        compare(bluetooth.firstActiveDevice, null);
        compare(bluetooth.deviceName, null);
        compare(icon(bluetooth), "bluetooth_disabled");
        compare(bluetooth.devices.length, 2);
    }

    function test_staleToggleSendsNothing() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        stale(bluetooth.topic, bluetoothData());
        bluetooth.toggleEnabled();
        bluetooth.setEnabled(true);
        compare(sent(), []);
        disabled(bluetooth.topic, bluetoothData());
        bluetooth.toggleEnabled();
        bluetooth.setEnabled(true);
        compare(sent(), []);
        notPresent(bluetooth.topic);
        bluetooth.toggleEnabled();
        bluetooth.setEnabled(true);
        compare(sent(), []);
    }

    // rfkill: the adapter cannot be powered, and axon would answer conflict (D11).
    function test_blockedToggleSendsNothing() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData({}, {
            powered: false,
            blocked: true
        }));
        compare(bluetooth.blocked, true);
        bluetooth.toggleEnabled();
        compare(sent(), []);
        frame(bluetooth.topic, "ok", bluetoothData({}, {
            powered: false
        }));
        bluetooth.toggleEnabled();
        compare(sent(), ['bluetooth.set_powered {"on":true}']);
    }

    function test_toggleCallsSetPowered() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        bluetooth.toggleEnabled();
        frame(bluetooth.topic, "ok", bluetoothData({}, {
            powered: false
        }));
        bluetooth.toggleEnabled();
        compare(sent(), ['bluetooth.set_powered {"on":false}', 'bluetooth.set_powered {"on":true}']);
    }

    function test_serviceFunctionsCallBluetooth() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        bluetooth.setEnabled(true);
        bluetooth.setEnabled(false);
        bluetooth.connectDevice("F4:73:35:AB:CD:EF");
        bluetooth.disconnectDevice("AC:80:0A:12:34:56");
        bluetooth.forgetDevice("AC:80:0A:12:34:56");
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("request_pin", "AC:80:0A:12:34:56")
        }));
        bluetooth.replyPairing("p3", true, "0000");
        bluetooth.replyPairing("p3", false, "0000");
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("request_passkey", "AC:80:0A:12:34:56", {
                id: "p4"
            })
        }));
        bluetooth.replyPairing("p4", true, "012345");
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56", {
                id: "p5"
            })
        }));
        bluetooth.replyPairing("p5", true, "482913");
        bluetooth.replyPairing("p5", false);
        compare(sent(), ['bluetooth.set_powered {"on":true}', 'bluetooth.set_powered {"on":false}', 'bluetooth.connect {"device":"F4:73:35:AB:CD:EF"}', 'bluetooth.disconnect {"device":"AC:80:0A:12:34:56"}', 'bluetooth.forget {"device":"AC:80:0A:12:34:56"}', 'bluetooth.pair_reply {"request":"p3","accept":true,"pin":"0000"}', 'bluetooth.pair_reply {"request":"p3","accept":false}', 'bluetooth.pair_reply {"request":"p4","accept":true,"passkey":12345}', 'bluetooth.pair_reply {"request":"p5","accept":true}', 'bluetooth.pair_reply {"request":"p5","accept":false}']);
    }

    // An accept that would answer BlueZ with an empty or malformed passkey or PIN sends nothing and warns
    // once, naming the request and its kind (fix round 1, F1). A reject needs no value.
    function test_acceptRefusesInvalidValue() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("request_passkey", "AC:80:0A:12:34:56", {
                id: "p4"
            })
        }));
        for (const value of ["", "12a", "1234567", " 12", undefined]) {
            ignoreWarning(/pair_reply p4 \(request_passkey\) not sent/);
            bluetooth.replyPairing("p4", true, value);
        }
        compare(sent(), []);
        bluetooth.replyPairing("p4", false, "");
        bluetooth.replyPairing("p4", true, "012345");
        compare(sent(), ['bluetooth.pair_reply {"request":"p4","accept":false}', 'bluetooth.pair_reply {"request":"p4","accept":true,"passkey":12345}']);
        Backend.calls = [];
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("request_pin", "AC:80:0A:12:34:56", {
                id: "p6"
            })
        }));
        // PIN length counts bytes (C23): 16 ASCII or 8 two-byte characters pass, 17 or 9 do not.
        for (const value of ["", "12345678901234567", "ääääääääd"]) {
            ignoreWarning(/pair_reply p6 \(request_pin\) not sent/);
            bluetooth.replyPairing("p6", true, value);
        }
        compare(sent(), []);
        bluetooth.replyPairing("p6", true, "1234567890123456");
        bluetooth.replyPairing("p6", true, "ääääääää");
        compare(sent(), ['bluetooth.pair_reply {"request":"p6","accept":true,"pin":"1234567890123456"}', 'bluetooth.pair_reply {"request":"p6","accept":true,"pin":"ääääääää"}']);
        compare(bluetooth.pairingValueValid("request_pin", "ääääääää"), true);
        compare(bluetooth.pairingValueValid("request_pin", "äääääääää"), false);
        compare(bluetooth.pairingValueValid("confirm", ""), true);
    }

    // D13: while the engine does not run (disabled) or bluetoothd is gone, the rows stay with every busy
    // null and no pairing prompt shows; both come back with the service (fix round 1, F2).
    function test_noBusyOrPromptWhileNotRunning() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        const working = bluetoothData({
            pairing: pairing("confirm", "F4:73:35:AB:CD:EF")
        }, {}, [headphones(), stranger({
                rev: 4,
                busy: "pairing"
            })]);
        frame(bluetooth.topic, "ok", working);
        compare(bluetooth.devices[1].busy, "pairing");
        compare(bluetooth.pairingRequest.device_id, "F4:73:35:AB:CD:EF");
        disabled(bluetooth.topic, working);
        compare(bluetooth.devices.length, 2);
        compare(bluetooth.devices.map(row => row.busy), [null, null]);
        compare(bluetooth.devices[1].name, "F4-73-35-AB-CD-EF");
        compare(bluetooth.pairingRequest, null);
        stale(bluetooth.topic, working);
        compare(bluetooth.devices.map(row => row.busy), [null, null]);
        compare(bluetooth.pairingRequest, null);
        frame(bluetooth.topic, "ok", working);
        compare(bluetooth.devices[1].busy, "pairing");
        compare(bluetooth.pairingRequest.device_id, "F4:73:35:AB:CD:EF");
    }

    function test_pairingRequestFromTopic() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.pairingRequest, null);
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56")
        }));
        compare(bluetooth.pairingRequest, pairing("confirm", "AC:80:0A:12:34:56"));
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.pairingRequest, null);
    }

    function test_lastErrorFromTopic() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        const error = {
            device_id: "F4:73:35:AB:CD:EF",
            code: "rejected",
            message: "org.bluez.Error.AuthenticationRejected: Authentication Rejected",
            at: 1792922470000
        };
        frame(bluetooth.topic, "ok", bluetoothData({
            last_error: error
        }));
        compare(bluetooth.lastError, error);
        frame(bluetooth.topic, "ok", bluetoothData());
        compare(bluetooth.lastError, null);
    }

    function test_nothingBeforeFirstSnapshot() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        compare(bluetooth.topic.status, "waiting");
        compare(bluetooth.present, true);
        compare(bluetooth.serviceRunning, true);
        compare(bluetooth.devices, []);
        compare(bluetooth.enabled, false);
        compare(bluetooth.connected, false);
        compare(bluetooth.activeDeviceCount, 0);
        compare(bluetooth.firstActiveDevice, null);
        compare(bluetooth.deviceName, null);
        compare(bluetooth.blocked, false);
        compare(bluetooth.discovering, false);
        compare(bluetooth.pairingRequest, null);
        compare(bluetooth.lastError, null);
        compare(bluetooth.statusLine, "Bluetooth: Off");
        compare(icon(bluetooth), "bluetooth_disabled");
        compare(sent(), []);
    }

    function test_dataNullKeepsRows() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData());
        const row = bluetooth.devices[0];
        frame(bluetooth.topic, "starting", null);
        compare(bluetooth.devices.length, 2);
        verify(bluetooth.devices[0] === row);
        compare(bluetooth.deviceName, "WH-1000XM4");
        compare(bluetooth.enabled, true);
    }

    // The facade holds `bluetooth` only: `bluetooth.discovery`, which runs discovery, belongs to the
    // sidebar while the dialog is open.
    function test_loadingSendsNothing() {
        const bluetooth = createTemporaryObject(bluetoothComponent, tc);
        frame(bluetooth.topic, "ok", bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56")
        }));
        compare(sent(), []);
        compare(Backend.attachments, ["+bluetooth"]);
    }
}
