import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt5Compat.GraphicalEffects
import Quickshell.Io
import Quickshell
import Quickshell.Wayland
import Quickshell.Hyprland

WindowDialog {
    id: root

    // The last failed pair or connect, as one line: "<device>: <what went wrong>".
    function errorLine(error) {
        if (!error)
            return "";
        const reasons = {
            rejected: Translation.tr("Pairing was rejected"),
            auth_failed: Translation.tr("Authentication failed"),
            timeout: Translation.tr("Timed out"),
            failed: Translation.tr("Could not connect")
        };
        const name = BluetoothStatus.devices.find(device => device.id === error.device_id)?.name ?? error.device_id;
        return Translation.tr("%1: %2").arg(name).arg(reasons[error.code] ?? error.message);
    }

    WindowDialogTitle {
        text: Translation.tr("Bluetooth devices")
    }
    StyledText {
        objectName: "lastErrorLine"
        visible: BluetoothStatus.lastError !== null
        Layout.fillWidth: true
        wrapMode: Text.NoWrap
        elide: Text.ElideRight
        color: Appearance.colors.colError
        text: root.errorLine(BluetoothStatus.lastError)
    }
    WindowDialogSeparator {
        objectName: "discoverySeparator"
        visible: !BluetoothStatus.discovering
    }
    StyledIndeterminateProgressBar {
        objectName: "discoveryProgress"
        visible: BluetoothStatus.discovering
        Layout.fillWidth: true
        Layout.topMargin: -8
        Layout.bottomMargin: -8
        Layout.leftMargin: -Appearance.rounding.large
        Layout.rightMargin: -Appearance.rounding.large
    }
    StyledListView {
        objectName: "deviceList"
        Layout.fillHeight: true
        Layout.fillWidth: true
        Layout.topMargin: -15
        Layout.bottomMargin: -16
        Layout.leftMargin: -Appearance.rounding.large
        Layout.rightMargin: -Appearance.rounding.large

        clip: true
        spacing: 0
        animateAppearance: false
        // No current item, so a row moving cannot take focus from a PIN being typed (RowModel).
        currentIndex: -1

        // axon sends the rows in display order (section 31), so the view does not sort. RowModel, not a
        // bare ScriptModel: a device that pairs moves up, and its delegate must see its new row.
        model: RowModel {
            rows: BluetoothStatus.devices
        }
        delegate: BluetoothDeviceItem {
            required property var modelData
            device: modelData
            anchors {
                left: parent?.left
                right: parent?.right
            }
        }
    }
    WindowDialogSeparator {}
    WindowDialogButtonRow {
        DialogButton {
            objectName: "detailsButton"
            buttonText: Translation.tr("Details")
            onClicked: {
                Quickshell.execDetached(["bash", "-c", `${Config.options.apps.bluetooth}`]);
                GlobalStates.sidebarRightOpen = false;
            }
        }

        Item {
            Layout.fillWidth: true
        }

        DialogButton {
            buttonText: Translation.tr("Done")
            onClicked: root.dismiss()
        }
    }
}
