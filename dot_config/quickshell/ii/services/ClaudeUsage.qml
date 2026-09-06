pragma Singleton
pragma ComponentBehavior: Bound

import QtQuick
import Quickshell
import Quickshell.Io

/**
 * Exposes live Claude Code rate-limit/usage metrics read from the status
 * JSON written by ~/.config/claude-widget/statusline-tee.sh (fed by the
 * Claude Code statusline hook on every turn).
 */
Singleton {
    id: root
    property real fiveHourPct: -1
    property real fiveHourResetsAt: -1
    property real weeklyPct: -1
    property real weeklyResetsAt: -1
    property real sessionCostUsd: 0
    property real contextPct: -1
    property bool live: false

    // ccusage-derived usage stats (Task 7)
    property int todayTokens: 0
    property real todayCostUsd: 0
    property var dailyTrend: []
    property var topProjects: []
    property bool ccusageOk: false
    property bool pollingEnabled: false // StatsTab sets true while visible

    property string statusPath: Quickshell.env("HOME") + "/.cache/claude-widget/status.json"

    function parseStatus(text) {
        try {
            const j = JSON.parse(text);
            root.fiveHourPct = j.rate_limits?.five_hour?.used_percentage ?? -1;
            root.fiveHourResetsAt = j.rate_limits?.five_hour?.resets_at ?? -1;
            root.weeklyPct = j.rate_limits?.seven_day?.used_percentage ?? -1;
            root.weeklyResetsAt = j.rate_limits?.seven_day?.resets_at ?? -1;
            root.sessionCostUsd = j.cost?.total_cost_usd ?? 0;
            root.contextPct = j.context_window?.used_percentage ?? -1;
        } catch (e) {
            console.log("[ClaudeUsage] Failed to parse status.json: " + e);
            // Parse failure means we can't trust the file's current content
            // (e.g. a read raced a mid-write from the tee script) even if its
            // mtime is fresh, so mark stale and skip the mtime check for this
            // cycle. The next 60s timer tick or a successful reload recovers.
            root.live = false;
            return;
        }
        mtimeCheck.running = true;
    }

    function refreshCcusage() {
        dailyProc.running = true;
        sessionProc.running = true;
    }

    // ccusage's project directories are named after the project's absolute
    // path with every "/" swapped for "-" (the same convention Claude Code
    // itself uses under ~/.claude/projects/), so this is a best-effort
    // reversal to get a readable label. It's ambiguous when a real directory
    // name contains a literal "-" (e.g. "menu-restructure" collapses into
    // separate segments), but that only affects the display name, not the
    // aggregated totals.
    function projectDisplayName(projectPath) {
        const parts = (projectPath ?? "").split("-").filter(p => p.length > 0);
        return parts.length ? parts[parts.length - 1] : (projectPath || "unknown");
    }

    // Sums tokens/cost per project across all sessions, sorted desc, top 5.
    function aggregateProjects(j) {
        const sessions = j.sessions ?? [];
        const byProject = {};
        for (const s of sessions) {
            const key = s.projectPath ?? "unknown";
            if (!byProject[key]) {
                byProject[key] = { tokens: 0, cost: 0 };
            }
            byProject[key].tokens += s.totalTokens ?? 0;
            byProject[key].cost += s.totalCost ?? 0;
        }
        return Object.keys(byProject).map(key => ({
            name: root.projectDisplayName(key),
            tokens: byProject[key].tokens,
            cost: byProject[key].cost
        })).sort((a, b) => b.tokens - a.tokens).slice(0, 5);
    }

    FileView {
        id: statusFile
        path: root.statusPath
        watchChanges: true
        onFileChanged: reload()
        onLoaded: root.parseStatus(statusFile.text())
        onLoadFailed: (error) => {
            console.log("[ClaudeUsage] status.json not available yet: " + error);
            root.live = false;
        }
    }

    // Freshness: re-evaluate every minute
    Timer {
        interval: 60000
        running: true
        repeat: true
        onTriggered: statusFile.reload()
    }

    // FileView has no mtime accessor, so shell out to stat to determine
    // staleness: live is only true when the file was written < 10 min ago.
    Process {
        id: mtimeCheck
        command: ["stat", "-c", "%Y", root.statusPath]
        stdout: StdioCollector {
            id: mtimeCollector
            onStreamFinished: {
                const mtime = parseInt(mtimeCollector.text);
                root.live = !isNaN(mtime) && (Date.now() / 1000 - mtime) < 600;
            }
        }
    }

    // ccusage polling: only runs while pollingEnabled (StatsTab visible with
    // the top menu open) so no ccusage process is spawned in the background
    // otherwise.
    Timer {
        interval: 120000
        running: root.pollingEnabled
        repeat: true
        triggeredOnStart: true
        onTriggered: root.refreshCcusage()
    }

    // Claude-specific daily totals (ccusage's unified `daily` command mixes
    // in other agents like Codex and keys rows by "period" instead of
    // "date" — `ccusage claude daily` keeps this scoped to Claude Code and
    // matches the field names used below).
    Process {
        id: dailyProc
        command: ["bunx", "ccusage", "claude", "daily", "--json"]
        stdout: StdioCollector {
            id: dailyCollector
            onStreamFinished: {
                try {
                    const j = JSON.parse(dailyCollector.text);
                    const days = j.daily ?? [];
                    root.dailyTrend = days.slice(-30).map(d => ({
                        date: d.date, tokens: d.totalTokens, cost: d.totalCost
                    }));
                    // Use the local calendar date (not toISOString's UTC
                    // date) so "today" matches ccusage's own default
                    // system-timezone day grouping.
                    const now = new Date();
                    const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
                    const t = days.find(d => d.date === today);
                    root.todayTokens = t ? t.totalTokens : 0;
                    root.todayCostUsd = t ? t.totalCost : 0;
                    root.ccusageOk = true;
                } catch (e) {
                    console.log("[ClaudeUsage] Failed to parse ccusage daily output: " + e);
                    root.ccusageOk = false;
                }
            }
        }
    }

    // Per-project (session) breakdown for the "top projects" list. On any
    // failure (parse error, or the process itself exiting non-zero/failing
    // to launch), topProjects is cleared rather than left holding a stale
    // list from a previous successful cycle — otherwise the "Top projects"
    // section could look live while silently showing outdated data (or
    // stay populated forever after the very first failure if there was
    // never a successful run).
    Process {
        id: sessionProc
        command: ["bunx", "ccusage", "claude", "session", "--json"]
        stdout: StdioCollector {
            id: sessionCollector
            onStreamFinished: {
                try {
                    const j = JSON.parse(sessionCollector.text);
                    root.topProjects = root.aggregateProjects(j);
                } catch (e) {
                    console.log("[ClaudeUsage] Failed to parse ccusage session output: " + e);
                    root.topProjects = [];
                }
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0) {
                console.log("[ClaudeUsage] ccusage session command exited with code " + exitCode);
                root.topProjects = [];
            }
        }
    }
}
