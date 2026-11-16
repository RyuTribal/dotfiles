import QtQuick
import QtTest
import ShellUnderTest

// The real services/Brightness.qml over the stand-in Backend of fakes/Backend.qml and the stand-in Hypr
// of fakes/brightness/Hypr.qml: the monitor objects built from the `brightness` topic, the OSD deadline
// and the calls they send (section 30 brightness, D18, D21). backend_tests.sh copies it without its
// `pragma Singleton` line into ShellUnderTest as BrightnessService, beside BrightnessOutput.qml,
// OsdDeadline.qml, brightness.js and osd.js, so each test makes its own instance.
// Config.options.osd.timeout is 1000 in fakes/qs/modules/common/Config.qml. Run it through
// backend_tests.sh.
TestCase {
    id: tc
    name: "BrightnessService"

    Component {
        id: brightnessComponent
        BrightnessService {}
    }

    property double rev: 0
    property double clock: 1792922458100

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 1792922460000;
        Backend.downSince = 0;
        Hypr.focusedMonitorName = "eDP-1";
        tc.rev = 0;
        tc.clock = 1792922458100;
    }

    // An instance whose wall clock reads tc.clock.
    function makeBrightness() {
        const brightness = createTemporaryObject(brightnessComponent, tc);
        brightness.wallClock = () => tc.clock;
        return brightness;
    }

    function frame(brightness, status, data, error) {
        tc.rev += 1;
        brightness.topic._apply({
            t: "snapshot",
            topic: "brightness",
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1792922460000,
            error: error ?? null,
            ops: [],
            data: data
        });
    }

    function lastCall() {
        return Backend.calls[Backend.calls.length - 1];
    }

    // Section 30's example row, with osd_at long past so no test sees the OSD it does not ask for.
    function panelRow(overrides) {
        return Object.assign({
            id: "eDP-1",
            rev: 31,
            kind: "backlight",
            device: "intel_backlight",
            percent: 95.0,
            raw: 36848,
            raw_max: 38787,
            ready: true,
            osd_at: 1792922000000,
            error: null
        }, overrides ?? {});
    }

    function ddcRow(overrides) {
        return Object.assign({
            id: "DP-1",
            rev: 3,
            kind: "ddc",
            device: "/dev/i2c-7",
            percent: 40.0,
            raw: 40,
            raw_max: 100,
            ready: true,
            osd_at: null,
            error: null
        }, overrides ?? {});
    }

    function test_monitorFromRow() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow({
                    osd_at: 1792922458000
                })]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        compare(monitor.name, "eDP-1");
        compare(monitor.brightness, 0.95);
        compare(monitor.available, true);
        compare(monitor.ready, true);
        compare(monitor.isDdc, false);
        compare(monitor.osdAt, 1792922458000);
    }

    function test_screenWithoutRowIsInert() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "HEADLESS-2"
        });
        compare(monitor.available, false);
        compare(monitor.ready, false);
        compare(monitor.brightness, 0);
        monitor.setBrightness(0.5);
        monitor.stepBrightness(5);
        compare(Backend.calls.length, 0);
    }

    function test_nullScreenIsInert() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        for (const screen of [null, undefined]) {
            const monitor = brightness.getMonitorForScreen(screen);
            verify(monitor !== null && monitor !== undefined, "an object for " + screen);
            compare(monitor.available, false);
            compare(monitor.ready, false);
            compare(monitor.brightness, 0);
            monitor.setBrightness(0.5);
            monitor.stepBrightness(5);
        }
        compare(Backend.calls.length, 0);
    }

    function test_monitorObjectSurvivesSnapshots() {
        const brightness = makeBrightness();
        const early = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        compare(early.available, false);
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        verify(monitor === early, "the object asked for before the first snapshot");
        compare(monitor.available, true);
        frame(brightness, "ok", {
            outputs: [panelRow({
                    rev: 32,
                    percent: 50.0,
                    raw: 19394
                })]
        });
        verify(brightness.getMonitorForScreen({
            name: "eDP-1"
        }) === monitor, "the same object after a second snapshot");
        compare(monitor.brightness, 0.5);
        // A snapshot without the row makes it inert, one with it again brings it back.
        frame(brightness, "ok", {
            outputs: [ddcRow()]
        });
        verify(brightness.getMonitorForScreen({
            name: "eDP-1"
        }) === monitor, "the same object while its row is gone");
        compare(monitor.available, false);
        compare(monitor.ready, false);
        frame(brightness, "ok", {
            outputs: [ddcRow(), panelRow({
                    rev: 33
                })]
        });
        verify(brightness.getMonitorForScreen({
            name: "eDP-1"
        }) === monitor, "the same object when its row is back");
        compare(monitor.available, true);
        compare(monitor.brightness, 0.95);
    }

    function test_notPresentMakesEveryMonitorInert() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow(), ddcRow()]
        });
        const panel = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        const ddc = brightness.getMonitorForScreen({
            name: "DP-1"
        });
        compare(panel.available, true);
        compare(panel.brightness, 0.95);
        compare(ddc.available, true);
        compare(ddc.isDdc, true);
        compare(ddc.brightness, 0.4);
        // Only the not_present reason makes them inert (D21): not the data-null frame of a restarted
        // axond, and not a failed engine.
        Backend.instance = "two";
        frame(brightness, "starting", null);
        compare(panel.available, true);
        compare(panel.brightness, 0.95);
        compare(ddc.available, true);
        frame(brightness, "failed", null, {
            code: "engine_failed",
            message: "panicked at actor.rs"
        });
        compare(panel.available, true);
        compare(panel.brightness, 0.95);
        compare(ddc.available, true);
        frame(brightness, "unconfigured", null, {
            code: "not_installed",
            message: "no backlight device and no DDC display",
            details: {
                reason: "not_present"
            }
        });
        compare(panel.available, false);
        compare(panel.ready, false);
        compare(ddc.available, false);
        compare(ddc.ready, false);
        panel.setBrightness(0.5);
        ddc.stepBrightness(5);
        compare(Backend.calls.length, 0);
        // A monitor first asked for while not present is inert too.
        compare(brightness.getMonitorForScreen({
            name: "HDMI-A-1"
        }).available, false);
    }

    function test_keepsLastValueWhileAxonDown() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        Backend.up = false;
        compare(monitor.brightness, 0.95);
        compare(monitor.available, true);
        compare(monitor.ready, true);
    }

    // A row with `percent: null` (a DDC display that failed its first read after a re-detect) keeps
    // the last brightness instead of showing 0.
    function test_rowWithoutPercentKeepsLastValue() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [ddcRow()]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "DP-1"
        });
        compare(monitor.brightness, 0.4);
        frame(brightness, "ok", {
            outputs: [ddcRow({
                    rev: 4,
                    percent: null,
                    raw: null,
                    raw_max: null,
                    ready: false
                })]
        });
        compare(monitor.ready, false);
        compare(monitor.available, true);
        compare(monitor.brightness, 0.4);
    }

    function test_setBrightnessSendsClampedPercent() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        const monitor = brightness.getMonitorForScreen({
            name: "eDP-1"
        });
        monitor.setBrightness(1.05);
        compare(lastCall().method, "brightness.set");
        compare(lastCall().args, {
            output: "eDP-1",
            percent: 100
        });
        monitor.setBrightness(0.42);
        compare(lastCall().method, "brightness.set");
        compare(lastCall().args, {
            output: "eDP-1",
            percent: 42
        });
        compare(Backend.calls.length, 2);
    }

    function test_increaseSendsStep() {
        const brightness = makeBrightness();
        brightness.increaseBrightness();
        compare(lastCall().method, "brightness.step");
        compare(lastCall().args, {
            delta: 5
        });
        verify(!("output" in lastCall().args), "no output: axon steps the focused monitor");
        brightness.decreaseBrightness();
        compare(lastCall().method, "brightness.step");
        compare(lastCall().args, {
            delta: -5
        });
        verify(!("output" in lastCall().args), "no output: axon steps the focused monitor");
        compare(Backend.calls.length, 2);
    }

    function test_scrollSendsStep() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        brightness.getMonitorForScreen({
            name: "eDP-1"
        }).stepBrightness(5);
        compare(lastCall().method, "brightness.step");
        compare(lastCall().args, {
            output: "eDP-1",
            delta: 5
        });
        brightness.getMonitorForScreen({
            name: "eDP-1"
        }).stepBrightness(-5);
        compare(lastCall().args, {
            output: "eDP-1",
            delta: -5
        });
        brightness.getMonitorForScreen(null).stepBrightness(5);
        compare(Backend.calls.length, 2);
    }

    function test_osdVisibleUntilTimeout() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow()]
        });
        compare(brightness.osdVisible, false);
        frame(brightness, "ok", {
            outputs: [panelRow({
                    rev: 32,
                    osd_at: tc.clock
                })]
        });
        compare(brightness.osdAt, tc.clock);
        compare(brightness.osdVisible, true);
        // The one-shot timer's handler, run at the deadline.
        tc.clock += 1000;
        brightness.osd._arm();
        compare(brightness.osdVisible, false);
    }

    function test_passedDeadlineHidesAtOnce() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow({
                    osd_at: tc.clock - 1000
                })]
        });
        compare(brightness.osdVisible, false);
        frame(brightness, "ok", {
            outputs: [panelRow({
                    rev: 32,
                    osd_at: tc.clock - 1001
                })]
        });
        compare(brightness.osdVisible, false);
    }

    function test_hideOsdHidesUntilNextChange() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow({
                    osd_at: tc.clock
                })]
        });
        compare(brightness.osdVisible, true);
        brightness.hideOsd();
        compare(brightness.osdVisible, false);
        // The one-shot timer's handler, still inside the old timeout.
        tc.clock += 100;
        brightness.osd._arm();
        compare(brightness.osdVisible, false);
        tc.clock += 100;
        frame(brightness, "ok", {
            outputs: [panelRow({
                    rev: 32,
                    osd_at: tc.clock
                })]
        });
        compare(brightness.osdVisible, true);
    }

    function test_focusChangeMovesTheOsdToThatOutput() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow(), ddcRow({
                    osd_at: tc.clock
                })]
        });
        compare(brightness.osdAt, 1792922000000);
        compare(brightness.osdVisible, false);
        Hypr.focusedMonitorName = "DP-1";
        compare(brightness.osdAt, tc.clock);
        compare(brightness.osdVisible, true);
        // A focused monitor without a row has no OSD.
        Hypr.focusedMonitorName = "HEADLESS-2";
        compare(brightness.osdAt, 0);
        compare(brightness.osdVisible, false);
    }

    function test_firstSnapshotWithOldOsdAtShowsNothing() {
        const brightness = makeBrightness();
        frame(brightness, "ok", {
            outputs: [panelRow({
                    osd_at: tc.clock - 1500
                })]
        });
        compare(brightness.osdAt, tc.clock - 1500);
        compare(brightness.osdVisible, false);
    }
}
