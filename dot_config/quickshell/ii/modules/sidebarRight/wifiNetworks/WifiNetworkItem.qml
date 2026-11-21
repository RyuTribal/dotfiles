import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts
import Quickshell

DialogListItem {
    id: root
    // A row of Network.wifiNetworks.
    required property var wifiNetwork

    readonly property string rowId: wifiNetwork?.id ?? ""
    // NetworkManager asks for this row's password. The field and its buttons act only while it is true,
    // so a typed password can never go to another network.
    readonly property bool prompting: Network.passwordApId !== "" && Network.passwordApId === rowId

    active: (Network.passwordApId === wifiNetwork?.id || wifiNetwork?.active) ?? false
    onClicked: {
        Network.connectToWifiNetwork(wifiNetwork);
    }
    onPromptingChanged: root.resetPrompt()
    onRowIdChanged: root.resetPrompt()

    // The prompt closed, opened or now belongs to another row: what was typed goes, with the focus.
    function resetPrompt() {
        passwordField.text = "";
        passwordField.focus = false;
    }

    // Whether the prompt is open here for this row. An action from a prompt that is not is a mistake:
    // it is refused loudly, never sent.
    function promptOpen(action) {
        if (prompting)
            return true;
        console.warn(`WifiNetworkItem: ${action} for ${rowId || "no row"} not sent: its prompt is not open`);
        return false;
    }

    function submitPassword() {
        if (promptOpen("password"))
            Network.changePassword(root.wifiNetwork, passwordField.text);
    }

    contentItem: ColumnLayout {
        anchors {
            fill: parent
            topMargin: root.verticalPadding
            bottomMargin: root.verticalPadding
            leftMargin: root.horizontalPadding
            rightMargin: root.horizontalPadding
        }
        spacing: 0

        RowLayout {
            // Name
            spacing: 10
            MaterialSymbol {
                iconSize: Appearance.font.pixelSize.larger
                property int strength: root.wifiNetwork?.strength ?? 0
                text: strength > 80 ? "signal_wifi_4_bar" : strength > 60 ? "network_wifi_3_bar" : strength > 40 ? "network_wifi_2_bar" : strength > 20 ? "network_wifi_1_bar" : "signal_wifi_0_bar"
                color: Appearance.colors.colOnSurfaceVariant
            }
            StyledText {
                Layout.fillWidth: true
                color: Appearance.colors.colOnSurfaceVariant
                elide: Text.ElideRight
                text: root.wifiNetwork?.ssid ?? Translation.tr("Unknown")
            }
            MaterialSymbol {
                objectName: "stateSymbol"
                visible: (root.wifiNetwork?.isSecure || root.wifiNetwork?.active) ?? false
                text: root.wifiNetwork?.active ? "check" : Network.connectingApId === root.wifiNetwork?.id ? "settings_ethernet" : "lock"
                iconSize: Appearance.font.pixelSize.larger
                color: Appearance.colors.colOnSurfaceVariant
            }
        }

        ColumnLayout { // Password
            id: passwordPrompt
            objectName: "passwordPrompt"
            Layout.topMargin: 8
            visible: root.prompting

            MaterialTextField {
                id: passwordField
                objectName: "passwordField"
                Layout.fillWidth: true
                placeholderText: Translation.tr("Password")

                // Password
                echoMode: TextInput.Password
                inputMethodHints: Qt.ImhSensitiveData

                onAccepted: root.submitPassword()
            }

            RowLayout {
                Layout.fillWidth: true

                Item {
                    Layout.fillWidth: true
                }

                DialogButton {
                    objectName: "cancelButton"
                    buttonText: Translation.tr("Cancel")
                    onClicked: {
                        if (root.promptOpen("cancel"))
                            Network.cancelPassword(root.rowId);
                    }
                }

                DialogButton {
                    buttonText: Translation.tr("Connect")
                    onClicked: root.submitPassword()
                }
            }
        }

        ColumnLayout { // Public wifi login page
            id: publicWifiPortal
            objectName: "publicWifiPortal"
            Layout.topMargin: 8
            visible: (root.wifiNetwork?.active && Network.portalDetected) ?? false

            RowLayout {
                DialogButton {
                    objectName: "portalButton"
                    Layout.fillWidth: true
                    buttonText: Translation.tr("Open network portal")
                    colBackground: Appearance.colors.colLayer4
                    colBackgroundHover: Appearance.colors.colLayer4Hover
                    colRipple: Appearance.colors.colLayer4Active
                    onClicked: {
                        Quickshell.execDetached(["xdg-open", Network.portalUrl]);
                        GlobalStates.sidebarRightOpen = false;
                    }
                }
            }
        }

        Item {
            Layout.fillHeight: true
        }
    }
}
