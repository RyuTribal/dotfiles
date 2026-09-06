import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Bluetooth

// The Bluetooth device list, re-rooted for use as a sidebar drill-in page
// instead of the floating BluetoothDialog. Reuses BluetoothDeviceItem so
// connect/disconnect/forget keep working unchanged.
ColumnLayout {
    id: root
    spacing: 8

    StyledIndeterminateProgressBar {
        visible: Bluetooth.defaultAdapter?.discovering ?? false
        Layout.fillWidth: true
    }

    StyledListView {
        Layout.fillWidth: true
        Layout.fillHeight: true
        clip: true
        spacing: 0
        animateAppearance: false

        model: ScriptModel {
            values: [...Bluetooth.devices.values].sort((a, b) => {
                // Connected -> paired -> others
                let conn = (b.connected - a.connected) || (b.paired - a.paired);
                if (conn !== 0)
                    return conn;

                // Ones with meaningful names before MAC addresses
                const macRegex = /^([0-9A-Fa-f]{2}-){5}[0-9A-Fa-f]{2}$/;
                const aIsMac = macRegex.test(a.name);
                const bIsMac = macRegex.test(b.name);
                if (aIsMac !== bIsMac)
                    return aIsMac ? 1 : -1;

                // Alphabetical by name
                return a.name.localeCompare(b.name);
            })
        }
        delegate: BluetoothDeviceItem {
            required property BluetoothDevice modelData
            device: modelData
            anchors {
                left: parent?.left
                right: parent?.right
            }
        }
    }

    RowLayout {
        Layout.fillWidth: true
        Item {
            Layout.fillWidth: true
        }
        DialogButton {
            buttonText: Translation.tr("Configure")
            onClicked: {
                Quickshell.execDetached(["bash", "-c", `${Config.options.apps.bluetooth}`]);
                GlobalStates.sidebarRightOpen = false;
            }
        }
    }
}
