import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs.modules.sidebarRight.notifications
import qs.modules.sidebarRight.volumeMixer
import qs.modules.sidebarRight.wifiNetworks
import qs.modules.sidebarRight.bluetoothDevices
import qs
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

Rectangle {
    id: root
    radius: Appearance.rounding.normal
    color: Appearance.colors.colLayer1

    // "" shows the default view (notifications). Any other value drills
    // into the matching page below, swapped in via the SwipeView the same
    // way this area already swapped between tabs before this change.
    property string activeDrillIn: ""
    readonly property bool drillInActive: activeDrillIn !== ""

    function openDrillIn(name) {
        root.activeDrillIn = name;
    }
    function closeDrillIn() {
        root.activeDrillIn = "";
    }

    Keys.onPressed: event => {
        if (event.key === Qt.Key_Escape && root.drillInActive) {
            root.closeDrillIn();
            event.accepted = true;
        }
    }

    ColumnLayout {
        anchors.margins: 5
        anchors.fill: parent
        spacing: 0

        SwipeView {
            id: swipeView
            Layout.fillWidth: true
            Layout.fillHeight: true
            // Drill-in pages are opened and closed through explicit triggers
            // (chevron, back button), not by swiping between them.
            interactive: false
            currentIndex: root.drillInActive ? 1 : 0

            clip: true
            layer.enabled: true
            layer.effect: OpacityMask {
                maskSource: Rectangle {
                    width: swipeView.width
                    height: swipeView.height
                    radius: Appearance.rounding.small
                }
            }

            NotificationList {}

            Loader {
                active: root.drillInActive
                sourceComponent: root.activeDrillIn === "mixer" ? mixerDrillIn : root.activeDrillIn === "wifi" ? wifiDrillIn : root.activeDrillIn === "bluetooth" ? bluetoothDrillIn : null
            }
        }
    }

    Component {
        id: mixerDrillIn
        DrillInView {
            title: Translation.tr("Volume mixer")
            onBack: root.closeDrillIn()
            VolumeMixer {
                Layout.fillWidth: true
                Layout.fillHeight: true
            }
        }
    }
    Component {
        id: wifiDrillIn
        DrillInView {
            title: Translation.tr("Wi-Fi networks")
            onBack: root.closeDrillIn()
            WifiNetworksView {
                Layout.fillWidth: true
                Layout.fillHeight: true
            }
        }
    }
    Component {
        id: bluetoothDrillIn
        DrillInView {
            title: Translation.tr("Bluetooth devices")
            onBack: root.closeDrillIn()
            BluetoothDevicesView {
                Layout.fillWidth: true
                Layout.fillHeight: true
            }
        }
    }
}
