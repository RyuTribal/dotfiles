import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts

// Claude Code usage panel: live rate-limit gauges (from the statusline hook,
// see ClaudeUsage.qml) plus ccusage-derived token/cost trends. Lives inside
// StatsTab's left half.
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

    function formatCost(n) {
        return "$" + n.toFixed(2);
    }

    // Local clock tick so the reset countdowns keep counting down without
    // needing ClaudeUsage itself to re-emit anything.
    property double nowMs: Date.now()
    Timer {
        interval: 30000
        running: true
        repeat: true
        onTriggered: root.nowMs = Date.now()
    }

    function resetCountdown(resetsAt) {
        if (resetsAt <= 0)
            return "";
        const mins = Math.max(0, Math.round((resetsAt * 1000 - root.nowMs) / 60000));
        return Translation.tr("resets in %1m").arg(mins);
    }

    // Section 1: 5-hour rate-limit window
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 4
        opacity: ClaudeUsage.live ? 1 : 0.5

        RowLayout {
            Layout.fillWidth: true
            StyledText {
                Layout.fillWidth: true
                text: Translation.tr("5-hour window")
                font.weight: Font.Medium
            }
            StyledText {
                color: Appearance.colors.colSubtext
                text: ClaudeUsage.live ? root.resetCountdown(ClaudeUsage.fiveHourResetsAt) : Translation.tr("no active session")
            }
        }
        StyledProgressBar {
            Layout.fillWidth: true
            value: Math.max(0, ClaudeUsage.fiveHourPct) / 100
        }
    }

    // Section 2: weekly rate-limit window (hidden when unavailable)
    ColumnLayout {
        Layout.fillWidth: true
        spacing: 4
        visible: ClaudeUsage.weeklyPct !== -1
        opacity: ClaudeUsage.live ? 1 : 0.5

        RowLayout {
            Layout.fillWidth: true
            StyledText {
                Layout.fillWidth: true
                text: Translation.tr("Weekly window")
                font.weight: Font.Medium
            }
            StyledText {
                color: Appearance.colors.colSubtext
                text: ClaudeUsage.live ? root.resetCountdown(ClaudeUsage.weeklyResetsAt) : Translation.tr("no active session")
            }
        }
        StyledProgressBar {
            Layout.fillWidth: true
            value: Math.max(0, ClaudeUsage.weeklyPct) / 100
        }
    }

    // Divider
    Rectangle {
        Layout.fillWidth: true
        height: 1
        color: Appearance.colors.colOutlineVariant
        opacity: 0.3
    }

    // Section 3-5: ccusage-derived stats, replaced with a hint when
    // ccusage isn't available (offline, not installed, etc.)
    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        spacing: 10
        visible: ClaudeUsage.ccusageOk

        // Section 3: today's usage
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            StyledText {
                text: Translation.tr("Today")
                font.weight: Font.Medium
            }
            RowLayout {
                spacing: 12
                StyledText {
                    text: root.formatTokens(ClaudeUsage.todayTokens) + " " + Translation.tr("tokens")
                }
                StyledText {
                    color: Appearance.colors.colSubtext
                    text: root.formatCost(ClaudeUsage.todayCostUsd) + " " + Translation.tr("(API-equivalent)")
                }
            }
            // Current session cost and context-window usage, from the same
            // live status.json as the rate-limit gauges above (greyed out
            // together with them when stale).
            StyledText {
                opacity: ClaudeUsage.live ? 1 : 0.5
                color: Appearance.colors.colSubtext
                text: Translation.tr("session %1 · context %2%").arg(root.formatCost(ClaudeUsage.sessionCostUsd)).arg(Math.max(0, ClaudeUsage.contextPct))
            }
        }

        // Section 4: 30-day trend mini bar-chart
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            StyledText {
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

        // Section 5: top projects
        ColumnLayout {
            Layout.fillWidth: true
            spacing: 2
            StyledText {
                text: Translation.tr("Top projects")
                font.weight: Font.Medium
            }
            Repeater {
                model: ClaudeUsage.topProjects
                delegate: RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    StyledText {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        text: modelData.name
                    }
                    StyledText {
                        color: Appearance.colors.colSubtext
                        text: root.formatTokens(modelData.tokens)
                    }
                }
            }
        }
    }

    // Fallback hint when ccusage failed or isn't installed
    ColumnLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        visible: !ClaudeUsage.ccusageOk
        spacing: 4

        StyledText {
            Layout.fillWidth: true
            wrapMode: Text.WordWrap
            color: Appearance.colors.colSubtext
            text: Translation.tr("ccusage unavailable — install: npm i -g ccusage (or keep bunx)")
        }
    }

    Item { Layout.fillHeight: true }
}
