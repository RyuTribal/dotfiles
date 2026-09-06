import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.sidebarRight.quickToggles
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Bluetooth
import Quickshell.Hyprland

Item {
    id: root
    property int sidebarWidth: Appearance.sizes.sidebarWidth
    property int sidebarPadding: 12
    property string settingsQmlPath: Quickshell.shellPath("settings.qml")
    property bool toolsPopupOpen: false
    Connections {
        target: GlobalStates
        function onSidebarRightOpenChanged() {
            if (!GlobalStates.sidebarRightOpen) {
                centerWidgetGroup.closeDrillIn();
                root.toolsPopupOpen = false;
            }
        }
    }
    Connections {
        target: centerWidgetGroup
        function onActiveDrillInChanged() {
            // Stop discovering whenever we're not looking at the Bluetooth
            // drill-in, mirroring the old dialog's dismiss behaviour.
            if (centerWidgetGroup.activeDrillIn !== "bluetooth" && Bluetooth.defaultAdapter)
                Bluetooth.defaultAdapter.discovering = false;
        }
    }

    implicitHeight: sidebarRightBackground.implicitHeight
    implicitWidth: sidebarRightBackground.implicitWidth

    StyledRectangularShadow {
        target: sidebarRightBackground
    }
    Rectangle {
        id: sidebarRightBackground

        anchors.fill: parent
        implicitHeight: parent.height - Appearance.sizes.hyprlandGapsOut * 2
        implicitWidth: sidebarWidth - Appearance.sizes.hyprlandGapsOut * 2
        color: Appearance.colors.colLayer0
        border.width: 1
        border.color: Appearance.colors.colLayer0Border
        radius: Appearance.rounding.screenRounding - Appearance.sizes.hyprlandGapsOut + 1

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: sidebarPadding
            spacing: sidebarPadding

            RowLayout {
                Layout.fillHeight: false
                spacing: 10
                Layout.margins: 10
                Layout.topMargin: 5
                Layout.bottomMargin: 0

                CustomIcon {
                    id: distroIcon
                    width: 25
                    height: 25
                    source: SystemInfo.distroIcon
                    colorize: true
                    color: Appearance.colors.colOnLayer0
                }

                StyledText {
                    font.pixelSize: Appearance.font.pixelSize.normal
                    color: Appearance.colors.colOnLayer0
                    text: Translation.tr("Up %1").arg(DateTime.uptime)
                    textFormat: Text.MarkdownText
                }

                Item {
                    Layout.fillWidth: true
                }

                ButtonGroup {
                    QuickToggleButton {
                        toggled: false
                        buttonIcon: "restart_alt"
                        onClicked: {
                            Hyprland.dispatch("reload");
                            Quickshell.reload(true);
                        }
                        StyledToolTip {
                            content: Translation.tr("Reload Hyprland & Quickshell")
                        }
                    }
                    QuickToggleButton {
                        toggled: root.toolsPopupOpen
                        buttonIcon: "construction"
                        onClicked: {
                            root.toolsPopupOpen = !root.toolsPopupOpen;
                        }
                        StyledToolTip {
                            content: Translation.tr("Tools")
                        }
                    }
                    QuickToggleButton {
                        toggled: false
                        buttonIcon: "settings"
                        onClicked: {
                            GlobalStates.sidebarRightOpen = false;
                            Quickshell.execDetached(["qs", "-p", root.settingsQmlPath]);
                        }
                        StyledToolTip {
                            content: Translation.tr("Settings")
                        }
                    }
                    QuickToggleButton {
                        toggled: false
                        buttonIcon: "power_settings_new"
                        onClicked: {
                            GlobalStates.sessionOpen = true;
                        }
                        StyledToolTip {
                            content: Translation.tr("Session")
                        }
                    }
                }
            }

            ButtonGroup {
                Layout.alignment: Qt.AlignHCenter
                spacing: 5
                padding: 5
                color: Appearance.colors.colLayer1

                NetworkToggle {
                    altAction: () => {
                        Network.enableWifi();
                        Network.rescanWifi();
                        centerWidgetGroup.openDrillIn("wifi");
                    }
                }
                BluetoothToggle {
                    altAction: () => {
                        if (Bluetooth.defaultAdapter) {
                            Bluetooth.defaultAdapter.enabled = true;
                            Bluetooth.defaultAdapter.discovering = true;
                        }
                        centerWidgetGroup.openDrillIn("bluetooth");
                    }
                }
                NightLight {}
                GameMode {}
                PowerMode {}
                IdleInhibitor {}
                EasyEffectsToggle {}
                CloudflareWarp {}
            }

            ColumnLayout {
                id: slidersColumn
                Layout.fillWidth: true
                Layout.margins: 5
                spacing: 8

                property var focusedScreen: Quickshell.screens.find(s => s.name === Hyprland.focusedMonitor?.name)
                property var brightnessMonitor: Brightness.getMonitorForScreen(focusedScreen)

                RowLayout { // Volume
                    Layout.fillWidth: true
                    spacing: 10

                    MaterialSymbol {
                        iconSize: Appearance.font.pixelSize.larger
                        text: Audio.materialSymbol
                        color: Appearance.colors.colOnLayer0
                    }
                    StyledSlider {
                        Layout.fillWidth: true
                        value: Audio.ready ? Audio.sink.audio.volume : 0
                        onValueChanged: {
                            if (Audio.ready)
                                Audio.setVolumeLinear(value);
                        }
                    }
                    QuickToggleButton {
                        toggled: false
                        buttonIcon: "chevron_right"
                        onClicked: centerWidgetGroup.openDrillIn("mixer")
                        StyledToolTip {
                            content: Translation.tr("Volume mixer")
                        }
                    }
                }

                RowLayout { // Brightness
                    Layout.fillWidth: true
                    spacing: 10

                    MaterialSymbol {
                        iconSize: Appearance.font.pixelSize.larger
                        text: "light_mode"
                        color: Appearance.colors.colOnLayer0
                    }
                    StyledSlider {
                        Layout.fillWidth: true
                        value: slidersColumn.brightnessMonitor?.brightness ?? 0
                        onValueChanged: {
                            if (slidersColumn.brightnessMonitor?.ready)
                                slidersColumn.brightnessMonitor.setBrightness(value);
                        }
                    }
                }
            }

            CenterWidgetGroup {
                id: centerWidgetGroup
                Layout.alignment: Qt.AlignHCenter
                Layout.fillHeight: true
                Layout.fillWidth: true
            }
        }

        // Placed after the ColumnLayout so it paints above the rest of the
        // sidebar's content; only the header's tools button toggles it.
        ToolsPopup {
            open: root.toolsPopupOpen
        }
    }
}
