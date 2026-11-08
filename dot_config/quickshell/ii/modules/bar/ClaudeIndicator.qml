import "../../services/claude.js" as Claude
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts

// Bar icon + label for the Claude Code 5-hour rate-limit window (see
// services/ClaudeUsage.qml). Click jumps to the top menu's Stats tab, same
// as the Resources area. Hovering shows a StyledPopup preview, the same
// component ResourcesPopup/BatteryPopup use, as the bar's hover-preview idiom.
// The one exception is the value's own StyledToolTip, which section 20's
// stage 1 minimum asks for: while the topic is not live it says how old the
// number is and why it is dimmed. ClaudeUsage's `claude` topic is
// always subscribed, so this widget adds no polling of its own; opening the
// popup asks axon for a probe when its newest reading is over a minute old.
// Countdowns and ages read Backend.now.
MouseArea {
    id: root
    implicitWidth: rowLayout.implicitWidth
    implicitHeight: Appearance.sizes.barHeight
    hoverEnabled: true
    opacity: ClaudeUsage.topic.live ? 1 : 0.5

    readonly property bool valid: ClaudeUsage.fiveHourPct >= 0 && !ClaudeUsage.fiveHourUnknown
    readonly property real clampedPct: Math.max(0, Math.min(100, ClaudeUsage.fiveHourPct))
    readonly property string ageLine: Claude.ageLine(ClaudeUsage.topic.ageMs, Backend.now, Translation.tr)

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

            StyledToolTip {
                content: [root.ageLine, ClaudeUsage.statusLine].filter(line => line !== "").join("\n")
                extraVisibleCondition: root.containsMouse && !ClaudeUsage.topic.live && content !== ""
            }
        }
    }

    StyledPopup {
        hoverTarget: root
        onActiveChanged: {
            if (active)
                ClaudeUsage.refreshIfOlder(60000);
        }

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

            // What the topic's status says ("Waiting for the first reading"
            // before any data), hidden while there is nothing to say.
            StyledText {
                visible: text !== ""
                text: ClaudeUsage.statusLine
                color: Appearance.colors.colSubtext
            }

            ColumnLayout {
                visible: ClaudeUsage.topic.hasData
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
                        text: root.valid ? `${Math.round(root.clampedPct)}%` : "--"
                    }
                }

                StyledText {
                    visible: text !== ""
                    color: Appearance.colors.colOnSurfaceVariant
                    text: Claude.resetText(ClaudeUsage.fiveHourWindow, Backend.now, Claude.zoneTime, Translation.tr)
                }

                RowLayout {
                    visible: ClaudeUsage.weeklyPct >= 0 || ClaudeUsage.weeklyUnknown
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
                        text: ClaudeUsage.weeklyUnknown ? "--" : `${Math.round(ClaudeUsage.weeklyPct)}%`
                    }
                }

                StyledText {
                    visible: text !== ""
                    color: Appearance.colors.colSubtext
                    text: root.ageLine
                }
            }
        }
    }
}
