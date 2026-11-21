pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import QtQuick
import Quickshell

/**
 * Bluetooth, a thin client of axon's `bluetooth` topic (section 31, "Bluetooth"). axon follows BlueZ for
 * the default adapter, sorts the devices and answers BlueZ's pairing requests through its own agent;
 * this maps the rows to the field names the widgets read and sends the controls. A row is a plain
 * object, identical while its `rev` holds (reuseRows). Discovery runs while a view holds the
 * `bluetooth.discovery` topic, which the sidebar does while its dialog is open.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "bluetooth"
        onSnapshot: data => root._apply(data)
    }

    // False while the topic is unconfigured: no adapter on this machine, or the engine disabled in
    // axon.toml. The widgets hide then.
    readonly property bool present: topic.status !== "unconfigured"
    // False while axon reports bluetoothd off the bus; the rows then hold their last values.
    readonly property bool serviceRunning: !(topic.status === "stale" && topic.errorCode === "source_down")
    readonly property bool enabled: _running && (_data?.adapter?.powered ?? false)
    // rfkill blocks the adapter: it cannot be powered.
    readonly property bool blocked: _data?.adapter?.blocked ?? false
    readonly property bool discovering: _data?.adapter?.discovering ?? false
    readonly property int activeDeviceCount: _running ? (_data?.connected_count ?? 0) : 0
    readonly property bool connected: activeDeviceCount > 0
    // The first connected row, in axon's order, or null.
    readonly property var firstActiveDevice: _running ? _firstActiveDevice : null
    readonly property var deviceName: _running ? (_data?.first_connected_name ?? null) : null
    // While the service does not run nothing is in progress: every row's busy reads null.
    readonly property var devices: _running ? _rows : _rows.map(row => row.busy === null ? row : Object.assign({}, row, {
            busy: null
        }))
    // `{id, device_id, device_name, kind, passkey, expires_at}` while BlueZ asks axon's agent about a
    // device that has a row and the service runs, else null.
    readonly property var pairingRequest: _running ? _pairingRequest : null
    // `{device_id, code, message, at}` of the last failed pair or connect, or null.
    readonly property var lastError: _data?.last_error ?? null
    // The bar's tooltip.
    readonly property string statusLine: !serviceRunning ? "Bluetooth service not running" : connected ? `BT: ${deviceName}` : enabled ? "Bluetooth: On" : "Bluetooth: Off"

    property var _data: null
    property var _rows: []
    property var _index: ({})
    property var _pairingRequest: null
    property var _firstActiveDevice: null
    // Power and the connected devices read as off while bluetoothd is gone or the engine does not run.
    readonly property bool _running: present && serviceRunning

    // Sends nothing while the adapter cannot be powered: bluetoothd gone, the engine not running, or
    // rfkill.
    function setEnabled(on) {
        if (_running && !blocked)
            Backend.call("bluetooth.set_powered", {
                on: on
            });
    }

    function toggleEnabled() {
        setEnabled(!enabled);
    }

    function connectDevice(id) {
        Backend.call("bluetooth.connect", {
            device: id
        });
    }

    function disconnectDevice(id) {
        Backend.call("bluetooth.disconnect", {
            device: id
        });
    }

    function forgetDevice(id) {
        Backend.call("bluetooth.forget", {
            device: id
        });
    }

    // Whether `value` can answer a request of `kind`: a passkey is 1 to 6 digits, a PIN 1 to 16 bytes
    // in UTF-8 (C23); every other kind takes no value.
    function pairingValueValid(kind, value) {
        if (kind === "request_passkey")
            return typeof value === "string" && /^[0-9]{1,6}$/.test(value);
        if (kind === "request_pin") {
            if (typeof value !== "string")
                return false;
            let bytes = 0;
            for (const character of value) {
                const point = character.codePointAt(0);
                bytes += point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
            }
            return bytes >= 1 && bytes <= 16;
        }
        return true;
    }

    // Answers the pairing request `request`. An accepted `request_pin` sends `value` as the PIN, an
    // accepted `request_passkey` sends it as the passkey; any other request takes no value. An accept
    // whose value is not valid sends nothing: the view never offers one, so it is a mistake.
    function replyPairing(request, accept, value) {
        const args = {
            request: request,
            accept: accept
        };
        const pairing = _data?.pairing;
        const kind = pairing?.id === request ? pairing.kind : "";
        if (accept && !pairingValueValid(kind, value)) {
            console.warn(`BluetoothStatus: pair_reply ${request} (${kind}) not sent: the value is not valid for its kind`);
            return;
        }
        if (accept && kind === "request_pin")
            args.pin = String(value);
        else if (accept && kind === "request_passkey")
            args.passkey = Number(value);
        Backend.call("bluetooth.pair_reply", args);
    }

    // A `bluetooth` row with the widgets' field names: `battery` in 0..1, as the item reads it.
    function _row(device) {
        const percent = device.battery_percent ?? null;
        return {
            id: device.id,
            rev: device.rev,
            icon: device.icon,
            name: device.name,
            connected: device.connected,
            paired: device.paired,
            busy: device.busy,
            batteryAvailable: percent !== null,
            battery: percent !== null ? percent / 100 : 0
        };
    }

    // No change notification may show pairingRequest or firstActiveDevice naming a device that is not
    // in devices: one that changes is cleared before the rows change and set after.
    function _apply(data) {
        const rows = B.reuseRows(_index, data.devices.map(_row));
        const pairing = data.pairing && rows.index[data.pairing.device_id] !== undefined ? data.pairing : null;
        const first = rows.rows.find(row => row.connected) ?? null;
        if (_pairingRequest?.device_id !== pairing?.device_id)
            _pairingRequest = null;
        if (_firstActiveDevice !== first)
            _firstActiveDevice = null;
        _index = rows.index;
        _rows = rows.rows;
        _data = data;
        _pairingRequest = pairing;
        _firstActiveDevice = first;
    }
}
