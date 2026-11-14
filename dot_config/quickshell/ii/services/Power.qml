pragma Singleton
pragma ComponentBehavior: Bound

import "power.js" as P
import QtQuick
import Quickshell

/**
 * The power mode and the session actions, a thin client of axon's `power.mode` and `power.session`
 * topics (section 30, "power"). axon applies the mode and puts it back when the firmware moves it;
 * the session actions close the windows themselves before they power off, reboot or log out.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "power.mode"
        onSnapshot: data => root._applyMode(data)
    }
    readonly property BackendTopic sessionTopic: BackendTopic {
        name: "power.session"
        onSnapshot: data => root._applySession(data)
    }

    readonly property string mode: _mode // lap, balanced or max
    readonly property int watts: _watts // the PL1 the machine runs at now
    readonly property bool modeAvailable: P.isAvailable(topic.status)
    // logind's answers: "yes", "no", "challenge", "na" or "inhibited"; empty before the first snapshot.
    readonly property string canSuspend: _canSuspend
    readonly property string canHibernate: _canHibernate
    readonly property string canRebootFirmware: _canRebootFirmware

    property string _mode: "balanced"
    property int _watts: 0
    property string _canSuspend: ""
    property string _canHibernate: ""
    property string _canRebootFirmware: ""

    function setMode(mode) {
        Backend.call("power.set_mode", {
            mode: mode
        });
    }

    function lock() {
        Backend.call("power.lock", {});
    }

    function suspend() {
        Backend.call("power.suspend", {});
    }

    function hibernate() {
        Backend.call("power.hibernate", {});
    }

    function poweroff() {
        Backend.call("power.poweroff", {});
    }

    function reboot() {
        Backend.call("power.reboot", {});
    }

    function rebootToFirmware() {
        Backend.call("power.reboot_firmware", {});
    }

    function logout() {
        Backend.call("power.logout", {});
    }

    // Asks axon whether a package manager or a download is running. `done` gets
    // { packageManagers, downloads }, both false when the call fails (Backend logs the failure).
    function sessionBusy(done) {
        Backend.call("power.session_busy", {}, reply => {
            done(reply.ok ? P.busyFlags(reply.result) : {
                packageManagers: false,
                downloads: false
            });
        });
    }

    function _applyMode(data) {
        _mode = data.mode;
        _watts = data.actual.pl1_w;
    }

    function _applySession(data) {
        _canSuspend = data.can_suspend;
        _canHibernate = data.can_hibernate;
        _canRebootFirmware = data.can_reboot_firmware;
    }
}
