import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import Quickshell.Io
import Quickshell
import Quickshell.Wayland
import Quickshell.Hyprland

Scope { // Scope
    id: root
    property Component contentComponent: TopMenuContent {}
    property Item topMenuContent

    Component.onCompleted: {
        root.topMenuContent = contentComponent.createObject(null, {
            "scopeRoot": root,
        });
        topMenuLoader.item.contentParent.children = [root.topMenuContent];
    }

    Loader {
        id: topMenuLoader
        active: true

        sourceComponent: PanelWindow { // Window
            id: topMenuRoot
            visible: GlobalStates.topMenuOpen

            property var contentParent: topMenuBackground

            function hide() {
                GlobalStates.topMenuOpen = false
            }

            exclusiveZone: 0
            implicitWidth: 860 + Appearance.sizes.elevationMargin * 2
            implicitHeight: 520 + Appearance.sizes.elevationMargin * 2
            WlrLayershell.namespace: "quickshell:topMenu"
            color: "transparent"

            anchors {
                top: true
                left: true
                right: true
            }

            mask: Region {
                item: topMenuBackground
            }

            HyprlandFocusGrab { // Click outside to close
                id: grab
                windows: [ topMenuRoot ]
                active: false
                onActiveChanged: { // Focus the selected tab
                    if (active) topMenuBackground.children[0].focusActiveItem()
                }
                onCleared: () => {
                    if (!active) topMenuRoot.hide()
                }
            }

            Connections {
                target: GlobalStates
                function onTopMenuOpenChanged() {
                    grab.active = false;
                }
            }

            // Content
            StyledRectangularShadow {
                target: topMenuBackground
                radius: topMenuBackground.radius
            }
            Rectangle {
                id: topMenuBackground
                anchors.top: parent.top
                anchors.horizontalCenter: parent.horizontalCenter
                anchors.topMargin: Appearance.sizes.hyprlandGapsOut
                implicitWidth: 860
                height: 520
                color: Appearance.colors.colLayer0
                border.width: 1
                border.color: Appearance.colors.colLayer0Border
                radius: Appearance.rounding.screenRounding - Appearance.sizes.hyprlandGapsOut + 1

                HoverHandler {
                    acceptedDevices: PointerDevice.AllPointerTypes
                    onHoveredChanged: {
                        if (hovered && GlobalStates.topMenuOpen && !grab.active) {
                            grab.active = true;
                        }
                    }
                }

                Keys.onPressed: (event) => {
                    if (event.key === Qt.Key_Escape) {
                        topMenuRoot.hide();
                    }
                }
            }
        }
    }

    IpcHandler {
        target: "topMenu"

        function toggle(): void {
            GlobalStates.topMenuOpen = !GlobalStates.topMenuOpen
        }

        function close(): void {
            GlobalStates.topMenuOpen = false
        }

        function open(): void {
            GlobalStates.topMenuOpen = true
        }

        function openTab(tab: string): void {
            GlobalStates.topMenuTab = tab;
            GlobalStates.topMenuOpen = true;
        }
    }

    GlobalShortcut {
        name: "topMenuToggle"
        description: "Toggles top menu on press"

        onPressed: {
            GlobalStates.topMenuOpen = !GlobalStates.topMenuOpen;
        }
    }

    GlobalShortcut {
        name: "topMenuOpen"
        description: "Opens top menu on press"

        onPressed: {
            GlobalStates.topMenuOpen = true;
        }
    }

    GlobalShortcut {
        name: "topMenuClose"
        description: "Closes top menu on press"

        onPressed: {
            GlobalStates.topMenuOpen = false;
        }
    }

}
