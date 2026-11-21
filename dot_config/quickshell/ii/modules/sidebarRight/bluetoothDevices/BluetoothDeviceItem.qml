import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts

DialogListItem {
    id: root
    // A row of BluetoothStatus.devices.
    required property var device
    property bool expanded: false
    // BlueZ's pairing request for this device, or null.
    readonly property var pairingRequest: BluetoothStatus.pairingRequest?.device_id === device?.id ? BluetoothStatus.pairingRequest : null
    // The device shows a code and BlueZ waits for nothing from us.
    readonly property bool pairingDisplays: pairingRequest?.kind === "display_passkey" || pairingRequest?.kind === "display_pin"
    // The field's text can answer the request (always true for a request that takes no value).
    readonly property bool pairingValueValid: BluetoothStatus.pairingValueValid(pairingRequest?.kind ?? "", pairingField.text)
    readonly property string deviceId: device?.id ?? ""
    // BlueZ asks about this row's device. The field and its buttons act only while it is true, so a
    // typed PIN can never answer for another row.
    readonly property bool prompting: pairingRequest !== null
    pointingHandCursor: !expanded

    onClicked: expanded = !expanded
    altAction: () => expanded = !expanded
    onPromptingChanged: root.resetPrompt()
    onDeviceIdChanged: root.resetPrompt()

    // The prompt closed, opened or now belongs to another device: what was typed goes, with the focus.
    function resetPrompt() {
        pairingField.text = "";
        pairingField.focus = false;
    }

    // The prompt's line for each kind of request (section 31, "pairing").
    function pairingText(request) {
        switch (request?.kind) {
        case "confirm":
            return Translation.tr("Does the device show %1?").arg(request.passkey);
        case "authorize":
            return Translation.tr("Allow this device to pair?");
        case "display_passkey":
            return Translation.tr("Type %1 on the device").arg(request.passkey);
        case "display_pin":
            return Translation.tr("Type PIN %1 on the device").arg(request.passkey);
        case "request_pin":
            return Translation.tr("Enter the device's PIN");
        case "request_passkey":
            return Translation.tr("Enter the device's passkey");
        default:
            return "";
        }
    }

    // A reply from a prompt that is not open here is a mistake: it is refused loudly, never sent.
    function replyPairing(accept) {
        if (!root.prompting) {
            console.warn(`BluetoothDeviceItem: pairing reply for ${root.deviceId || "no device"} not sent: its prompt is not open`);
            return;
        }
        if (accept && !root.pairingValueValid)
            return;
        BluetoothStatus.replyPairing(root.pairingRequest.id, accept, pairingField.text);
        pairingField.text = "";
    }

    component ActionButton: DialogButton {
        colBackground: Appearance.colors.colPrimary
        colBackgroundHover: Appearance.colors.colPrimaryHover
        colRipple: Appearance.colors.colPrimaryActive
        colText: Appearance.colors.colOnPrimary
    }

    contentItem: ColumnLayout {
        anchors {
            fill: parent
            topMargin: root.verticalPadding
            leftMargin: root.horizontalPadding
            rightMargin: root.horizontalPadding
        }
        spacing: 0

        RowLayout {
            // Name
            spacing: 10

            MaterialSymbol {
                iconSize: Appearance.font.pixelSize.larger
                text: Icons.getBluetoothDeviceMaterialSymbol(root.device?.icon || "")
                color: Appearance.colors.colOnSurfaceVariant
            }

            ColumnLayout {
                spacing: 2
                Layout.fillWidth: true
                StyledText {
                    Layout.fillWidth: true
                    color: Appearance.colors.colOnSurfaceVariant
                    elide: Text.ElideRight
                    text: root.device?.name || Translation.tr("Unknown device")
                }
                StyledText {
                    visible: (root.device?.connected || root.device?.paired) ?? false
                    Layout.fillWidth: true
                    font.pixelSize: Appearance.font.pixelSize.smaller
                    color: Appearance.colors.colSubtext
                    elide: Text.ElideRight
                    text: {
                        if (!root.device?.paired)
                            return "";
                        let statusText = root.device?.connected ? Translation.tr("Connected") : Translation.tr("Paired");
                        if (!root.device?.batteryAvailable)
                            return statusText;
                        statusText += ` • ${Math.round(root.device?.battery * 100)}%`;
                        return statusText;
                    }
                }
            }

            MaterialSymbol {
                text: "keyboard_arrow_down"
                iconSize: Appearance.font.pixelSize.larger
                color: Appearance.colors.colOnLayer3
                rotation: root.expanded ? 180 : 0
                Behavior on rotation {
                    animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                }
            }
        }

        RowLayout {
            visible: root.expanded
            Layout.topMargin: 8
            Item {
                Layout.fillWidth: true
            }
            ActionButton {
                objectName: "connectButton"
                // axon answers conflict while it pairs, connects or disconnects the device.
                enabled: !root.device?.busy
                buttonText: root.device?.connected ? Translation.tr("Disconnect") : Translation.tr("Connect")

                onClicked: {
                    if (root.device?.connected) {
                        BluetoothStatus.disconnectDevice(root.device.id);
                    } else {
                        BluetoothStatus.connectDevice(root.device.id);
                    }
                }
            }
            ActionButton {
                objectName: "forgetButton"
                visible: root.device?.paired ?? false
                enabled: !root.device?.busy
                colBackground: Appearance.colors.colError
                colBackgroundHover: Appearance.colors.colErrorHover
                colRipple: Appearance.colors.colErrorActive
                colText: Appearance.colors.colOnError

                buttonText: Translation.tr("Forget")
                onClicked: {
                    BluetoothStatus.forgetDevice(root.device.id);
                }
            }
        }

        ColumnLayout { // Pairing
            objectName: "pairingPrompt"
            Layout.topMargin: 8
            visible: root.pairingRequest !== null

            StyledText {
                objectName: "pairingText"
                Layout.fillWidth: true
                color: Appearance.colors.colOnSurfaceVariant
                wrapMode: Text.Wrap
                text: root.pairingText(root.pairingRequest)
            }

            MaterialTextField {
                id: pairingField
                objectName: "pairingField"
                Layout.fillWidth: true
                visible: root.pairingRequest?.kind === "request_pin" || root.pairingRequest?.kind === "request_passkey"
                placeholderText: root.pairingRequest?.kind === "request_pin" ? Translation.tr("PIN") : Translation.tr("Passkey")
                echoMode: TextInput.Password
                inputMethodHints: Qt.ImhSensitiveData

                onAccepted: root.replyPairing(true)
            }

            RowLayout {
                Layout.fillWidth: true

                Item {
                    Layout.fillWidth: true
                }

                DialogButton {
                    objectName: "rejectButton"
                    buttonText: root.pairingDisplays ? Translation.tr("Cancel") : Translation.tr("Reject")
                    onClicked: root.replyPairing(false)
                }

                DialogButton {
                    objectName: "acceptButton"
                    enabled: root.pairingValueValid
                    buttonText: root.pairingDisplays ? Translation.tr("Done") : Translation.tr("Accept")
                    onClicked: root.replyPairing(true)
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }
    }
}
