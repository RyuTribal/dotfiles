import QtQuick
import QtTest
import "../../services/brightness.js" as BrightnessJs

// services/brightness.js: the pure helpers of the Brightness facade (section 30 brightness). Run it
// through backend_tests.sh.
TestCase {
    id: tc
    name: "Brightness"

    function test_percentFor() {
        compare(BrightnessJs.percentFor(-0.2), 0);
        compare(BrightnessJs.percentFor(0.5), 50);
        compare(BrightnessJs.percentFor(1.05), 100);
    }

    function test_rowState() {
        // Section 30's example row.
        const panel = BrightnessJs.rowState({
            id: "eDP-1",
            rev: 31,
            kind: "backlight",
            device: "intel_backlight",
            percent: 95.0,
            raw: 36848,
            raw_max: 38787,
            ready: true,
            osd_at: 1792922458000,
            error: null
        });
        compare(panel.available, true);
        compare(panel.brightness, 0.95);
        compare(panel.ready, true);
        compare(panel.isDdc, false);
        compare(panel.osdAt, 1792922458000);

        // A DDC display not read yet.
        const ddc = BrightnessJs.rowState({
            id: "DP-1",
            rev: 2,
            kind: "ddc",
            device: "/dev/i2c-7",
            percent: null,
            raw: null,
            raw_max: null,
            ready: false,
            osd_at: null,
            error: null
        });
        compare(ddc.available, true);
        compare(ddc.brightness, 0);
        compare(ddc.ready, false);
        compare(ddc.isDdc, true);
        compare(ddc.osdAt, 0);
    }
}
