import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts
import Quickshell

WindowDialog {
    id: root

    // The last failed join, as one line: "<ssid>: <what went wrong>".
    function errorLine(error) {
        if (!error)
            return "";
        const reasons = {
            wrong_password: Translation.tr("Wrong password"),
            no_secrets: Translation.tr("No password was given"),
            timeout: Translation.tr("Timed out"),
            unsupported_security: Translation.tr("This network's security is not supported"),
            failed: Translation.tr("Could not connect")
        };
        return Translation.tr("%1: %2").arg(error.ssid).arg(reasons[error.code] ?? error.message);
    }

    WindowDialogTitle {
        text: Translation.tr("Connect to Wi-Fi")
    }
    StyledText {
        objectName: "lastErrorLine"
        visible: Network.lastError !== null
        Layout.fillWidth: true
        wrapMode: Text.NoWrap
        elide: Text.ElideRight
        color: Appearance.colors.colError
        text: root.errorLine(Network.lastError)
    }
    WindowDialogSeparator {
        visible: !Network.wifiScanning
    }
    StyledIndeterminateProgressBar {
        visible: Network.wifiScanning
        Layout.fillWidth: true
        Layout.topMargin: -8
        Layout.bottomMargin: -8
        Layout.leftMargin: -Appearance.rounding.large
        Layout.rightMargin: -Appearance.rounding.large
    }
    ListView {
        objectName: "networkList"
        Layout.fillHeight: true
        Layout.fillWidth: true
        Layout.topMargin: -15
        Layout.bottomMargin: -16
        Layout.leftMargin: -Appearance.rounding.large
        Layout.rightMargin: -Appearance.rounding.large

        clip: true
        spacing: 0
        // No current item, so a row moving cannot take focus from a password being typed (RowModel).
        currentIndex: -1

        // axon sends the rows in display order (section 31), so the view does not sort. RowModel, not a
        // bare ScriptModel: a network that is joined or changes strength bucket moves, and its delegate
        // must see its new row.
        model: RowModel {
            rows: Network.wifiNetworks
        }
        delegate: WifiNetworkItem {
            required property var modelData
            wifiNetwork: modelData
            anchors {
                left: parent?.left
                right: parent?.right
            }
        }
    }
    WindowDialogSeparator {}
    WindowDialogButtonRow {
        DialogButton {
            buttonText: Translation.tr("Details")
            onClicked: {
                Quickshell.execDetached(["bash", "-c", `${Network.ethernet ? Config.options.apps.networkEthernet : Config.options.apps.network}`]);
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
