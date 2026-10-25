import qs
import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Wayland
pragma Singleton

/**
 * A nice wrapper for date and time strings.
 */
Singleton {
    id: root

    readonly property bool inhibit: Persistent.states.idle.inhibit

    function load() {} // Dummy to force init

    function toggleInhibit() {
        Persistent.states.idle.inhibit = !Persistent.states.idle.inhibit
    }

    // One inhibitor per screen: a window dies with its screen, so a single one
    // silently stops inhibiting after a monitor is unplugged.
    Variants {
        model: Quickshell.screens

        Scope {
            id: perScreen
            required property ShellScreen modelData
            property bool mapped: false

            PanelWindow { // Inhibitor requires a visible surface
                id: inhibitorWindow
                screen: perScreen.modelData
                implicitWidth: 1
                implicitHeight: 1
                color: "transparent"
                anchors {
                    right: true
                    bottom: true
                }
                // Make it not interactable
                mask: Region {
                    item: null
                }
            }

            // Hyprland checks an inhibitor's surface only when the inhibitor is
            // created; one created before the surface is mapped is ignored until
            // the next focus change. Wait for the map before creating it.
            Timer {
                running: inhibitorWindow.backingWindowVisible
                interval: 1000
                onTriggered: perScreen.mapped = true
            }

            IdleInhibitor {
                enabled: root.inhibit && perScreen.mapped
                window: inhibitorWindow
            }
        }
    }
}
