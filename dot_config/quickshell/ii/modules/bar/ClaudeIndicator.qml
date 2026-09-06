import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts

// Bar icon + label for the Claude Code 5-hour rate-limit window (see
// services/ClaudeUsage.qml). Click jumps to the top menu's Stats tab, same
// as the Resources area. Hovering shows a StyledPopup preview — the same
// component ResourcesPopup/BatteryPopup use — rather than the StyledToolTip
// batch 2 added, so the bar has one consistent hover-preview idiom instead
// of two competing ones. ClaudeUsage's own FileView watch is always-on
// already, so this widget adds no polling of its own — it only reads
// properties ClaudeUsage already keeps live.
MouseArea {
    id: root
    implicitWidth: rowLayout.implicitWidth
    implicitHeight: Appearance.sizes.barHeight
    hoverEnabled: true
    opacity: ClaudeUsage.live ? 1 : 0.5

    readonly property bool valid: ClaudeUsage.live && ClaudeUsage.fiveHourPct >= 0
    readonly property real clampedPct: Math.max(0, Math.min(100, ClaudeUsage.fiveHourPct))

    // Local clock tick purely so the popup's countdown keeps counting down
    // while hovered, mirroring ClaudePanel.qml's same trick.
    property double nowMs: Date.now()
    Timer {
        interval: 30000
        running: true
        repeat: true
        onTriggered: root.nowMs = Date.now()
    }

    function resetCountdown(resetsAt) {
        if (resetsAt <= 0)
            return Translation.tr("unknown");
        const mins = Math.max(0, Math.round((resetsAt * 1000 - root.nowMs) / 60000));
        return Translation.tr("%1m").arg(mins);
    }

    onPressed: (event) => {
        if (event.button === Qt.LeftButton) {
            GlobalStates.topMenuTab = "stats";
            GlobalStates.topMenuOpen = true;
        }
    }

    RowLayout {
        id: rowLayout
        anchors.centerIn: parent
        spacing: 2

        MaterialSymbol {
            text: "neurology"
            iconSize: Appearance.font.pixelSize.normal
            color: Appearance.colors.colOnLayer1
        }
        StyledText {
            text: root.valid ? `${Math.round(root.clampedPct)}%` : "--"
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colOnLayer1
        }
    }

    StyledPopup {
        hoverTarget: root

        // anchors.centerIn: parent matches BatteryPopup/ResourcesPopup's
        // content root — without it, this ColumnLayout sat unanchored at
        // popupBackground's (0,0) corner, eating all of StyledPopup's 10px
        // margin on the right/bottom and none on the top/left, which read
        // as cramped compared to the other popups.
        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            // Header
            RowLayout {
                spacing: 5

                MaterialSymbol {
                    fill: 0
                    font.weight: Font.Medium
                    text: "neurology"
                    iconSize: Appearance.font.pixelSize.large
                    color: Appearance.colors.colOnSurfaceVariant
                }
                StyledText {
                    text: Translation.tr("Claude")
                    font {
                        weight: Font.Medium
                        pixelSize: Appearance.font.pixelSize.normal
                    }
                    color: Appearance.colors.colOnSurfaceVariant
                }
            }

            // Greyed-out placeholder when there's no live status file / it's
            // gone stale (see ClaudeUsage.live) instead of showing zeros.
            StyledText {
                visible: !root.valid
                text: Translation.tr("No active session")
                color: Appearance.colors.colSubtext
            }

            ColumnLayout {
                visible: root.valid
                spacing: 4

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    StyledText {
                        Layout.fillWidth: true
                        text: Translation.tr("5h window:")
                        color: Appearance.colors.colOnSurfaceVariant
                    }
                    StyledText {
                        horizontalAlignment: Text.AlignRight
                        color: Appearance.colors.colOnSurfaceVariant
                        text: `${Math.round(root.clampedPct)}%`
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    StyledText {
                        Layout.fillWidth: true
                        text: Translation.tr("Resets in:")
                        color: Appearance.colors.colOnSurfaceVariant
                    }
                    StyledText {
                        horizontalAlignment: Text.AlignRight
                        color: Appearance.colors.colOnSurfaceVariant
                        text: root.resetCountdown(ClaudeUsage.fiveHourResetsAt)
                    }
                }

                RowLayout {
                    visible: ClaudeUsage.weeklyPct >= 0
                    Layout.fillWidth: true
                    spacing: 8
                    StyledText {
                        Layout.fillWidth: true
                        text: Translation.tr("Weekly:")
                        color: Appearance.colors.colOnSurfaceVariant
                    }
                    StyledText {
                        horizontalAlignment: Text.AlignRight
                        color: Appearance.colors.colOnSurfaceVariant
                        text: `${Math.round(ClaudeUsage.weeklyPct)}%`
                    }
                }
            }
        }
    }
}
