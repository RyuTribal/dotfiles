import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import Quickshell.Hyprland

// Centered modal for the right sidebar's Tools button, replacing the old
// anchored ToolsPopup. Anchored popups can't cover the whole screen and read
// as a stray menu rather than a deliberate surface, so this follows the
// codebase's full-screen-layer modal idiom instead - the same one
// SessionScreen.qml (config root) uses for the session screen: a Scope owns
// a Loader gated on a GlobalStates flag, whose sourceComponent is a
// PanelWindow spanning the focused screen. That gives a real centered
// dialog over the entire output, independent of the sidebar's own
// PanelWindow and column layout, rather than trying to make an Item modal
// inside the sidebar look "centered" within just the sidebar's width.
//
// Content styling borrows SelectionDialog's scrim + colSurfaceContainerHigh
// dialog surface (modules/common/widgets/SelectionDialog.qml), with a
// killDialog.qml-style top-right close button in place of SelectionDialog's
// bottom Cancel/OK row, since there's no choice here to confirm or cancel -
// only a set of actions to launch or a modal to dismiss.
//
// Growing the tool list: each entry is one object in the `tools` array
// below - name, icon, description and an action() to run. No registry
// service or config schema, just add a line here for the next tool.
Scope {
    id: root

    readonly property var tools: [
        {
            name: Translation.tr("Sweep"),
            icon: "storage",
            description: Translation.tr("Disk usage inspector"),
            action: () => {
                // SweepPanel.qml owns its own visibility as a local
                // property on its Scope (not a GlobalStates flag), toggled
                // through its IpcHandler{ target: "sweep" } - see the
                // toggle()/show()/hide() functions there. Match that API.
                Quickshell.execDetached(["qs", "-c", "ii", "ipc", "call", "sweep", "toggle"]);
            }
        }
    ]

    component ToolCard: RippleButton {
        id: card
        required property var tool

        Layout.preferredWidth: 176
        Layout.preferredHeight: 176
        buttonRadius: Appearance.rounding.large
        // One tone step up from the dialog's own colSurfaceContainerHigh so
        // the card reads as sitting on top of it (M3's dark-theme elevation
        // convention: higher elevation = lighter tone), rather than blending
        // into the dialog background the way colLayer2 (a lower tone) did
        // in the first pass.
        colBackground: Appearance.colors.colLayer4
        colBackgroundHover: Appearance.colors.colLayer4Hover
        colRipple: Appearance.colors.colLayer4Active

        onClicked: {
            card.tool.action();
            toolsLoader.item?.hide();
        }

        contentItem: ColumnLayout {
            anchors.fill: parent
            anchors.margins: 16
            spacing: 10

            Item {
                Layout.alignment: Qt.AlignHCenter
                implicitWidth: 64
                implicitHeight: 64

                Rectangle { // Tonal icon circle
                    anchors.fill: parent
                    radius: Appearance.rounding.full
                    color: Appearance.colors.colSecondaryContainer
                }
                MaterialSymbol {
                    anchors.centerIn: parent
                    text: card.tool.icon
                    iconSize: 36
                    color: Appearance.colors.colOnSecondaryContainer
                }
            }

            StyledText {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignHCenter
                horizontalAlignment: Text.AlignHCenter
                text: card.tool.name
                font.pixelSize: Appearance.font.pixelSize.normal
                font.weight: Font.Medium
                color: Appearance.colors.colOnLayer4
            }
            StyledText {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignHCenter
                horizontalAlignment: Text.AlignHCenter
                wrapMode: Text.WordWrap
                text: card.tool.description
                font.pixelSize: Appearance.font.pixelSize.smaller
                color: Appearance.colors.colSubtext
            }
        }
    }

    Loader {
        id: toolsLoader
        active: GlobalStates.toolsOpen

        // Mirrors SessionScreen's own Loader: dismiss without ceremony if
        // the screen locks while the modal happens to be open.
        Connections {
            target: GlobalStates
            function onScreenLockedChanged() {
                if (GlobalStates.screenLocked)
                    GlobalStates.toolsOpen = false;
            }
        }

        sourceComponent: PanelWindow {
            id: toolsRoot
            readonly property HyprlandMonitor monitor: Hyprland.monitorFor(toolsRoot.screen)
            property var focusedScreen: Quickshell.screens.find(s => s.name === Hyprland.focusedMonitor?.name)

            // Drives the scale+fade in/out. Starts false so the dialog's
            // first frame is drawn already hidden, then flips true right
            // after creation so the Behaviors below animate it open -
            // rather than popping in at full size/opacity like the plain
            // visibility toggles SessionScreen/WallpaperSelector use.
            property bool activated: false
            Component.onCompleted: toolsRoot.activated = true

            function hide() {
                if (!toolsRoot.activated)
                    return;
                toolsRoot.activated = false;
            }

            Timer {
                id: closeTimer
                interval: Appearance.animation.elementMoveExit.duration
                onTriggered: GlobalStates.toolsOpen = false
            }
            Connections {
                target: toolsRoot
                function onActivatedChanged() {
                    if (!toolsRoot.activated)
                        closeTimer.restart();
                }
            }

            visible: toolsLoader.active
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.namespace: "quickshell:toolsModal"
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
            color: "transparent"

            anchors {
                top: true
                left: true
                right: true
            }
            implicitWidth: toolsRoot.focusedScreen?.width ?? 0
            implicitHeight: toolsRoot.focusedScreen?.height ?? 0

            Rectangle { // Scrim
                id: scrim
                anchors.fill: parent
                color: Appearance.colors.colScrim
                opacity: toolsRoot.activated ? 1 : 0

                Behavior on opacity {
                    NumberAnimation {
                        duration: Appearance.animation.elementMoveFast.duration
                        easing.type: Appearance.animation.elementMoveFast.type
                        easing.bezierCurve: Appearance.animation.elementMoveFast.bezierCurve
                    }
                }

                MouseArea {
                    anchors.fill: parent
                    onClicked: toolsRoot.hide()
                }
            }

            StyledRectangularShadow {
                target: dialog
            }

            Rectangle { // The dialog
                id: dialog
                anchors.centerIn: parent
                implicitWidth: dialogColumn.implicitWidth + 48
                implicitHeight: dialogColumn.implicitHeight + 48
                radius: Appearance.rounding.large
                color: Appearance.colors.colSurfaceContainerHigh
                border.width: 1
                border.color: Appearance.colors.colLayer0Border

                opacity: toolsRoot.activated ? 1 : 0
                scale: toolsRoot.activated ? 1 : 0.9
                transformOrigin: Item.Center

                Behavior on opacity {
                    NumberAnimation {
                        duration: toolsRoot.activated ? Appearance.animation.elementMoveEnter.duration : Appearance.animation.elementMoveExit.duration
                        easing.type: Appearance.animation.elementMoveEnter.type
                        easing.bezierCurve: toolsRoot.activated ? Appearance.animationCurves.emphasizedDecel : Appearance.animationCurves.emphasizedAccel
                    }
                }
                Behavior on scale {
                    NumberAnimation {
                        duration: toolsRoot.activated ? Appearance.animation.elementMoveEnter.duration : Appearance.animation.elementMoveExit.duration
                        easing.type: Appearance.animation.elementMoveEnter.type
                        easing.bezierCurve: toolsRoot.activated ? Appearance.animationCurves.emphasizedDecel : Appearance.animationCurves.emphasizedAccel
                    }
                }

                // Swallow clicks so they don't fall through to the scrim
                // behind (declared before dialogColumn so the column's own
                // buttons still take priority in hit-testing).
                MouseArea {
                    anchors.fill: parent
                    onClicked: {}
                }

                focus: true
                Keys.onPressed: event => {
                    if (event.key === Qt.Key_Escape) {
                        toolsRoot.hide();
                        event.accepted = true;
                    }
                }

                ColumnLayout {
                    id: dialogColumn
                    anchors.centerIn: parent
                    spacing: 18

                    RowLayout { // Title row
                        Layout.fillWidth: true
                        spacing: 12

                        MaterialSymbol {
                            text: "construction"
                            iconSize: Appearance.font.pixelSize.huge
                            color: Appearance.colors.colOnLayer1
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            spacing: 2

                            StyledText {
                                text: Translation.tr("Tools")
                                font.family: Appearance.font.family.title
                                font.pixelSize: Appearance.font.pixelSize.title
                                font.weight: Font.DemiBold
                                color: Appearance.colors.colOnLayer1
                            }
                            StyledText {
                                text: Translation.tr("Shell utilities and daemons")
                                font.pixelSize: Appearance.font.pixelSize.small
                                color: Appearance.colors.colSubtext
                            }
                        }

                        RippleButton {
                            buttonRadius: Appearance.rounding.full
                            implicitWidth: 36
                            implicitHeight: 36
                            colBackground: "transparent"
                            colBackgroundHover: Appearance.colors.colLayer1Hover
                            onClicked: toolsRoot.hide()
                            contentItem: MaterialSymbol {
                                anchors.centerIn: parent
                                text: "close"
                                iconSize: 20
                                color: Appearance.colors.colOnLayer1
                            }
                        }
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        implicitHeight: 1
                        color: Appearance.colors.colLayer0Border
                    }

                    GridLayout {
                        Layout.alignment: Qt.AlignHCenter
                        columns: Math.max(1, Math.min(3, root.tools.length))
                        rowSpacing: 14
                        columnSpacing: 14

                        Repeater {
                            model: root.tools
                            delegate: ToolCard {
                                required property var modelData
                                tool: modelData
                            }
                        }
                    }
                }
            }
        }
    }

    IpcHandler {
        target: "tools"

        function toggle(): void {
            GlobalStates.toolsOpen = !GlobalStates.toolsOpen;
        }

        function open(): void {
            GlobalStates.toolsOpen = true;
        }

        function close(): void {
            GlobalStates.toolsOpen = false;
        }
    }
}
