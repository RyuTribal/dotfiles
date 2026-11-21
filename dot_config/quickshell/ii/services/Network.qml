pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import QtQuick
import Quickshell

/**
 * Networking, a thin client of axon's `network` topic (section 31, "Network"). axon follows
 * NetworkManager, groups the access points by SSID, joins networks and answers NM's password requests
 * through its own secret agent; this maps the rows to the field names the widgets read and sends the
 * controls. A row is a plain object, identical while its `rev` holds (reuseRows).
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "network"
        onSnapshot: data => root._apply(data)
    }

    readonly property bool wifiPresent: _data?.wifi.present ?? false
    readonly property bool wifiEnabled: _data?.wifi.enabled ?? false
    readonly property bool wifiScanning: _data?.wifi.scanning ?? false
    // A wired connection is up.
    readonly property bool ethernet: _data?.ethernet.connected ?? false
    readonly property var wifiNetworks: _rows
    // The primary connection's name, the one the bar shows.
    readonly property string networkName: _data?.primary?.name ?? ""
    // "unknown", "none", "portal", "limited" or "full".
    readonly property string connectivity: _data?.connectivity ?? "unknown"
    readonly property bool portalDetected: connectivity === "portal"
    readonly property string portalUrl: _data?.portal_url ?? ""
    // `{ap_id, ssid, code, message, at}` of the last failed join, or null.
    readonly property var lastError: _data?.wifi.last_error ?? null
    // The id of the row axon is joining, and of the row whose password NM asks for; "" for none.
    readonly property string connectingApId: _connectingApId
    readonly property string passwordApId: _passwordApId
    // False while axon reports NetworkManager off the bus; the data then holds its last values.
    readonly property bool nmRunning: !(topic.status === "stale" && topic.errorCode === "source_down")
    readonly property string statusLine: nmRunning ? `Network: ${networkName}` : "NetworkManager not running"
    readonly property string materialSymbol: ethernet ? "lan" : wifiEnabled ? (root._strength > 80 ? "signal_wifi_4_bar" : root._strength > 60 ? "network_wifi_3_bar" : root._strength > 40 ? "network_wifi_2_bar" : root._strength > 20 ? "network_wifi_1_bar" : "signal_wifi_0_bar") : "signal_wifi_off"

    property var _data: null
    property var _rows: []
    property var _index: ({})
    property string _connectingApId: ""
    property string _passwordApId: ""
    property string _requestId: ""
    readonly property int _strength: _data?.wifi.strength ?? 0

    function enableWifi(enabled = true) {
        Backend.call("network.set_wifi", {
            on: enabled
        });
    }

    function toggleWifi() {
        if (wifiPresent)
            enableWifi(!wifiEnabled);
    }

    function rescanWifi() {
        Backend.call("network.rescan", {});
    }

    // `row` is a mapped row; the active one is already joined.
    function connectToWifiNetwork(row) {
        if (row.active)
            return;
        Backend.call("network.connect", {
            ap: row.id
        });
    }

    // Answers NM's pending request when it is for `row`, else joins `row` with the password.
    function changePassword(row, password) {
        if (_passwordApId !== "" && row.id === _passwordApId) {
            Backend.call("network.submit_secret", {
                request: _requestId,
                password: password
            });
            return;
        }
        Backend.call("network.connect", {
            ap: row.id,
            password: password
        });
    }

    function cancelPassword(apId) {
        if (_passwordApId !== "" && apId === _passwordApId)
            Backend.call("network.cancel_secret", {
                request: _requestId
            });
    }

    // The sidebar's alt action on the network toggle: with Wi-Fi, radio on, a scan and the dialog;
    // without, the toggle's own Details launcher.
    function sidebarAltAction(toggle, showDialog) {
        if (!wifiPresent) {
            toggle.openDetails();
            return;
        }
        enableWifi();
        rescanWifi();
        showDialog();
    }

    // A `network` row with the widgets' field names: `security` is "" for an open network, so
    // `isSecure` reads as it did.
    function _row(ap) {
        const security = ap.secure ? ap.security : "";
        return {
            id: ap.id,
            rev: ap.rev,
            ssid: ap.ssid,
            bssid: ap.bssid,
            strength: ap.strength,
            frequency: ap.frequency_mhz,
            active: ap.active,
            saved: ap.saved,
            security: security,
            isSecure: security.length > 0
        };
    }

    // No change notification may show connectingApId or passwordApId naming a row that is not in
    // wifiNetworks: an id that changes is cleared before the rows change and set after. A request or
    // an activation in a persisted snapshot belongs to a daemon that no longer runs (D11).
    function _apply(data) {
        const live = topic.source !== "persisted";
        const rows = B.reuseRows(_index, data.access_points.map(_row));
        const kept = id => rows.index[id] !== undefined ? id : "";
        const connecting = live ? kept(data.wifi.connecting?.ap_id ?? "") : "";
        const request = live ? data.wifi.secret_request : null;
        const asking = kept(request?.ap_id ?? "");
        if (_connectingApId !== connecting)
            _connectingApId = "";
        if (_passwordApId !== asking)
            _passwordApId = "";
        _requestId = request?.id ?? "";
        _index = rows.index;
        _rows = rows.rows;
        _data = data;
        _connectingApId = connecting;
        _passwordApId = asking;
    }
}
