import "../../services/claude.js" as Claude
import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts

// Claude Code usage panel: the plan's rate-limit gauges and the token/cost
// stats axon's claude engine keeps (see ClaudeUsage.qml). Lives inside
// StatsTab's left half. The gauges and the session line dim whenever the
// `claude` topic is not live; countdowns read Backend.now.
//
// StatsTab's card has a fixed width and clips, so no text here may set the
// card's width. A layout gives an item without Layout.fillWidth exactly its
// text's width and passes that up as every enclosing layout's minimum, which
// once laid the whole panel out wider than the card. Every text is a
// FittedText, or a RowLabel beside one.
//
// FittedText and RowLabel are files of their own, not inline components: a
// running shell's hot reload fails with "<Name> is not a type" when a file
// uses an inline component that the file's running version did not declare.
//
// The card's height is fixed too. The stats block gets only the height the
// status lines and gauges above it leave, never its content's, and the
// top-project list shows as many whole rows as fit in it.
ColumnLayout {
    id: root
    spacing: 12

    function formatTokens(n) {
        if (n >= 1000000)
            return (n / 1000000).toFixed(1) + "M";
        if (n >= 1000)
            return (n / 1000).toFixed(1) + "K";
        return String(n);
    }

    readonly property real liveOpacity: ClaudeUsage.topic.live ? 1 : 0.5

    readonly property bool statsKnown: ClaudeUsage.statsTopic.hasData

    function gaugeLabel(window) {
        return Claude.gaugeLabel(window, Backend.now, Claude.zoneTime, Translation.tr);
    }

    // What the `claude` topic's status says, then how old the numbers are.
    // The message takes two lines at most, the rest elided, so no message or
    // translation can push the gauges and stats down. The age is never cut:
    // it keeps its own width (a few words, "as of 12 min ago", the one text
    // here allowed to) and the message gets what is left.
    RowLayout {
        id: statusRow
        Layout.fillWidth: true
        visible: statusMessage.text !== "" || statusRow.age !== ""
        spacing: 4

        readonly property string age: Claude.ageLine(ClaudeUsage.topic.ageMs, Backend.now, Translation.tr)

        FittedText {
            id: statusMessage
            Layout.alignment: Qt.AlignTop
            visible: statusMessage.text !== ""
            wrapMode: Text.WordWrap
            maximumLineCount: 2
            color: Appearance.colors.colSubtext
            text: ClaudeUsage.statusLine
        }
        StyledText {
            Layout.alignment: Qt.AlignTop
            visible: statusRow.age !== ""
            color: Appearance.colors.colSubtext
            text: statusMessage.text === "" ? statusRow.age : "· " + statusRow.age
        }
    }

    // Section 1: 5-hour rate-limit window
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 4
        opacity: root.liveOpacity

        RowLayout {
            Layout.fillWidth: true
            RowLabel {
                text: Translation.tr("5-hour window")
                font.weight: Font.Medium
            }
            FittedText {
                hAlignment: Text.AlignRight
                color: Appearance.colors.colSubtext
                text: root.gaugeLabel(ClaudeUsage.fiveHourWindow)
            }
        }
        // No bar while the percentage is not known, never an empty one.
        StyledProgressBar {
            Layout.fillWidth: true
            visible: ClaudeUsage.fiveHourPct >= 0
            value: ClaudeUsage.fiveHourPct / 100
        }
    }

    // Section 2: weekly rate-limit window (hidden when unavailable)
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 4
        visible: ClaudeUsage.weeklyWindow !== null
        opacity: root.liveOpacity

        RowLayout {
            Layout.fillWidth: true
            RowLabel {
                text: Translation.tr("Weekly window")
                font.weight: Font.Medium
            }
            FittedText {
                hAlignment: Text.AlignRight
                color: Appearance.colors.colSubtext
                text: root.gaugeLabel(ClaudeUsage.weeklyWindow)
            }
        }
        StyledProgressBar {
            Layout.fillWidth: true
            visible: ClaudeUsage.weeklyPct >= 0
            value: ClaudeUsage.weeklyPct / 100
        }
    }

    // Divider
    Rectangle {
        Layout.fillWidth: true
        height: 1
        color: Appearance.colors.colOutlineVariant
        opacity: 0.3
    }

    // Section 3-5: the claude.stats numbers, under their status line while
    // the transcripts are indexed or the stats have an error. The numbers
    // stay hidden until the first stats frame, so no zeros show as values.
    // Its height is what the sections above leave (preferred and minimum 0).
    ColumnLayout {
        id: statsBlock
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.preferredHeight: 0
        Layout.minimumHeight: 0
        spacing: 10

        // Two lines at most, like the status line above.
        FittedText {
            visible: text !== ""
            wrapMode: Text.WordWrap
            maximumLineCount: 2
            color: Appearance.colors.colSubtext
            text: ClaudeUsage.statsStatus
        }

        // Section 3: today's usage
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            visible: root.statsKnown || ClaudeUsage.sessionLabel !== ""
            FittedText {
                text: Translation.tr("Today")
                font.weight: Font.Medium
            }
            RowLayout {
                Layout.fillWidth: true
                visible: root.statsKnown
                spacing: 12
                RowLabel {
                    text: root.formatTokens(ClaudeUsage.todayTokens) + " " + Translation.tr("tokens")
                }
                FittedText {
                    color: Appearance.colors.colSubtext
                    text: Claude.formatCost(ClaudeUsage.todayCostUsd) + " " + Translation.tr("(API-equivalent)")
                }
            }
            // The last session's name, cost and context-window usage, from the
            // same `claude` topic as the gauges above (dimmed with them).
            FittedText {
                visible: ClaudeUsage.sessionLabel !== ""
                opacity: root.liveOpacity
                color: Appearance.colors.colSubtext
                text: Claude.sessionLine(ClaudeUsage.sessionLabel, ClaudeUsage.sessionCostUsd, ClaudeUsage.contextPct, Translation.tr)
            }
        }

        // Section 4: 30-day trend mini bar-chart
        ColumnLayout {
            id: trendSection
            Layout.fillWidth: true
            visible: root.statsKnown
            spacing: 2
            FittedText {
                text: Translation.tr("30-day trend")
                font.weight: Font.Medium
            }
            RowLayout {
                id: trendRow
                Layout.fillWidth: true
                Layout.preferredHeight: 40
                spacing: 2

                property real maxTokens: ClaudeUsage.dailyTrend.reduce((m, d) => Math.max(m, d.tokens), 1)

                Repeater {
                    model: ClaudeUsage.dailyTrend
                    delegate: Rectangle {
                        required property var modelData
                        Layout.fillWidth: true
                        Layout.alignment: Qt.AlignBottom
                        Layout.preferredHeight: Math.max(2, 40 * (modelData.tokens / Math.max(1, trendRow.maxTokens)))
                        color: Appearance.colors.colPrimary
                        radius: 1
                    }
                }
            }
        }

        // Section 5: top projects, the whole rows that fit below the chart
        // (the section hides when not one does). Every row has the same
        // height, so the count follows from the first row's.
        ColumnLayout {
            id: topSection
            Layout.fillWidth: true
            visible: root.statsKnown && topSection.rowsThatFit > 0
            spacing: 2

            readonly property real rowHeight: projectRows.count > 0 && projectRows.itemAt(0) ? projectRows.itemAt(0).implicitHeight : 0
            // Where the first row starts, in statsBlock: below the chart,
            // the gap and the heading.
            readonly property real rowsTop: trendSection.y + trendSection.height + statsBlock.spacing + topHeading.implicitHeight + topSection.spacing
            // Where the last row must end: above the gap before the spacer
            // at the bottom of statsBlock.
            readonly property real rowsBottom: statsBlock.height - statsBlock.spacing
            readonly property int rowsThatFit: topSection.rowHeight > 0 ? Math.max(0, Math.floor((topSection.rowsBottom - topSection.rowsTop + topSection.spacing) / (topSection.rowHeight + topSection.spacing))) : 0

            FittedText {
                id: topHeading
                text: Translation.tr("Top projects")
                font.weight: Font.Medium
            }
            Repeater {
                id: projectRows
                model: ClaudeUsage.topProjects
                delegate: RowLayout {
                    id: projectRow
                    required property var modelData
                    required property int index
                    Layout.fillWidth: true
                    visible: projectRow.index < topSection.rowsThatFit
                    FittedText {
                        text: modelData.name
                    }
                    RowLabel {
                        color: Appearance.colors.colSubtext
                        text: root.formatTokens(modelData.tokens)
                    }
                }
            }
        }

        // Keeps the sections at the top of the block.
        Item {
            Layout.fillHeight: true
        }
    }
}
