import QtQuick
import QtTest
import ShellUnderTest

// The real services/ClaudeUsage.qml over the stand-in Backend of fakes/Backend.qml and the identity
// Translation of fakes/Translation.qml: the property names the widgets bind, from the `claude` and
// `claude.stats` snapshots, and the stats interest with its refresh call. backend_tests.sh copies
// ClaudeUsage.qml without its `pragma Singleton` line into ShellUnderTest as ClaudeUsageService, so
// each test makes its own instance. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "ClaudeUsage"

    Component {
        id: usageComponent
        ClaudeUsageService {}
    }

    property double rev: 0

    // Section 32's example frame: 19 s after the push, 2 h 13 min before the 5-hour reset.
    readonly property double exampleNow: 1792922460000

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = tc.exampleNow;
        Backend.downSince = 0;
        tc.rev = 0;
    }

    function exampleData() {
        return {
            windows: {
                five_hour: {
                    used_pct: 42.0,
                    resets_at: 1792930440000,
                    state: "current",
                    source: "push",
                    observed_at: 1792922441000
                },
                seven_day: {
                    used_pct: 23.0,
                    resets_at: 1793379600000,
                    state: "current",
                    source: "push",
                    observed_at: 1792922441000
                }
            },
            last_session: {
                id: "0b5e2c1a-3f4d-4e8b-9a61-2d7c5f0e9b13",
                name: "axon design",
                cwd: "/home/ryutribal/programming/axon",
                model: "claude-opus-5-5",
                cost_usd: 18.42,
                context_pct: 61.0,
                observed_at: 1792922441000
            },
            push: {
                last_at: 1792922441000,
                error: null
            },
            probe: {
                state: "idle",
                cc_version: "2.1.310",
                last_ok_at: 1792921927000,
                next_at: 1792922827000,
                retry_at: null,
                over_budget: false,
                error: null
            }
        };
    }

    function statsData() {
        return {
            window_days: 30,
            today: {
                date: "2026-10-25",
                tokens: 41268012,
                cost_usd: 38.17
            },
            daily: [
                {
                    id: "2026-09-26",
                    rev: 1,
                    date: "2026-09-26",
                    tokens: 12750331,
                    cost_usd: 14.02
                },
                {
                    id: "2026-09-27",
                    rev: 2,
                    date: "2026-09-27",
                    tokens: 30117842,
                    cost_usd: 27.65
                }
            ],
            top_projects: [
                {
                    id: "/home/ryutribal/programming/engrain",
                    rev: 3,
                    name: "engrain",
                    tokens: 612004113,
                    cost_usd: 512.4
                },
                {
                    id: "/home/ryutribal/programming/my-axon",
                    rev: 4,
                    name: "my-axon",
                    tokens: 98113004,
                    cost_usd: 91.2
                }
            ],
            unpriced: {
                tokens: 0,
                models: []
            },
            prices: {
                source: "litellm 0f3c2a1 (2026-10-20)",
                overrides: 0
            },
            index: {
                state: "ready",
                files_total: 14695,
                files_done: 14695,
                skipped_lines: 0
            }
        };
    }

    function frame(topic, status, frameData) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: "push",
            observed_at: 1792922441000,
            data: frameData
        });
    }

    function test_parityNamesFromSnapshot() {
        const usage = createTemporaryObject(usageComponent, tc);
        frame(usage.topic, "ok", exampleData());
        compare(usage.fiveHourPct, 42);
        compare(usage.fiveHourResetsAt, 1792930440000);
        verify(!usage.fiveHourUnknown);
        compare(usage.weeklyPct, 23);
        compare(usage.weeklyResetsAt, 1793379600000);
        compare(usage.sessionCostUsd, 18.42);
        compare(usage.contextPct, 61);
        compare(usage.sessionLabel, "axon design");
        compare(usage.status, "ok");
        compare(usage.observedAt, 1792922441000);
        compare(usage.knownAt, 1792922441000);
        compare(usage.statusLine, "");
    }

    function test_sessionLabelFallsBackToCwdBasename() {
        const usage = createTemporaryObject(usageComponent, tc);
        const data = exampleData();
        data.last_session.name = null;
        frame(usage.topic, "ok", data);
        compare(usage.sessionLabel, "axon");
    }

    function test_nullWindowIsMinusOne() {
        const usage = createTemporaryObject(usageComponent, tc);
        const data = exampleData();
        data.windows.five_hour = null;
        data.last_session = null;
        frame(usage.topic, "ok", data);
        compare(usage.fiveHourPct, -1);
        compare(usage.fiveHourResetsAt, -1);
        compare(usage.weeklyPct, 23);
        compare(usage.contextPct, -1);
        compare(usage.sessionCostUsd, -1);
        compare(usage.sessionLabel, "");
    }

    // A session that did not report its cost is -1, as contextPct, so the panel shows "--".
    function test_sessionWithoutCostIsMinusOne() {
        const usage = createTemporaryObject(usageComponent, tc);
        const data = exampleData();
        data.last_session.cost_usd = null;
        frame(usage.topic, "ok", data);
        compare(usage.sessionCostUsd, -1);
        compare(usage.contextPct, 61);
    }

    // A day's tokens pass 2^31 on a heavy day; the count is not a 32-bit int.
    function test_todayTokensAbove32Bits() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.pollingEnabled = true;
        const stats = statsData();
        stats.today.tokens = 2500000000;
        frame(usage.statsTopic, "ok", stats);
        compare(usage.todayTokens, 2500000000);
    }

    function test_noDataBeforeTheFirstFrame() {
        const usage = createTemporaryObject(usageComponent, tc);
        compare(usage.fiveHourPct, -1);
        compare(usage.knownAt, null);
        compare(usage.statusLine, "Waiting for the first reading");
    }

    function test_resetPassedFlipsOnBackendNow() {
        const usage = createTemporaryObject(usageComponent, tc);
        frame(usage.topic, "ok", exampleData());
        compare(usage.fiveHourPct, 42);
        Backend.now = 1792930440000 + 1000;
        verify(usage.fiveHourUnknown);
        compare(usage.fiveHourPct, -1);
    }

    function test_statsMapping() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.pollingEnabled = true;
        frame(usage.statsTopic, "ok", statsData());
        compare(usage.todayTokens, 41268012);
        compare(usage.todayCostUsd, 38.17);
        compare(usage.dailyTrend.length, 2);
        compare(usage.dailyTrend[1].date, "2026-09-27");
        compare(usage.dailyTrend[1].tokens, 30117842);
        compare(usage.dailyTrend[1].cost, 27.65);
        compare(usage.topProjects.length, 2);
        compare(usage.topProjects[1].name, "my-axon");
        compare(usage.topProjects[1].tokens, 98113004);
        compare(usage.topProjects[1].cost, 91.2);
        compare(usage.statsStatus, "");
    }

    // Before the first claude.stats frame the block says so instead of showing zeros.
    function test_noStatsFrameSaysWaiting() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.pollingEnabled = true;
        verify(!usage.statsTopic.hasData);
        compare(usage.statsStatus, "Waiting for the first reading");
        frame(usage.statsTopic, "ok", statsData());
        verify(usage.statsTopic.hasData);
        compare(usage.statsStatus, "");
    }

    function test_statsStatusWhileIndexing() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.pollingEnabled = true;
        const data = statsData();
        data.index = {
            state: "indexing",
            files_total: 14695,
            files_done: 6000,
            skipped_lines: 0
        };
        frame(usage.statsTopic, "starting", data);
        compare(usage.statsStatus, "Indexing transcripts 40 %");
    }

    function test_pollingEnabledActivatesStatsAndRefreshes() {
        const usage = createTemporaryObject(usageComponent, tc);
        compare(Backend.attachments, ["+claude"]);
        compare(Backend.calls.length, 0);
        usage.pollingEnabled = true;
        compare(Backend.attachments, ["+claude", "+claude.stats"]);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "claude.refresh");
        compare(Backend.calls[0].args, {
            if_older_than_ms: 60000
        });
    }

    function test_pollingDisabledDetachesStats() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.pollingEnabled = true;
        usage.pollingEnabled = false;
        compare(Backend.attachments, ["+claude", "+claude.stats", "-claude.stats"]);
        compare(Backend.calls.length, 1);
    }

    function test_refreshIfOlderSendsTheCall() {
        const usage = createTemporaryObject(usageComponent, tc);
        usage.refreshIfOlder(5000);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "claude.refresh");
        compare(Backend.calls[0].args, {
            if_older_than_ms: 5000
        });
    }

    function test_formatCountdown() {
        const usage = createTemporaryObject(usageComponent, tc);
        compare(usage.formatCountdown(2 * 3600000 + 13 * 60000), "2h 13m");
    }
}
