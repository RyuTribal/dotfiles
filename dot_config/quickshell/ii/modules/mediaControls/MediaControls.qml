import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import qs.modules.common.functions
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import Quickshell.Hyprland

Scope {
    id: root
    property bool visible: false
    readonly property var activePlayer: MprisController.activePlayer
    // axon filters and merges the players (section 31, media); MprisController
    // maps them as meaningfulPlayers, shared with the topMenu Media tab. Kept as a
    // local alias so the rest of this file reads exactly as it did before.
    readonly property var meaningfulPlayers: MprisController.meaningfulPlayers
    readonly property real osdWidth: Appearance.sizes.osdWidth
    readonly property real widgetWidth: Appearance.sizes.mediaControlsWidth
    readonly property real widgetHeight: Appearance.sizes.mediaControlsHeight
    property real contentPadding: 13
    property real popupRounding: Appearance.rounding.screenRounding - Appearance.sizes.elevationMargin + 1
    property real artRounding: Appearance.rounding.verysmall
    property list<real> visualizerPoints: []

    // The waveform: axon's one shared spectrum, wanted while the popup is open (section 31,
    // media.spectrum). The bars clear when it closes.
    BackendTopic {
        name: "media.spectrum"
        active: mediaControlsLoader.active
        onSnapshot: data => root.visualizerPoints = data.bars
        onActiveChanged: if (!active)
            root.visualizerPoints = []
    }

    Loader {
        id: mediaControlsLoader
        active: GlobalStates.mediaControlsOpen
        onActiveChanged: {
            if (!mediaControlsLoader.active && root.meaningfulPlayers.length === 0) {
                GlobalStates.mediaControlsOpen = false;
            }
        }

        sourceComponent: PanelWindow {
            id: mediaControlsRoot
            visible: true

            exclusionMode: ExclusionMode.Ignore
            exclusiveZone: 0
            implicitWidth: root.widgetWidth
            implicitHeight: playerColumnLayout.implicitHeight
            color: "transparent"
            WlrLayershell.namespace: "quickshell:mediaControls"

            anchors {
                top: !Config.options.bar.bottom || Config.options.bar.vertical
                bottom: Config.options.bar.bottom && !Config.options.bar.vertical
                left: !(Config.options.bar.vertical && Config.options.bar.bottom)
                right: Config.options.bar.vertical && Config.options.bar.bottom
            }
            margins {
                top: Config.options.bar.vertical ? ((mediaControlsRoot.screen.height / 2) - widgetHeight * 1.5) : Appearance.sizes.barHeight
                bottom: Appearance.sizes.barHeight
                left: Config.options.bar.vertical ? Appearance.sizes.barHeight : ((mediaControlsRoot.screen.width / 2) - (osdWidth / 2) - widgetWidth)
                right: Appearance.sizes.barHeight
            }

            mask: Region {
                item: playerColumnLayout
            }

            HyprlandFocusGrab {
                id: grab
                windows: [mediaControlsRoot]
                active: false
                onCleared: () => {
                    if (!active) {
                        GlobalStates.mediaControlsOpen = false;
                    }
                }
            }

            HoverHandler {
                acceptedDevices: PointerDevice.AllPointerTypes
                onHoveredChanged: {
                    if (hovered && GlobalStates.mediaControlsOpen && !grab.active) {
                        grab.active = true;
                    }
                }
            }

            Connections {
                target: GlobalStates
                function onMediaControlsOpenChanged() {
                    if (!GlobalStates.mediaControlsOpen) {
                        grab.active = false;
                    }
                }
            }

            ColumnLayout {
                id: playerColumnLayout
                anchors.fill: parent
                spacing: -Appearance.sizes.elevationMargin // Shadow overlap okay

                Repeater {
                    model: ScriptModel {
                        values: MprisController.playerIds
                    }
                    delegate: PlayerControl {
                        required property string modelData
                        player: MprisController.byId[modelData] ?? null
                        visualizerPoints: root.visualizerPoints
                        implicitWidth: widgetWidth
                        implicitHeight: widgetHeight
                        radius: root.popupRounding
                    }
                }

                Item {
                    // No player placeholder
                    Layout.fillWidth: true
                    visible: root.meaningfulPlayers.length === 0
                    implicitWidth: placeholderBackground.implicitWidth + Appearance.sizes.elevationMargin
                    implicitHeight: placeholderBackground.implicitHeight + Appearance.sizes.elevationMargin

                    StyledRectangularShadow {
                        target: placeholderBackground
                    }

                    Rectangle {
                        id: placeholderBackground
                        anchors.centerIn: parent
                        color: Appearance.colors.colLayer0
                        radius: root.popupRounding
                        property real padding: 20
                        implicitWidth: placeholderLayout.implicitWidth + padding * 2
                        implicitHeight: placeholderLayout.implicitHeight + padding * 2

                        ColumnLayout {
                            id: placeholderLayout
                            anchors.centerIn: parent

                            StyledText {
                                text: Translation.tr("No active player")
                                font.pixelSize: Appearance.font.pixelSize.large
                            }
                            StyledText {
                                color: Appearance.colors.colSubtext
                                text: Translation.tr("Make sure your player has MPRIS support\nor try turning off duplicate player filtering")
                                font.pixelSize: Appearance.font.pixelSize.small
                            }
                        }
                    }
                }
            }
        }
    }

    IpcHandler {
        target: "mediaControls"

        function toggle(): void {
            mediaControlsLoader.active = !mediaControlsLoader.active;
            if (mediaControlsLoader.active)
                Notifications.timeoutAll();
        }

        function close(): void {
            mediaControlsLoader.active = false;
        }

        function open(): void {
            mediaControlsLoader.active = true;
            Notifications.timeoutAll();
        }
    }

    GlobalShortcut {
        name: "mediaControlsToggle"
        description: "Toggles media controls on press"

        onPressed: {
            GlobalStates.mediaControlsOpen = !GlobalStates.mediaControlsOpen;
        }
    }
    GlobalShortcut {
        name: "mediaControlsOpen"
        description: "Opens media controls on press"

        onPressed: {
            GlobalStates.mediaControlsOpen = true;
        }
    }
    GlobalShortcut {
        name: "mediaControlsClose"
        description: "Closes media controls on press"

        onPressed: {
            GlobalStates.mediaControlsOpen = false;
        }
    }
}
