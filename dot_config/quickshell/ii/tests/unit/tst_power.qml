import QtQuick
import QtTest
import "../../services/power.js" as PowerJs

// services/power.js: the pure helpers Battery, Power and Idle build their Parity names from (section 30
// power). Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "Power"

    function test_chargeStateNumbers() {
        const names = ["unknown", "charging", "discharging", "empty", "fully_charged", "pending_charge", "pending_discharge"];
        for (let number = 0; number < names.length; number += 1)
            compare(PowerJs.chargeStateNumber(names[number]), number, names[number]);
        compare(PowerJs.chargeStateNumber("exploding"), 0);
        compare(PowerJs.chargeStateNumber(""), 0);
        compare(PowerJs.chargeStateNumber(undefined), 0);
    }

    function test_isAvailable() {
        compare(PowerJs.isAvailable("waiting"), false);
        compare(PowerJs.isAvailable("unconfigured"), false);
        for (const status of ["ok", "stale", "error", "starting", "failed"])
            compare(PowerJs.isAvailable(status), true, status);
    }

    function test_isPluggedIn() {
        compare(PowerJs.isPluggedIn("charging"), true);
        compare(PowerJs.isPluggedIn("pending_charge"), true);
        for (const state of ["unknown", "discharging", "empty", "fully_charged", "pending_discharge"])
            compare(PowerJs.isPluggedIn(state), false, state);
    }

    function test_nextMode() {
        compare(PowerJs.nextMode("lap"), "balanced");
        compare(PowerJs.nextMode("balanced"), "max");
        compare(PowerJs.nextMode("max"), "lap");
    }

    function test_busyFlags() {
        compare(PowerJs.busyFlags({
            package_managers: ["pacman"],
            downloads: false
        }), {
            packageManagers: true,
            downloads: false
        });
        compare(PowerJs.busyFlags({
            package_managers: [],
            downloads: true
        }), {
            packageManagers: false,
            downloads: true
        });
    }
}
