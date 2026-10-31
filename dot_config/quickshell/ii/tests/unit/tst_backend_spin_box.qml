import QtQuick
import QtTest
import qs.services
import qs.modules.settings

// The real modules/settings/BackendSpinBox.qml over the real services/AxonConfig.qml and the
// stand-in Backend of fakes/Backend.qml (section 50.1, backend controls): "--" until the first
// `config` snapshot, one `config.set` per entry, a written value shown until the topic shows it, and
// a refused one snapped back with the reply's error beside it. Run it through backend_tests.sh, which
// builds the qs.* modules it imports.
TestCase {
    id: tc
    name: "BackendSpinBox"
    when: windowShown
    // A TestCase is invisible by default, and under it every item's `visible` reads false, which
    // would make the error row's visibility checks pass whatever the row does.
    visible: true

    property double rev: 0

    BackendSpinBox {
        id: spin
        key: "resources.interval_ms"
        text: "Polling interval (ms)"
        from: 100
        to: 10000
        stepSize: 100
    }

    function field() {
        return findChild(spin, "valueField");
    }

    function errorRow() {
        return findChild(spin, "errorRow");
    }

    function lastCall() {
        return Backend.calls[Backend.calls.length - 1];
    }

    function config(intervalMs) {
        tc.rev += 1;
        AxonConfig.topic._apply({
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            data: {
                values: {
                    resources: {
                        interval_ms: intervalMs
                    }
                },
                issues: []
            }
        });
    }

    // Types `digits` over the field's text, as the user does, and leaves the entry open.
    function typeDigits(digits) {
        field().forceActiveFocus();
        field().selectAll();
        for (const digit of digits)
            keyClick(digit);
    }

    // Types `digits` and finishes the entry with Return.
    function typeEntry(digits) {
        typeDigits(digits);
        keyClick(Qt.Key_Return);
    }

    function refuse(message) {
        lastCall().done({
            ok: false,
            error: {
                code: "invalid_args",
                message: message
            }
        });
    }

    function accept() {
        lastCall().done({
            ok: true,
            result: null
        });
    }

    // Runs once, before the first test function. AxonConfig is a singleton and stays known once a
    // snapshot has arrived, so this is the only place where none has.
    function initTestCase() {
        compare(AxonConfig.known, false);
        compare(field().text, "--");
        compare(spin.enabled, false);
        compare(errorRow().text, "axon unreachable");
        compare(errorRow().visible, true);

        // `from` stood in for the value, so this snapshot leaves the value at 100 and the text must
        // change all the same.
        config(100);
        compare(spin.value, 100);
        compare(field().text, "100");
        compare(spin.enabled, true);
        compare(errorRow().visible, false);
    }

    function init() {
        config(3000);
        Backend.calls = [];
    }

    function test_aTypedEntryIsOneWrite() {
        compare(field().text, "3000");
        typeEntry("2500");
        compare(Backend.calls.length, 1);
        compare(lastCall().method, "config.set");
        compare(lastCall().args.key, "resources.interval_ms");
        compare(lastCall().args.value, 2500);
        accept();
        // The topic says 2500 once axon has published the new value; until then the write stays shown.
        compare(spin.value, 2500);
        config(2500);
        compare(spin.value, 2500);
        compare(field().text, "2500");
        compare(Backend.calls.length, 1); // a topic update writes nothing back
    }

    // Every value the control shows after `act` runs, as the field's text.
    function shownWhile(act) {
        const shown = [];
        const record = () => shown.push(field().text);
        field().textChanged.connect(record);
        act();
        field().textChanged.disconnect(record);
        return shown;
    }

    // axon replies before its next config snapshot, and a snapshot from before the write (or one of
    // another key) can come between. The old value never shows again.
    function test_anAcceptedWriteStaysShownUntilTheTopicShowsIt() {
        typeEntry("2500");
        compare(field().text, "2500");
        const shown = shownWhile(() => {
            accept();
            config(3000);
            config(2500);
        });
        compare(shown, []);
        config(2600); // it follows the topic again
        compare(field().text, "2600");
    }

    // A step leaves the value bound to the topic in Qt's SpinBox, so a snapshot before the reply would
    // move it back.
    function test_aStepStaysShownBeforeTheReply() {
        field().forceActiveFocus();
        const shown = shownWhile(() => {
            keyClick(Qt.Key_Up);
            config(3000);
            accept();
            config(3100);
        });
        compare(shown, ["3100"]);
        config(3200);
        compare(field().text, "3200");
    }

    // Steps quicker than the topic: a snapshot showing an earlier step is behind, not a change.
    function test_quickStepsShowTheLatest() {
        field().forceActiveFocus();
        const shown = shownWhile(() => {
            keyClick(Qt.Key_Up);
            accept();
            keyClick(Qt.Key_Up);
            accept();
            config(3100);
            config(3200);
        });
        compare(shown, ["3100", "3200"]);
        compare(Backend.calls.length, 2);
        config(3300);
        compare(field().text, "3300");
    }

    // Someone else set the key before axon published this control's write: axon's value shows.
    function test_anotherWriteToTheKeyShows() {
        typeEntry("2500");
        accept();
        config(4000);
        compare(field().text, "4000");
        config(4100);
        compare(field().text, "4100");
    }

    function test_aRefusedWriteSnapsBackAndShowsTheError() {
        typeEntry("2050");
        compare(lastCall().args.value, 2050);
        refuse("resources.interval_ms must be a multiple of 100");
        compare(spin.value, 3000);
        compare(field().text, "3000");
        compare(spin.errorText, "resources.interval_ms must be a multiple of 100");
        compare(errorRow().text, "resources.interval_ms must be a multiple of 100");
        compare(errorRow().visible, true);
        config(2500); // it still follows the topic after snapping back
        compare(spin.value, 2500);
        compare(field().text, "2500");
    }

    function test_anAcceptedWriteClearsTheError() {
        typeEntry("2050");
        refuse("resources.interval_ms must be a multiple of 100");
        compare(errorRow().visible, true);
        typeEntry("2000");
        accept();
        compare(spin.errorText, "");
        compare(errorRow().visible, false);
        config(2000);
    }

    // Backend answers a call it cannot send with its own error, and the control snaps back all the
    // same.
    function test_aWriteWhileDisconnectedSnapsBack() {
        typeEntry("4000");
        lastCall().done({
            ok: false,
            error: {
                code: "disconnected",
                message: "axon is not connected"
            }
        });
        compare(spin.value, 3000);
        compare(field().text, "3000");
        compare(errorRow().text, "axon is not connected");
    }

    // A step of the spin box itself (the arrow keys, like its buttons) writes the stepped value
    // once.
    function test_aStepIsOneWrite() {
        field().forceActiveFocus();
        keyClick(Qt.Key_Up);
        compare(Backend.calls.length, 1);
        compare(lastCall().args.value, 3100);
        accept();
        config(3100);
        compare(spin.value, 3100);
        compare(Backend.calls.length, 1);
    }

    // A step during a typed entry is that entry's one write. So after a refused step, leaving the
    // field writes nothing more, and the refusal's message stays.
    function test_leavingTheFieldAfterARefusedStepWritesNothingMore() {
        typeDigits("205");
        keyClick(Qt.Key_Up);
        compare(Backend.calls.length, 1);
        compare(lastCall().args.value, 305);
        refuse("resources.interval_ms must be a multiple of 100");
        compare(spin.value, 3000);
        tc.forceActiveFocus(); // focus moves elsewhere
        compare(Backend.calls.length, 1);
        compare(field().text, "3000");
        compare(errorRow().text, "resources.interval_ms must be a multiple of 100");
    }
}
