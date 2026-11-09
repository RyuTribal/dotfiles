import QtQuick
import QtTest
import qs.modules.topMenu
import qs.services

// The real modules/topMenu/ClaudePanel.qml inside a stand-in for StatsTab's left card, over the real
// ClaudeUsage singleton and the stand-in Backend: with long texts and a full 30-day chart, nothing in
// the panel may be wider than the card leaves it, and with every status line showing nothing may
// reach below it. The card is 410 x 416 px, measured from the live Stats tab on 2026-11-08 (the left
// card spans x 545-954 of a 1920 px screen at scale 1), and the panel sits in it with StatsTab's
// 10 px margins. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "ClaudePanel"
    when: windowShown

    readonly property double now: 1792922460000
    readonly property int minuteMs: 60 * 1000
    readonly property int dayMs: 24 * 60 * tc.minuteMs
    readonly property int cardHeight: 416
    readonly property var projectNames: ["2026-10-27-axon-cutover-3-theme-and-the-worktree-beside-it", "2026-10-26-axon-cutover-1-sweep-resources-follow-ups", "ryutribal-home-directory-without-a-project", "sections-of-the-axon-design-specification-draft", "axon"]
    property double rev: 0

    // A window of its own, since TestCase is an invisible Item and every item under it would read
    // as hidden.
    Window {
        id: host
        width: 410
        height: tc.cardHeight
        visible: true

        Rectangle {
            id: card
            anchors.fill: parent
            clip: true

            ClaudePanel {
                id: panel
                anchors.fill: parent
                anchors.margins: 10
            }
        }
    }

    function init() {
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = tc.now;
        Backend.downSince = 0;
    }

    function cleanup() {
        host.height = tc.cardHeight;
    }

    function frame(topic, status, error, frameData) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: "push",
            observed_at: tc.now - 8 * tc.minuteMs,
            error: error,
            data: frameData
        });
    }

    // The 5-hour window is current; the weekly one reset with nothing observed since, so its label
    // is the longest gauge text, "Reset at HH:MM, current unknown".
    function usageData() {
        return {
            windows: {
                five_hour: {
                    used_pct: 41.0,
                    resets_at: tc.now + 133 * tc.minuteMs,
                    state: "current",
                    source: "push",
                    observed_at: tc.now - 8 * tc.minuteMs
                },
                seven_day: {
                    used_pct: 96.0,
                    resets_at: tc.now - 5 * tc.minuteMs,
                    state: "reset_unknown",
                    source: "push",
                    observed_at: tc.now - 8 * tc.minuteMs
                }
            },
            last_session: {
                id: "0b5e2c1a-3f4d-4e8b-9a61-2d7c5f0e9b13",
                name: "Arch cozy-cyberpunk theme system, the session whose name nobody ever shortens",
                cwd: "/home/ryutribal/programming/axon",
                model: "claude-opus-5-5",
                cost_usd: 1024.5,
                context_pct: 61.0,
                observed_at: tc.now - 8 * tc.minuteMs
            }
        };
    }

    function statsData() {
        const daily = [];
        for (let day = 0; day < 30; day++) {
            const date = new Date(tc.now - (29 - day) * tc.dayMs).toISOString().slice(0, 10);
            daily.push({
                id: date,
                rev: day + 1,
                date: date,
                tokens: 1000000 * (day + 1),
                cost_usd: day + 1
            });
        }
        const projects = tc.projectNames;
        return {
            window_days: 30,
            today: {
                date: daily[29].date,
                tokens: 966500000,
                cost_usd: 325.35
            },
            daily: daily,
            top_projects: projects.map((name, rank) => ({
                        id: "/home/ryutribal/programming/" + name,
                        rev: 100 + rank,
                        name: name,
                        tokens: 900000000 - rank * 100000000,
                        cost_usd: 500 - rank * 50
                    })),
            unpriced: {
                tokens: 0,
                models: []
            },
            prices: {
                source: "litellm 0f3c2a1 (2026-10-20)",
                overrides: 0
            },
            index: {
                state: "indexing",
                files_total: 14695,
                files_done: 2351,
                skipped_lines: 0
            }
        };
    }

    // Every visible item under the panel whose right edge passes the panel's own right edge.
    function overflowing(item) {
        let found = [];
        for (const child of item.children) {
            if (!child.visible)
                continue;
            const right = child.mapToItem(panel, 0, 0).x + child.width;
            if (right > panel.width + 0.5)
                found.push(String(child) + (child.text === undefined ? "" : " \"" + child.text + "\"") + " ends at " + Math.round(right));
            found = found.concat(overflowing(child));
        }
        return found;
    }

    // Every visible item under the panel whose bottom edge passes the panel's own bottom edge.
    function reachingBelow(item) {
        let found = [];
        for (const child of item.children) {
            if (!child.visible)
                continue;
            const bottom = child.mapToItem(panel, 0, 0).y + child.height;
            if (bottom > panel.height + 0.5)
                found.push(String(child) + (child.text === undefined ? "" : " \"" + child.text + "\"") + " ends at " + Math.round(bottom));
            found = found.concat(reachingBelow(child));
        }
        return found;
    }

    // The visible texts under item whose text starts with prefix.
    function textsStartingWith(item, prefix) {
        let found = [];
        for (const child of item.children) {
            if (!child.visible)
                continue;
            if (typeof child.text === "string" && child.text.startsWith(prefix))
                found.push(child);
            found = found.concat(textsStartingWith(child, prefix));
        }
        return found;
    }

    function shownProjectRows() {
        return tc.projectNames.filter(name => textsStartingWith(panel, name).some(text => text.text === name)).length;
    }

    // Both windows current, so both gauges show a bar.
    function bothGaugesData() {
        const usage = usageData();
        usage.windows.seven_day.state = "current";
        usage.windows.seven_day.resets_at = tc.now + 5 * tc.dayMs;
        return usage;
    }

    // The tallest state the card is known to reach right after the install: a status line that
    // wraps to two lines with the age line on it, both gauge bars, and the full stats under
    // "Indexing transcripts N %".
    function showEveryStatusLine() {
        frame(ClaudeUsage.topic, "unconfigured", {
            code: "not_configured",
            message: "Probe off, waiting for a Claude Code session"
        }, bothGaugesData());
        frame(ClaudeUsage.statsTopic, "ok", null, statsData());
        compare(ClaudeUsage.statsStatus, "Indexing transcripts 15 %");
        const statusTexts = textsStartingWith(panel, "Probe off, waiting for a Claude Code session");
        compare(statusTexts.length, 1);
        compare(statusTexts[0].text, "Probe off, waiting for a Claude Code session");
        waitForItemPolished(panel);
        verify(statusTexts[0].lineCount >= 2, "the status line takes " + statusTexts[0].lineCount + " line(s)");
        checkAgeShowsWhole();
        verify(ClaudeUsage.fiveHourPct >= 0 && ClaudeUsage.weeklyPct >= 0);
    }

    // The age text after the status message is never cut: one text, not elided, as wide as its
    // text and inside the panel.
    function checkAgeShowsWhole() {
        const ageTexts = textsStartingWith(panel, "· as of 8 min ago");
        compare(ageTexts.length, 1, "the age text shows once");
        const age = ageTexts[0];
        compare(age.text, "· as of 8 min ago");
        verify(!age.truncated, "the age text is elided");
        verify(age.width >= age.implicitWidth - 0.5, "the age text is " + age.width + " px wide for " + age.implicitWidth + " px of text");
        const topLeft = age.mapToItem(panel, 0, 0);
        verify(topLeft.x >= 0 && topLeft.y >= 0 && topLeft.x + age.width <= panel.width + 0.5 && topLeft.y + age.height <= panel.height + 0.5, "the age text lies outside the panel");
    }

    function checkFits() {
        waitForItemPolished(panel);
        waitForRendering(card);
        compare(panel.width, 390);
        compare(overflowing(panel), []);
        verify(panel.implicitWidth <= panel.width, "implicitWidth " + panel.implicitWidth + " is wider than the panel's " + panel.width);
    }

    function test_longTextsStayInsideTheCard() {
        frame(ClaudeUsage.topic, "ok", null, usageData());
        frame(ClaudeUsage.statsTopic, "ok", null, statsData());
        verify(ClaudeUsage.statsTopic.hasData);
        compare(ClaudeUsage.dailyTrend.length, 30);
        compare(ClaudeUsage.topProjects.length, 5);
        checkFits();
    }

    function test_longStatusLineWrapsInsideTheCard() {
        frame(ClaudeUsage.topic, "error", {
            code: "fetch_failed",
            message: "the probe could not read the usage reply, retrying at 20:15 after the backoff the engine keeps in probe.json"
        }, usageData());
        frame(ClaudeUsage.statsTopic, "ok", null, statsData());
        verify(ClaudeUsage.statusLine !== "");
        checkFits();
    }

    // The top-project list gives way: it shows the whole rows that fit, and nothing reaches below
    // the card.
    function test_everyStatusLineStaysInsideTheCardHeight() {
        showEveryStatusLine();
        checkFits();
        compare(panel.height, tc.cardHeight - 20);
        compare(reachingBelow(panel), []);
        verify(shownProjectRows() >= 1, "no top-project row shows");
    }

    // A message of 500 characters on either status line takes two lines at most, so the gauges,
    // the chart and everything else stay inside the card.
    function test_aVeryLongStatusMessageKeepsTheSectionsInsideTheCard() {
        let message = "";
        while (message.length < 500)
            message += "the probe could not read the usage reply and retries after the backoff, ";
        message = message.slice(0, 500);
        frame(ClaudeUsage.topic, "error", {
            code: "fetch_failed",
            message: message
        }, bothGaugesData());
        frame(ClaudeUsage.statsTopic, "failed", {
            code: "engine_failed",
            message: message
        }, statsData());
        compare(ClaudeUsage.statusLine.length >= 500, true);
        compare(ClaudeUsage.statsStatus, "claude engine failed: " + message);
        checkFits();
        compare(reachingBelow(panel), []);
        for (const heading of ["5-hour window", "Weekly window", "30-day trend"])
            compare(textsStartingWith(panel, heading).length, 1, heading + " shows");
        for (const statusText of textsStartingWith(panel, "the probe could not").concat(textsStartingWith(panel, "claude engine failed")))
            verify(statusText.lineCount <= 2, "a status line takes " + statusText.lineCount + " lines");
        checkAgeShowsWhole();
    }

    // A RowLabel keeps its own width while there is room and never takes more, so the FittedText
    // beside it gets the rest of the row: the gauge titles stay whole and exactly as wide as their
    // text.
    function test_aRowLabelIsNeverWiderThanItsText() {
        frame(ClaudeUsage.topic, "ok", null, bothGaugesData());
        frame(ClaudeUsage.statsTopic, "ok", null, statsData());
        checkFits();
        for (const title of ["5-hour window", "Weekly window"]) {
            const labels = textsStartingWith(panel, title);
            compare(labels.length, 1, title + " shows");
            const label = labels[0];
            verify(!label.truncated, title + " is elided");
            verify(Math.abs(label.width - label.implicitWidth) <= 0.5, title + " is " + label.width + " px wide for " + label.implicitWidth + " px of text");
        }
    }

    // A 2.5-billion-token day shows as such, never wrapped through a 32-bit int to a negative count.
    function test_todayAbove32BitsShowsItsTokens() {
        const stats = statsData();
        stats.today.tokens = 2500000000;
        frame(ClaudeUsage.topic, "ok", null, usageData());
        frame(ClaudeUsage.statsTopic, "ok", null, stats);
        compare(textsStartingWith(panel, "2500.0M tokens").length, 1, "the day's tokens show");
    }

    // With room to spare, all five top projects show.
    function test_everyTopProjectShowsWhenTheCardHasRoom() {
        host.height = 700;
        showEveryStatusLine();
        checkFits();
        compare(reachingBelow(panel), []);
        compare(shownProjectRows(), 5);
    }
}
