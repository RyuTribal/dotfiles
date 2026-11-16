import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland

// Shows while Brightness.osdVisible: axon's brightness engine publishes the instant of each change that
// shows the OSD (`osd_at`) per output, and the Brightness service keeps the focused output's on screen
// for Config.options.osd.timeout. Of this and the volume OSD only the later change shows.
Scope {
    id: root
    property var focusedScreen: Quickshell.screens.find(s => s.name === Hypr.focusedMonitorName)
    property var brightnessMonitor: Brightness.getMonitorForScreen(focusedScreen)

    Loader {
        id: osdLoader
        active: Brightness.osdVisible && Brightness.osdAt > Audio.osdAt

        sourceComponent: PanelWindow {
            id: osdRoot
            color: "transparent"

            Connections {
                target: root
                function onFocusedScreenChanged() {
                    osdRoot.screen = root.focusedScreen
                }
            }

            WlrLayershell.namespace: "quickshell:onScreenDisplay"
            WlrLayershell.layer: WlrLayer.Overlay
            anchors {
                top: !Config.options.bar.bottom
                bottom: Config.options.bar.bottom
            }
            mask: Region {
                item: osdValuesWrapper
            }

            exclusionMode: ExclusionMode.Ignore
            exclusiveZone: 0
            margins {
                top: Appearance.sizes.barHeight
                bottom: Appearance.sizes.barHeight
            }

            implicitWidth: columnLayout.implicitWidth
            implicitHeight: columnLayout.implicitHeight
            visible: osdLoader.active

            ColumnLayout {
                id: columnLayout
                anchors.horizontalCenter: parent.horizontalCenter
                Item {
                    id: osdValuesWrapper
                    // Extra space for shadow
                    implicitHeight: osdValues.implicitHeight + Appearance.sizes.elevationMargin * 2
                    implicitWidth: osdValues.implicitWidth
                    clip: true

                    MouseArea {
                        anchors.fill: parent
                        hoverEnabled: true
                        onEntered: Brightness.hideOsd()
                    }

                    Behavior on implicitHeight {
                        NumberAnimation {
                            duration: Appearance.animation.menuDecel.duration
                            easing.type: Appearance.animation.menuDecel.type
                        }
                    }

                    OsdValueIndicator {
                        id: osdValues
                        anchors.fill: parent
                        anchors.margins: Appearance.sizes.elevationMargin
                        value: root.brightnessMonitor.brightness
                        icon: "light_mode"
                        rotateIcon: true
                        scaleIcon: true
                        name: Translation.tr("Brightness")
                    }
                }
            }

        }
    }
}
