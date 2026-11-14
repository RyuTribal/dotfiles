import QtQuick
import QtTest
import ShellUnderTest

// The real services/Battery.qml, Power.qml and Idle.qml over the stand-in Backend of fakes/Backend.qml:
// the Parity names built from the power topics and the calls the functions send (section 30 power).
// backend_tests.sh copies each without its `pragma Singleton` line into ShellUnderTest as
// BatteryService, PowerService and IdleService, beside power.js, so each test makes its own instance.
// Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "PowerService"

    Component {
        id: batteryComponent
        BatteryService {}
    }

    Component {
        id: powerComponent
        PowerService {}
    }

    Component {
        id: idleComponent
        IdleService {}
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

    function frame(topic, status, data) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1792922382000,
            ops: [],
            data: data
        });
    }

    // The stationary PC's frame of section 30: no battery, no data.
    function unconfiguredFrame(topic) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: "unconfigured",
            source: "live",
            observed_at: null,
            error: {
                code: "not_installed",
                message: "no battery: UPower's display device is not a present battery",
                details: {
                    reason: "not_present"
                }
            },
            data: null
        });
    }

    // Section 30's `power.battery` example.
    function batteryData(overrides) {
        return Object.assign({
            state: "discharging",
            percent: 87.0,
            on_ac: false,
            energy_rate_w: 7.42,
            time_to_empty_s: 24120,
            time_to_full_s: 0,
            is_low: false,
            is_critical: false
        }, overrides ?? {});
    }

    // Section 30's `power.mode` example.
    function modeData(overrides) {
        return Object.assign({
            mode: "balanced",
            in_sync: true,
            applying: false,
            actual: {
                profile: "balanced",
                pl1_w: 22,
                pl2_w: 57,
                lapmode: false
            },
            last_drift: null,
            reapplies_this_boot: 41
        }, overrides ?? {});
    }

    function test_batteryFromSnapshot() {
        const battery = createTemporaryObject(batteryComponent, tc);
        frame(battery.topic, "ok", batteryData());
        compare(battery.available, true);
        compare(battery.percentage, 0.87);
        compare(battery.chargeState, 2);
        compare(battery.isCharging, false);
        compare(battery.isPluggedIn, false);
        compare(battery.isLow, false);
        compare(battery.energyRate, 7.42);
        compare(battery.timeToEmpty, 24120);
        compare(battery.timeToFull, 0);
        frame(battery.topic, "ok", batteryData({
            state: "charging",
            on_ac: true
        }));
        compare(battery.chargeState, 1);
        compare(battery.isCharging, true);
        compare(battery.isPluggedIn, true);
    }

    function test_batteryUnavailableWhileUnconfigured() {
        const battery = createTemporaryObject(batteryComponent, tc);
        compare(battery.available, false); // waiting
        unconfiguredFrame(battery.topic);
        compare(battery.available, false);
        frame(battery.topic, "stale", batteryData());
        compare(battery.available, true);
    }

    function test_batteryKeepsLastValueWhileAxonDown() {
        const battery = createTemporaryObject(batteryComponent, tc);
        frame(battery.topic, "ok", batteryData());
        Backend.up = false;
        compare(battery.available, true);
        compare(battery.percentage, 0.87);
        compare(battery.chargeState, 2);
        compare(battery.energyRate, 7.42);
        compare(battery.timeToEmpty, 24120);
    }

    function test_isLowFollowsTheTopic() {
        const battery = createTemporaryObject(batteryComponent, tc);
        frame(battery.topic, "ok", batteryData({
            percent: 50.0,
            is_low: true
        }));
        compare(battery.percentage, 0.5);
        compare(battery.isLow, true);
        frame(battery.topic, "ok", batteryData({
            percent: 10.0,
            is_low: false
        }));
        compare(battery.isLow, false);
    }

    function test_setModeCallsPowerSetMode() {
        const power = createTemporaryObject(powerComponent, tc);
        power.setMode("max");
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "power.set_mode");
        compare(Backend.calls[0].args, {
            mode: "max"
        });
    }

    function test_modeAndWatts() {
        const power = createTemporaryObject(powerComponent, tc);
        frame(power.topic, "ok", modeData());
        compare(power.mode, "balanced");
        compare(power.watts, 22);
        compare(power.modeAvailable, true);
        frame(power.topic, "error", modeData({
            mode: "lap",
            actual: {
                profile: "balanced",
                pl1_w: 15,
                pl2_w: 57,
                lapmode: true
            }
        }));
        compare(power.mode, "lap");
        compare(power.watts, 15);
        compare(power.modeAvailable, true);
    }

    function test_modeUnavailableWhileUnconfigured() {
        const power = createTemporaryObject(powerComponent, tc);
        compare(power.modeAvailable, false); // waiting
        unconfiguredFrame(power.topic);
        compare(power.modeAvailable, false);
    }

    function test_toggleInhibitSendsNegation() {
        const idle = createTemporaryObject(idleComponent, tc);
        frame(idle.topic, "ok", {
            inhibit: true,
            held: true,
            owner: "hypridle"
        });
        compare(idle.inhibit, true);
        idle.toggleInhibit();
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "power.set_idle_inhibit");
        compare(Backend.calls[0].args, {
            inhibit: false
        });
        frame(idle.topic, "stale", {
            inhibit: false,
            held: false,
            owner: null
        });
        compare(idle.inhibit, false);
        compare(idle.available, true);
        idle.toggleInhibit();
        compare(Backend.calls[1].args, {
            inhibit: true
        });
    }

    function test_idleUnavailableWhileUnconfigured() {
        const idle = createTemporaryObject(idleComponent, tc);
        compare(idle.available, false); // waiting
        unconfiguredFrame(idle.topic);
        compare(idle.available, false);
    }

    function test_powerSessionMethodsSendTheirMethod() {
        const power = createTemporaryObject(powerComponent, tc);
        const cases = [
            {
                run: () => power.lock(),
                method: "power.lock"
            },
            {
                run: () => power.suspend(),
                method: "power.suspend"
            },
            {
                run: () => power.hibernate(),
                method: "power.hibernate"
            },
            {
                run: () => power.poweroff(),
                method: "power.poweroff"
            },
            {
                run: () => power.reboot(),
                method: "power.reboot"
            },
            {
                run: () => power.rebootToFirmware(),
                method: "power.reboot_firmware"
            },
            {
                run: () => power.logout(),
                method: "power.logout"
            }
        ];
        for (const testCase of cases) {
            Backend.calls = [];
            testCase.run();
            compare(Backend.calls.length, 1, testCase.method);
            compare(Backend.calls[0].method, testCase.method);
            compare(Backend.calls[0].args, {}, testCase.method);
        }
    }

    function test_sessionBusyMapsReply() {
        const power = createTemporaryObject(powerComponent, tc);
        const answers = [];
        power.sessionBusy(flags => answers.push(flags));
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "power.session_busy");
        compare(Backend.calls[0].args, {});
        Backend.calls[0].done({
            ok: true,
            result: {
                package_managers: ["pacman"],
                downloads: true
            }
        });
        compare(answers, [
            {
                packageManagers: true,
                downloads: true
            }
        ]);
        // A failed call shows nothing busy; Backend logs the failure.
        power.sessionBusy(flags => answers.push(flags));
        Backend.calls[1].done({
            ok: false,
            error: {
                code: "disconnected",
                message: "axon is not connected"
            }
        });
        compare(answers[1], {
            packageManagers: false,
            downloads: false
        });
    }

    function test_canHibernateFromSessionTopic() {
        const power = createTemporaryObject(powerComponent, tc);
        compare(power.canHibernate, ""); // waiting
        frame(power.sessionTopic, "ok", {
            can_suspend: "yes",
            can_hibernate: "na",
            can_reboot_firmware: "yes"
        });
        compare(power.canSuspend, "yes");
        compare(power.canHibernate, "na");
        compare(power.canRebootFirmware, "yes");
        frame(power.sessionTopic, "ok", {
            can_suspend: "yes",
            can_hibernate: "challenge",
            can_reboot_firmware: "no"
        });
        compare(power.canHibernate, "challenge");
        compare(power.canRebootFirmware, "no");
    }
}
