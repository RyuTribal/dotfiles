import QtQuick
import QtTest
import Quickshell
import qs
import qs.services
import BluetoothViewsUnderTest

// The real BluetoothDialog.qml, BluetoothDeviceItem.qml and BluetoothToggle.qml over the real
// services/BluetoothStatus.qml, fed `bluetooth` frames through the stand-in Backend (section 31
// Bluetooth, D13). backend_tests.sh gives it its own import root, as tst_network_views.qml has, because
// its BluetoothStatus, Backend and widgets are the singletons and types the views import by name.
// Quickshell.execDetached records the command instead of starting it. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "BluetoothViews"
    // A TestCase is invisible by default, and every item under it would read visible: false.
    visible: true
    when: windowShown
    width: 400
    height: 600

    Component {
        id: dialogComponent
        BluetoothDialog {
            width: 400
            height: 600
        }
    }

    Component {
        id: itemComponent
        BluetoothDeviceItem {
            width: 400
        }
    }

    Component {
        id: toggleComponent
        BluetoothToggle {}
    }

    property double rev: 0

    function init() {
        Backend.calls = [];
        Backend.up = true;
        Backend.instance = "one";
        Quickshell.launched = [];
        GlobalStates.sidebarRightOpen = true;
    }

    // BluetoothStatus is one singleton for the whole file, so revs only grow.
    function frame(data, status, error) {
        tc.rev += 1;
        BluetoothStatus.topic._apply({
            t: "snapshot",
            topic: "bluetooth",
            rev: tc.rev,
            status: status ?? "ok",
            source: "live",
            observed_at: 1792922458200,
            error: error ?? null,
            ops: [],
            data: data
        });
    }

    function headphones(overrides) {
        return Object.assign({
            id: "AC:80:0A:12:34:56",
            rev: 12,
            name: "WH-1000XM4",
            icon: "audio-headset",
            paired: true,
            trusted: true,
            connected: true,
            busy: null,
            battery_percent: 80
        }, overrides ?? {});
    }

    function stranger(overrides) {
        return Object.assign({
            id: "F4:73:35:AB:CD:EF",
            rev: 2,
            name: "F4-73-35-AB-CD-EF",
            icon: "",
            paired: false,
            trusted: false,
            connected: false,
            busy: null,
            battery_percent: null
        }, overrides ?? {});
    }

    function bluetoothData(overrides, adapter, rows) {
        const devices = rows ?? [headphones(), stranger()];
        const connectedRows = devices.filter(row => row.connected);
        return Object.assign({
            adapter: Object.assign({
                id: "hci0",
                name: "erichthonius",
                address: "04:F0:EE:12:34:56",
                powered: true,
                blocked: false,
                discovering: false,
                pairable: true
            }, adapter ?? {}),
            connected_count: connectedRows.length,
            first_connected_name: connectedRows.length > 0 ? connectedRows[0].name : null,
            devices: devices,
            pairing: null,
            last_error: null
        }, overrides ?? {});
    }

    function pairing(kind, deviceId, overrides) {
        return Object.assign({
            id: "p3",
            device_id: deviceId,
            device_name: "WH-1000XM4",
            kind: kind,
            passkey: kind === "confirm" || kind === "display_passkey" ? "482913" : null,
            expires_at: 1792922520000
        }, overrides ?? {});
    }

    // Three more rows for the list's moves: a paired pair of buds, a phone and a speaker named after it.
    function buds(overrides) {
        return Object.assign({
            id: "11:22:33:44:55:66",
            rev: 30,
            name: "Buds",
            icon: "audio-headphones",
            paired: true,
            trusted: true,
            connected: false,
            busy: null,
            battery_percent: 80
        }, overrides ?? {});
    }

    function phone(overrides) {
        return Object.assign({
            id: "02:C1:2F:00:00:02",
            rev: 40,
            name: "Phone",
            icon: "phone",
            paired: false,
            trusted: false,
            connected: false,
            busy: null,
            battery_percent: null
        }, overrides ?? {});
    }

    function speaker(overrides) {
        return Object.assign({
            id: "AA:BB:CC:00:00:03",
            rev: 50,
            name: "Speaker",
            icon: "audio-card",
            paired: false,
            trusted: false,
            connected: false,
            busy: null,
            battery_percent: null
        }, overrides ?? {});
    }

    // The dialog's delegate at `index`, once the list made it. ListView applies a model's moves to its
    // items at its next layout, so it lays out first.
    function delegateAt(list, index) {
        list.forceLayout();
        tryVerify(() => list.count > index && list.itemAtIndex(index) !== null, 1000, "a delegate at " + index);
        return list.itemAtIndex(index);
    }

    function sent() {
        return Backend.calls.map(call => call.method + " " + JSON.stringify(call.args));
    }

    // The row of BluetoothStatus.devices with this id.
    function row(id) {
        return BluetoothStatus.devices.find(device => device.id === id);
    }

    function test_noMethodCallOnLoad() {
        frame(bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56")
        }));
        const dialog = createTemporaryObject(dialogComponent, tc);
        createTemporaryObject(toggleComponent, tc);
        createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        wait(0);
        // The dialog's delegate, a BluetoothDeviceItem with its required row, was made for the row.
        const list = findChild(dialog, "deviceList");
        tryVerify(() => list.itemAtIndex(0) !== null, 1000, "a delegate for the row");
        compare(list.itemAtIndex(0).device.id, "AC:80:0A:12:34:56");
        compare(Backend.calls.length, 0);
        compare(Quickshell.launched, []);
    }

    // axon sorts the rows (D6); the dialog lists them in the topic's order, even one the old view sort
    // would have reversed (a MAC-like unpaired name before a connected device).
    function test_dialogListsTopicOrder() {
        frame(bluetoothData({}, {}, [stranger({
                rev: 5
            }), headphones({
                rev: 15
            })]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        tryVerify(() => list.count === 2 && list.itemAtIndex(1) !== null, 1000, "two delegates");
        compare(list.itemAtIndex(0).device.id, "F4:73:35:AB:CD:EF");
        compare(list.itemAtIndex(1).device.id, "AC:80:0A:12:34:56");
    }

    // The rehearsal's defect (fix round 2): a pair and connect both change the phone's row and move it to
    // the top. ScriptModel moves the object it held and announces no change, so the delegate must still
    // show the new row: connected, paired, not busy, Disconnect enabled.
    function test_rowThatChangesAndMovesShowsItsNewData() {
        frame(bluetoothData({}, {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            })]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        compare(delegateAt(list, 1).device.busy, "pairing");
        frame(bluetoothData({}, {}, [phone({
                rev: 42,
                paired: true,
                trusted: true,
                connected: true
            }), buds()]));
        const moved = delegateAt(list, 0);
        compare(moved.device.id, "02:C1:2F:00:00:02");
        compare(moved.device.connected, true);
        compare(moved.device.paired, true);
        compare(moved.device.busy, null);
        const button = findChild(moved, "connectButton");
        compare(button.buttonText, "Disconnect");
        compare(button.enabled, true);
        compare(delegateAt(list, 1).device.id, "11:22:33:44:55:66");
    }

    // A PIN being typed stays, with its focus and its delegate, while another row changes in place and
    // while a third row changes and moves above it; the moved row shows its new data (fix round 2).
    function test_pinFieldSurvivesOtherRowUpdates() {
        const asking = {
            pairing: pairing("request_pin", "02:C1:2F:00:00:02", {
                id: "p12",
                device_name: "Phone"
            })
        };
        frame(bluetoothData(asking, {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            }), speaker()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        const prompting = delegateAt(list, 1);
        compare(prompting.device.id, "02:C1:2F:00:00:02");
        const field = findChild(prompting, "pairingField");
        compare(field.visible, true);
        field.forceActiveFocus();
        field.text = "12";
        verify(field.activeFocus, "the PIN field has focus");
        frame(bluetoothData(asking, {}, [buds({
                rev: 31,
                battery_percent: 79
            }), phone({
                rev: 41,
                busy: "pairing"
            }), speaker()]));
        verify(delegateAt(list, 1) === prompting, "the phone keeps its delegate after an in-place change");
        compare(delegateAt(list, 0).device.battery, 0.79);
        compare(field.text, "12");
        verify(field.activeFocus, "the PIN field keeps focus after an in-place change");
        frame(bluetoothData(asking, {}, [speaker({
                rev: 51,
                paired: true,
                trusted: true,
                connected: true
            }), buds({
                rev: 31,
                battery_percent: 79
            }), phone({
                rev: 41,
                busy: "pairing"
            })]));
        compare(delegateAt(list, 0).device.id, "AA:BB:CC:00:00:03");
        compare(delegateAt(list, 0).device.connected, true);
        verify(delegateAt(list, 2) === prompting, "the phone keeps its delegate when another row moves");
        compare(field.text, "12");
        verify(field.activeFocus, "the PIN field keeps focus when another row moves");
        compare(Backend.calls.length, 0);
    }

    // The phone's PIN request, for the fix round 3 tests.
    function askingPhone() {
        return {
            pairing: pairing("request_pin", "02:C1:2F:00:00:02", {
                id: "p12",
                device_name: "Phone"
            })
        };
    }

    // Each delegate of `list` shows the row at its index, by id.
    function compareDelegateIds(list, ids) {
        for (let i = 0; i < ids.length; i++)
            compare(delegateAt(list, i).device.id, ids[i], "delegate " + i);
    }

    // The review's case (fix round 3): in one update the row above changes in place while the rows
    // below reorder. Each delegate keeps its own row, and Return answers the phone's request with the
    // typed PIN.
    function test_swapWithInPlaceAbove() {
        frame(bluetoothData(askingPhone(), {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            }), speaker()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        const above = delegateAt(list, 0);
        const prompting = delegateAt(list, 1);
        const below = delegateAt(list, 2);
        const field = findChild(prompting, "pairingField");
        field.forceActiveFocus();
        field.text = "12";
        frame(bluetoothData(askingPhone(), {}, [buds({
                rev: 31,
                battery_percent: 79
            }), speaker({
                rev: 51,
                paired: true,
                trusted: true
            }), phone({
                rev: 41,
                busy: "pairing"
            })]));
        compareDelegateIds(list, ["11:22:33:44:55:66", "AA:BB:CC:00:00:03", "02:C1:2F:00:00:02"]);
        verify(delegateAt(list, 0) === above, "the buds keep their delegate");
        verify(delegateAt(list, 1) === below, "the speaker keeps its delegate");
        verify(delegateAt(list, 2) === prompting, "the phone keeps its delegate");
        compare(above.device.battery, 0.79);
        compare(below.device.paired, true);
        compare(field.text, "12");
        verify(field.activeFocus, "the PIN field keeps focus");
        keyClick("3");
        keyClick(Qt.Key_Return);
        compare(sent(), ['bluetooth.pair_reply {"request":"p12","accept":true,"pin":"123"}']);
    }

    // In one update the top row changes in place, a row is added, one is removed and the speaker rises
    // above the phone: every delegate still shows its own row and the typed PIN stays with the phone.
    function test_addAndRemoveWithReorder() {
        frame(bluetoothData(askingPhone(), {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            }), speaker(), stranger()]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        const above = delegateAt(list, 0);
        const prompting = delegateAt(list, 1);
        const rising = delegateAt(list, 2);
        const field = findChild(prompting, "pairingField");
        field.forceActiveFocus();
        field.text = "12";
        frame(bluetoothData(askingPhone(), {}, [buds({
                rev: 31,
                battery_percent: 79
            }), headphones(), speaker({
                rev: 51,
                paired: true,
                trusted: true
            }), phone({
                rev: 41,
                busy: "pairing"
            })]));
        compareDelegateIds(list, ["11:22:33:44:55:66", "AC:80:0A:12:34:56", "AA:BB:CC:00:00:03", "02:C1:2F:00:00:02"]);
        compare(list.count, 4);
        verify(delegateAt(list, 0) === above, "the buds keep their delegate");
        verify(delegateAt(list, 2) === rising, "the speaker keeps its delegate");
        verify(delegateAt(list, 3) === prompting, "the phone keeps its delegate");
        compare(above.device.battery, 0.79);
        compare(rising.device.paired, true);
        compare(field.text, "12");
        verify(field.activeFocus, "the PIN field keeps focus");
    }

    // The prompting row's own change keeps the typed PIN (fix round 4): the phone's rev and name change
    // and BlueZ's request is published again under a new id; Return answers the new request.
    function test_ownRowChangeKeepsTypedValue() {
        frame(bluetoothData(askingPhone(), {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            })]));
        const dialog = createTemporaryObject(dialogComponent, tc);
        const list = findChild(dialog, "deviceList");
        const prompting = delegateAt(list, 1);
        const field = findChild(prompting, "pairingField");
        field.forceActiveFocus();
        field.text = "12";
        frame(bluetoothData({
            pairing: pairing("request_pin", "02:C1:2F:00:00:02", {
                id: "p13",
                device_name: "Phone 2"
            })
        }, {}, [buds(), phone({
                rev: 42,
                busy: "pairing",
                name: "Phone 2"
            })]));
        verify(delegateAt(list, 1) === prompting, "the phone keeps its delegate");
        compare(prompting.device.name, "Phone 2");
        compare(field.text, "12");
        verify(field.activeFocus, "the PIN field keeps focus");
        keyClick(Qt.Key_Return);
        compare(sent(), ['bluetooth.pair_reply {"request":"p13","accept":true,"pin":"12"}']);
    }

    // A field whose prompt is not open cannot act (fix round 3): Return, Accept and Reject send nothing
    // and each warns once; a prompt that closes clears its field and gives up focus.
    function test_hiddenFieldCannotSubmit() {
        frame(bluetoothData(askingPhone(), {}, [buds(), phone({
                rev: 41,
                busy: "pairing"
            })]));
        const item = createTemporaryObject(itemComponent, tc, {
            device: row("02:C1:2F:00:00:02")
        });
        const field = findChild(item, "pairingField");
        compare(findChild(item, "pairingPrompt").visible, true);
        field.forceActiveFocus();
        field.text = "12";
        frame(bluetoothData({}, {}, [buds(), phone()]));
        item.device = row("02:C1:2F:00:00:02");
        compare(findChild(item, "pairingPrompt").visible, false);
        compare(field.text, "", "the field is cleared when its prompt closes");
        compare(field.activeFocus, false);
        field.text = "12";
        for (let i = 0; i < 3; i++)
            ignoreWarning(/BluetoothDeviceItem: .*02:C1:2F:00:00:02.* prompt is not open/);
        field.accepted();
        findChild(item, "acceptButton").clicked();
        findChild(item, "rejectButton").clicked();
        compare(Backend.calls.length, 0);
    }

    function test_discoveringShowsProgress() {
        frame(bluetoothData());
        const dialog = createTemporaryObject(dialogComponent, tc);
        const progress = findChild(dialog, "discoveryProgress");
        const separator = findChild(dialog, "discoverySeparator");
        compare(progress.visible, false);
        compare(separator.visible, true);
        frame(bluetoothData({}, {
            discovering: true
        }));
        compare(progress.visible, true);
        compare(separator.visible, false);
        frame(bluetoothData());
        compare(progress.visible, false);
    }

    // 31:537: the prompt row shows in the item of the device the request names, and in no other.
    function test_promptShownForItsDevice() {
        frame(bluetoothData());
        const mine = createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        const other = createTemporaryObject(itemComponent, tc, {
            device: row("F4:73:35:AB:CD:EF")
        });
        compare(findChild(mine, "pairingPrompt").visible, false);
        frame(bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56")
        }));
        compare(findChild(mine, "pairingPrompt").visible, true);
        verify(findChild(mine, "pairingText").text.indexOf("482913") >= 0, findChild(mine, "pairingText").text);
        compare(findChild(mine, "pairingField").visible, false);
        compare(findChild(other, "pairingPrompt").visible, false);
        frame(bluetoothData());
        compare(findChild(mine, "pairingPrompt").visible, false);
    }

    function test_promptAcceptAndReject() {
        frame(bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56")
        }));
        const item = createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        findChild(item, "acceptButton").clicked();
        findChild(item, "rejectButton").clicked();
        compare(sent(), ['bluetooth.pair_reply {"request":"p3","accept":true}', 'bluetooth.pair_reply {"request":"p3","accept":false}']);
    }

    function test_pinFieldForRequestPin() {
        frame(bluetoothData({
            pairing: pairing("request_pin", "AC:80:0A:12:34:56", {
                id: "p7"
            })
        }));
        const item = createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        const field = findChild(item, "pairingField");
        compare(field.visible, true);
        field.text = "0000";
        findChild(item, "acceptButton").clicked();
        compare(sent(), ['bluetooth.pair_reply {"request":"p7","accept":true,"pin":"0000"}']);
        compare(field.text, "", "the PIN does not stay in the field");
        frame(bluetoothData({
            pairing: pairing("request_passkey", "AC:80:0A:12:34:56", {
                id: "p8"
            })
        }));
        compare(field.visible, true);
        frame(bluetoothData({
            pairing: pairing("authorize", "AC:80:0A:12:34:56", {
                id: "p9"
            })
        }));
        compare(field.visible, false);
    }

    // Accept answers BlueZ with the field's text, so it is disabled, and Enter does nothing, while that
    // text is not a passkey of 1 to 6 digits (fix round 1, F1). Reject stays enabled.
    function test_acceptDisabledOnInvalidValue() {
        frame(bluetoothData({
            pairing: pairing("request_passkey", "AC:80:0A:12:34:56", {
                id: "p10"
            })
        }));
        const item = createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        const field = findChild(item, "pairingField");
        const accept = findChild(item, "acceptButton");
        field.text = "";
        compare(accept.enabled, false, "empty");
        compare(findChild(item, "rejectButton").enabled, true);
        field.accepted();
        field.text = "12a";
        compare(accept.enabled, false, "not digits");
        field.accepted();
        compare(Backend.calls.length, 0);
        field.text = "482913";
        compare(accept.enabled, true, "six digits");
        field.accepted();
        compare(sent(), ['bluetooth.pair_reply {"request":"p10","accept":true,"passkey":482913}']);
        frame(bluetoothData({
            pairing: pairing("confirm", "AC:80:0A:12:34:56", {
                id: "p11"
            })
        }));
        compare(field.visible, false);
        compare(accept.enabled, true, "a confirm takes no value");
    }

    function test_lastErrorLineShown() {
        frame(bluetoothData());
        const dialog = createTemporaryObject(dialogComponent, tc);
        const line = findChild(dialog, "lastErrorLine");
        verify(line !== null);
        verify(dialog.children[1] === line, "the line sits right under the title");
        compare(line.visible, false);
        compare(line.wrapMode, Text.NoWrap);
        compare(line.elide, Text.ElideRight);
        const cases = [["rejected", "WH-1000XM4: Pairing was rejected"], ["auth_failed", "WH-1000XM4: Authentication failed"], ["timeout", "WH-1000XM4: Timed out"], ["failed", "WH-1000XM4: Could not connect"], ["something_new", "WH-1000XM4: org.bluez.Error.Failed: a BlueZ message"]];
        for (const [code, text] of cases) {
            frame(bluetoothData({
                last_error: {
                    device_id: "AC:80:0A:12:34:56",
                    code: code,
                    message: "org.bluez.Error.Failed: a BlueZ message",
                    at: 1792922470000
                }
            }));
            compare(line.visible, true, code);
            compare(line.text, text, code);
        }
        // A device without a row is named by its address.
        frame(bluetoothData({
            last_error: {
                device_id: "00:11:22:33:44:55",
                code: "timeout",
                message: "",
                at: 1792922470000
            }
        }));
        compare(line.text, "00:11:22:33:44:55: Timed out");
        frame(bluetoothData());
        compare(line.visible, false);
    }

    // BluetoothDeviceItem.qml's Connect/Disconnect and Forget call the facade with the row's id, and both
    // are disabled while axon works on the device.
    function test_buttonsCallServiceWithId() {
        frame(bluetoothData());
        const other = createTemporaryObject(itemComponent, tc, {
            device: row("F4:73:35:AB:CD:EF")
        });
        const mine = createTemporaryObject(itemComponent, tc, {
            device: row("AC:80:0A:12:34:56")
        });
        compare(findChild(other, "forgetButton").visible, false);
        findChild(other, "connectButton").clicked();
        findChild(mine, "connectButton").clicked();
        findChild(mine, "forgetButton").clicked();
        compare(sent(), ['bluetooth.connect {"device":"F4:73:35:AB:CD:EF"}', 'bluetooth.disconnect {"device":"AC:80:0A:12:34:56"}', 'bluetooth.forget {"device":"AC:80:0A:12:34:56"}']);
        compare(findChild(mine, "connectButton").enabled, true);
        compare(findChild(mine, "forgetButton").enabled, true);
        frame(bluetoothData({}, {}, [headphones({
                rev: 16,
                busy: "disconnecting"
            }), stranger()]));
        mine.device = row("AC:80:0A:12:34:56");
        compare(findChild(mine, "connectButton").enabled, false);
        compare(findChild(mine, "forgetButton").enabled, false);
    }

    function test_detailsLaunchesAppsBluetooth() {
        frame(bluetoothData());
        const dialog = createTemporaryObject(dialogComponent, tc);
        findChild(dialog, "detailsButton").clicked();
        compare(Quickshell.launched, [["bash", "-c", "bluetooth-details"]]);
        compare(GlobalStates.sidebarRightOpen, false);
        compare(Backend.calls.length, 0);
    }

    // 31:501 and D13: while bluetoothd is gone the toggle shows bluetooth_disabled with the tooltip
    // "Bluetooth service not running", and its click sends nothing.
    function test_toggleTooltipServiceNotRunning() {
        frame(bluetoothData());
        const toggle = createTemporaryObject(toggleComponent, tc);
        const tip = findChild(toggle, "toggleToolTip");
        compare(tip.content, "WH-1000XM4 | Right-click to configure");
        compare(toggle.buttonIcon, "bluetooth_connected");
        frame(bluetoothData(), "stale", {
            code: "source_down",
            message: "org.bluez is not on the system bus",
            retry_at: null
        });
        compare(tip.content, "Bluetooth service not running");
        compare(toggle.buttonIcon, "bluetooth_disabled");
        compare(toggle.toggled, false);
        toggle.clicked();
        compare(Backend.calls.length, 0);
        frame(bluetoothData());
        compare(tip.content, "WH-1000XM4 | Right-click to configure");
        toggle.clicked();
        compare(sent(), ['bluetooth.set_powered {"on":false}']);
    }
}
