pragma Singleton
pragma ComponentBehavior: Bound

import QtQuick
import Quickshell

/**
 * Distro and user facts, a thin client of axon's `system` topic (section 30,
 * "system"). axon reads /etc/os-release and the passwd entry once; this
 * singleton only turns those fields into the names the widgets bind.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "system"
        onSnapshot: data => root._apply(data)
    }

    readonly property string distroName: _distroName
    readonly property string distroIcon: _distroIcon
    readonly property string logo: _logo
    readonly property string homeUrl: _homeUrl
    readonly property string documentationUrl: _documentationUrl
    readonly property string supportUrl: _supportUrl
    readonly property string bugReportUrl: _bugReportUrl
    readonly property string privacyPolicyUrl: _privacyPolicyUrl
    readonly property string username: _username

    property string _distroName: "Unknown"
    property string _distroIcon: "linux-symbolic"
    property string _logo: ""
    property string _homeUrl: ""
    property string _documentationUrl: ""
    property string _supportUrl: ""
    property string _bugReportUrl: ""
    property string _privacyPolicyUrl: ""
    property string _username: ""

    function _iconForDistro(os) {
        const names = [os.id, os.name, os.pretty_name];
        if (names.some(name => (name ?? "").toLowerCase().includes("nyarch")))
            return "nyarch-symbolic";
        switch (os.id) {
        case "arch":
            return "arch-symbolic";
        case "endeavouros":
            return "endeavouros-symbolic";
        case "cachyos":
            return "cachyos-symbolic";
        case "nixos":
            return "nixos-symbolic";
        case "fedora":
            return "fedora-symbolic";
        case "linuxmint":
        case "ubuntu":
        case "zorin":
        case "popos":
            return "ubuntu-symbolic";
        case "debian":
        case "raspbian":
        case "kali":
            return "debian-symbolic";
        default:
            return "linux-symbolic";
        }
    }

    function _nameOfDistro(os) {
        if (os.pretty_name)
            return os.pretty_name;
        if (os.name)
            return os.name.replace(/Linux/i, "").trim();
        return "Unknown";
    }

    function _apply(data) {
        const os = data.os ?? {};
        _distroName = _nameOfDistro(os);
        _distroIcon = _iconForDistro(os);
        _logo = (os.logo ?? "").trim() || _distroIcon;
        _homeUrl = os.home_url ?? "";
        _documentationUrl = os.documentation_url ?? "";
        _supportUrl = os.support_url ?? "";
        _bugReportUrl = os.bug_report_url ?? "";
        _privacyPolicyUrl = os.privacy_policy_url ?? "";
        _username = data.user ?? "";
    }
}
