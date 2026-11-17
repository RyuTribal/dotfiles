import QtQuick
import QtTest
import ShellUnderTest

// The real services/Hyprsunset.qml over the stand-in Backend of fakes/Backend.qml: the facade's
// state from the `nightlight` topic and the calls its functions send (section 30 nightlight, D14).
// backend_tests.sh copies it without its `pragma Singleton` line into ShellUnderTest as
// HyprsunsetService, beside backend.js, so each test makes its own instance. Run it through
// backend_tests.sh.
TestCase {
    id: tc
    name: "HyprsunsetService"

    Component {
        id: hyprsunsetComponent
        HyprsunsetService {}
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

    // The stationary PC's frame: no hyprsunset installed (D10).
    function notInstalledFrame(topic) {
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
                message: "hyprsunset is not installed"
            },
            data: null
        });
    }

    // Section 30's `nightlight` example.
    function nightlightData(overrides) {
        return Object.assign({
            active: false,
            automatic: true,
            temperature: 5000,
            schedule: {
                from: "19:00",
                to: "06:30"
            },
            manual: null,
            next_change_at: 1792951200000,
            hyprsunset: {
                unit: "axon-nightlight-hyprsunset.service",
                adopted: false
            }
        }, overrides ?? {});
    }

    function test_appliesSnapshot() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData());
        compare(hyprsunset.active, false);
        compare(hyprsunset.automatic, true);
        compare(hyprsunset.available, true);
        frame(hyprsunset.topic, "ok", nightlightData({
            active: true,
            automatic: false
        }));
        compare(hyprsunset.active, true);
        compare(hyprsunset.automatic, false);
    }

    function test_waitingIsNotAvailable() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        compare(hyprsunset.topic.status, "waiting");
        compare(hyprsunset.available, false);
        compare(hyprsunset.active, false);
        compare(hyprsunset.automatic, false);
    }

    function test_unconfiguredIsNotAvailable() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        notInstalledFrame(hyprsunset.topic);
        compare(hyprsunset.available, false);
    }

    function test_staleStaysAvailable() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData({
            active: true
        }));
        frame(hyprsunset.topic, "stale", nightlightData({
            active: false
        }));
        compare(hyprsunset.available, true);
        compare(hyprsunset.active, false);
    }

    function test_toggleCalls() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData());
        hyprsunset.toggle();
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "nightlight.toggle");
        compare(Backend.calls[0].args, {});
    }

    function test_setAutomaticCalls() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData());
        hyprsunset.setAutomatic(false);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "nightlight.set_automatic");
        compare(Backend.calls[0].args, {
            on: false
        });
    }

    function test_keepsLastValueWhileAxonDown() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData({
            active: true
        }));
        Backend.up = false;
        compare(hyprsunset.available, true);
        compare(hyprsunset.active, true);
        compare(hyprsunset.automatic, true);
    }

    function test_dataNullKeepsValues() {
        const hyprsunset = createTemporaryObject(hyprsunsetComponent, tc);
        frame(hyprsunset.topic, "ok", nightlightData({
            active: true,
            automatic: false
        }));
        frame(hyprsunset.topic, "starting", null);
        compare(hyprsunset.active, true);
        compare(hyprsunset.automatic, false);
    }
}
