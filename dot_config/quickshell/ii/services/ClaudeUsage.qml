pragma Singleton
pragma ComponentBehavior: Bound

import "claude.js" as Claude
import qs
import QtQuick
import Quickshell

/**
 * Claude plan usage for the bar's ClaudeIndicator and the top menu's ClaudePanel, a thin client of
 * axon's claude engine (section 32): the 5-hour and weekly windows and the last session from the
 * `claude` topic, always subscribed, and the token and cost stats from `claude.stats` while
 * pollingEnabled. It keeps the property names the widgets bound before axon. Countdowns and the
 * on-time reset flip read Backend.now.
 */
Singleton {
    id: root

    property bool pollingEnabled: false // StatsTab sets true while visible

    readonly property BackendTopic topic: BackendTopic {
        name: "claude"
        onSnapshot: data => root._onUsage(data)
    }
    readonly property BackendTopic statsTopic: BackendTopic {
        name: "claude.stats"
        active: root.pollingEnabled
        onSnapshot: data => root._onStats(data)
    }

    readonly property real fiveHourPct: _fiveHour.pct // -1 when not known
    readonly property real fiveHourResetsAt: _fiveHour.resetsAt // ms, -1 without a window
    readonly property bool fiveHourUnknown: _fiveHour.unknown
    readonly property var fiveHourWindow: _windows.five_hour ?? null // the wire window, for claude.js resetText
    readonly property var weeklyWindow: _windows.seven_day ?? null
    readonly property real weeklyPct: _weekly.pct
    readonly property real weeklyResetsAt: _weekly.resetsAt
    readonly property bool weeklyUnknown: _weekly.unknown
    readonly property real sessionCostUsd: _session?.cost_usd ?? -1 // -1 when the session reported none
    readonly property real contextPct: _session?.context_pct ?? -1
    readonly property string sessionLabel: _session === null ? "" : _session.name || Claude.basename(_session.cwd ?? "")

    property real todayTokens: 0 // a heavy day passes a 32-bit int's 2^31
    property real todayCostUsd: 0
    property var dailyTrend: [] // [{date, tokens, cost}], oldest first
    property var topProjects: [] // [{name, tokens, cost}]

    readonly property string status: topic.status
    readonly property double observedAt: topic.observedAt
    readonly property var knownAt: topic.hasData ? topic.knownAt : null // ms of the newest datum
    readonly property string statusLine: Claude.statusLine(topic.status, topic.errorCode, topic.errorMessage, topic.hasData, knownAt, Claude.zoneTime, Translation.tr)
    readonly property string statsStatus: Claude.statsStatus(statsTopic.status, statsTopic.hasData, _index, statsTopic.errorMessage, Translation.tr)

    property var _windows: ({})
    property var _session: null
    property var _index: null
    readonly property var _fiveHour: Claude.windowView(fiveHourWindow, Backend.now)
    readonly property var _weekly: Claude.windowView(weeklyWindow, Backend.now)

    // Asks the engine for a probe unless its newest datum is younger than `ms` (section 32, probe
    // trigger 3).
    function refreshIfOlder(ms) {
        Backend.call("claude.refresh", {
            if_older_than_ms: ms
        });
    }

    function formatCountdown(ms) {
        return Claude.formatCountdown(ms, Translation.tr);
    }

    function _onUsage(data) {
        _windows = data.windows ?? {};
        _session = data.last_session ?? null;
    }

    function _onStats(data) {
        todayTokens = data.today?.tokens ?? 0;
        todayCostUsd = data.today?.cost_usd ?? 0;
        dailyTrend = (data.daily ?? []).map(day => ({
                    date: day.date,
                    tokens: day.tokens,
                    cost: day.cost_usd
                }));
        topProjects = (data.top_projects ?? []).map(project => ({
                    name: project.name,
                    tokens: project.tokens,
                    cost: project.cost_usd
                }));
        _index = data.index ?? null;
    }

    onPollingEnabledChanged: {
        if (pollingEnabled)
            refreshIfOlder(60000);
    }
}
