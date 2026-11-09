import QtQuick
import QtTest
import "../../services/claude.js" as Claude

// The pure parts of the claude views (services/claude.js). Texts go through an identity tr, and
// times are built and read in the local zone, so the results hold in any zone. Needs nothing else:
// QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/unit/tst_claude.qml
TestCase {
    name: "Claude"

    readonly property double minuteMs: 60 * 1000
    readonly property double hourMs: 60 * minuteMs
    readonly property double dayMs: 24 * hourMs
    // 13:14 local on Sunday 2026-10-25, the reset time of section 32's example.
    readonly property double resetAt: new Date(2026, 9, 25, 13, 14).getTime()

    function tr(text) {
        return text;
    }

    function localTime(ms) {
        const date = new Date(ms);
        return String(date.getHours()).padStart(2, "0") + ":" + String(date.getMinutes()).padStart(2, "0");
    }

    function currentWindow(usedPct, observedAt) {
        return {
            used_pct: usedPct,
            resets_at: resetAt,
            state: "current",
            source: "push",
            observed_at: observedAt
        };
    }

    function test_formatCountdown() {
        compare(Claude.formatCountdown(45 * minuteMs, tr), "45m");
        compare(Claude.formatCountdown(2 * hourMs + 13 * minuteMs + 59 * 1000, tr), "2h 13m");
        compare(Claude.formatCountdown(6 * dayMs + 7 * hourMs + 30 * minuteMs, tr), "6d 7h");
        compare(Claude.formatCountdown(minuteMs, tr), "1m");
        compare(Claude.formatCountdown(minuteMs - 1, tr), "<1m");
        compare(Claude.formatCountdown(0, tr), "<1m");
        compare(Claude.formatCountdown(-1, tr), "<1m");
    }

    function test_windowViewResetUnknownFromState() {
        const window = {
            used_pct: null,
            resets_at: resetAt,
            state: "reset_unknown",
            source: "probe",
            observed_at: resetAt - hourMs
        };
        const view = Claude.windowView(window, resetAt + minuteMs);
        verify(view.unknown);
        compare(view.pct, -1);
        compare(view.resetsAt, resetAt);
    }

    function test_windowViewCurrent() {
        const view = Claude.windowView(currentWindow(42, resetAt - hourMs), resetAt - minuteMs);
        verify(!view.unknown);
        compare(view.pct, 42);
        compare(view.resetsAt, resetAt);
    }

    function test_windowViewNull() {
        const view = Claude.windowView(null, resetAt);
        verify(!view.unknown);
        compare(view.pct, -1);
        compare(view.resetsAt, -1);
    }

    // D7: the engine has not flipped the window yet (axon down), but its reset passed with no datum
    // observed after it.
    function test_windowViewResetPassedLocally() {
        const view = Claude.windowView(currentWindow(42, resetAt - hourMs), resetAt + 1);
        verify(view.unknown);
        compare(view.pct, -1);
        compare(view.resetsAt, resetAt);
    }

    function test_windowViewObservedAfterResetStaysKnown() {
        const view = Claude.windowView(currentWindow(3, resetAt + 5), resetAt + 10);
        verify(!view.unknown);
        compare(view.pct, 3);
    }

    function test_resetTextUnknown() {
        const window = currentWindow(42, resetAt - hourMs);
        compare(Claude.resetText(window, resetAt + minuteMs, localTime, tr), "Reset at 13:14, current unknown");
    }

    function test_resetTextCountdown() {
        const window = currentWindow(42, resetAt - hourMs);
        const now = resetAt - (2 * hourMs + 13 * minuteMs);
        compare(Claude.resetText(window, now, localTime, tr), "resets in 2h 13m");
        compare(Claude.resetText(null, now, localTime, tr), "");
    }

    function test_ageLineHiddenUnderTwoMinutes() {
        const now = resetAt;
        compare(Claude.ageLine(0, now, tr), "");
        compare(Claude.ageLine(119 * 1000, now, tr), "");
        compare(Claude.ageLine(2 * minuteMs, now, tr), "");
    }

    function test_ageLineMinutes() {
        const now = resetAt;
        compare(Claude.ageLine(2 * minuteMs + 1000, now, tr), "as of 2 min ago");
        compare(Claude.ageLine(12 * minuteMs, now, tr), "as of 12 min ago");
        compare(Claude.ageLine(3 * hourMs, now, tr), "as of 3 h ago");
    }

    function test_ageLineDays() {
        const tuesdayNoon = new Date(2026, 9, 27, 12, 0).getTime();
        compare(Claude.ageLine(2 * dayMs, tuesdayNoon, tr), "since Sun");
    }

    // ageText's own words go through the same tr (D23).
    function test_ageLineThroughTr() {
        const marked = text => "<" + text + ">";
        compare(Claude.ageLine(12 * minuteMs, resetAt, marked), "<as of <12 min> ago>");
    }

    // The status line with data known at 10:52 local.
    function lineWithData(status, errorCode, errorMessage) {
        const knownAt = new Date(2026, 9, 25, 10, 52).getTime();
        return Claude.statusLine(status, errorCode, errorMessage, true, knownAt, localTime, tr);
    }

    function lineWithoutData(status, errorCode, errorMessage) {
        return Claude.statusLine(status, errorCode, errorMessage, false, null, localTime, tr);
    }

    // The codes with a fixed text get an engine message that differs from it, so passing the message
    // through would fail.
    function test_statusLineEachCode() {
        const engineText = "engine text";
        const signIn = "Sign-in needed: run claude, then /login";
        const probeOff = "Probe off, waiting for a Claude Code session";
        const rateLimited = "Rate-limited, retrying at 11:34";
        const probeFailed = "Probe failed, retrying at 11:20";
        const noPlanLimits = "This Claude account reports no plan limits";
        compare(lineWithData("stale", "sign_in_required", engineText), signIn);
        compare(lineWithData("stale", "rate_limited", rateLimited), rateLimited);
        compare(lineWithData("stale", "fetch_failed", probeFailed), probeFailed);
        compare(lineWithData("offline", "", ""), "Offline, as of 10:52");
        compare(lineWithData("unconfigured", "not_configured", engineText), probeOff);
        compare(lineWithData("unconfigured", "not_installed", engineText), "claude not installed");
        compare(lineWithData("unconfigured", "disabled", engineText), "Claude engine off");
        compare(lineWithoutData("unconfigured", "not_configured", engineText), "Claude usage unavailable: " + probeOff);
        compare(lineWithoutData("unconfigured", "not_installed", engineText), "Claude usage unavailable: claude not installed");
        compare(lineWithoutData("unconfigured", "disabled", engineText), "Claude usage unavailable: Claude engine off");
        compare(lineWithoutData("offline", "", ""), "Offline");
        compare(lineWithoutData("starting", "", ""), "Waiting for the first reading");
        compare(lineWithoutData("waiting", "", ""), "Waiting for the first reading");
        compare(lineWithData("error", "no_plan_limits", noPlanLimits), noPlanLimits);
        compare(lineWithData("failed", "engine_failed", "panicked at actor.rs"), "claude engine failed: panicked at actor.rs");
        compare(lineWithData("ok", "", ""), "");
        compare(lineWithData("starting", "", ""), "");
    }

    // A status without a message gets a fixed text, never the bare status word.
    function test_statusLineWithoutMessage() {
        compare(lineWithoutData("unconfigured", "", ""), "Claude usage unavailable: Not configured");
        compare(lineWithoutData("error", "", ""), "Claude usage unavailable: Error");
        compare(lineWithData("error", "", ""), "Error");
        compare(lineWithData("stale", "", ""), "No fresh data");
        compare(lineWithData("unconfigured", "", ""), "Not configured");
        compare(lineWithData("failed", "", ""), "claude engine failed");
    }

    function test_statsStatusIndexingPercent() {
        const indexing = {
            state: "indexing",
            files_total: 14695,
            files_done: 6000,
            skipped_lines: 0
        };
        compare(Claude.statsStatus("starting", true, indexing, "", tr), "Indexing transcripts 40 %");
    }

    function test_statsStatusWithoutData() {
        compare(Claude.statsStatus("waiting", false, null, "", tr), "Waiting for the first reading");
        compare(Claude.statsStatus("starting", false, null, "", tr), "Waiting for the first reading");
    }

    function test_statsStatusOtherwise() {
        const ready = {
            state: "ready",
            files_total: 14695,
            files_done: 14695,
            skipped_lines: 0
        };
        compare(Claude.statsStatus("ok", true, ready, "", tr), "");
        compare(Claude.statsStatus("unconfigured", false, null, "~/.claude/projects does not exist", tr), "~/.claude/projects does not exist");
        compare(Claude.statsStatus("unconfigured", true, ready, "", tr), "Not configured");
        compare(Claude.statsStatus("unconfigured", true, ready, "enabled = false", tr), "enabled = false");
        compare(Claude.statsStatus("error", true, ready, "usage.db quarantined", tr), "usage.db quarantined");
        compare(Claude.statsStatus("error", true, ready, "", tr), "Error");
        compare(Claude.statsStatus("failed", true, ready, "panicked", tr), "claude engine failed: panicked");
    }

    function test_gaugeLabel() {
        const window = currentWindow(42, resetAt - hourMs);
        const now = resetAt - (2 * hourMs + 13 * minuteMs);
        compare(Claude.gaugeLabel(window, now, localTime, tr), "resets in 2h 13m");
        compare(Claude.gaugeLabel(window, resetAt + minuteMs, localTime, tr), "Reset at 13:14, current unknown");
        compare(Claude.gaugeLabel(null, now, localTime, tr), "--");
    }

    function test_sessionLine() {
        compare(Claude.sessionLine("axon design", 18.42, 61, tr), "axon design · session $18.42 · context 61%");
        compare(Claude.sessionLine("axon design", 18.42, -1, tr), "axon design · session $18.42 · context --");
        compare(Claude.formatCost(38.17), "$38.17");
    }

    // A session that reported no cost (-1) shows "--", never "$0.00".
    function test_sessionLineWithoutCost() {
        compare(Claude.sessionLine("axon design", -1, 61, tr), "axon design · session -- · context 61%");
        compare(Claude.sessionLine("axon design", 0, 61, tr), "axon design · session $0.00 · context 61%");
    }

    // Ten pixels a character, and an elide that keeps what fits before "…".
    function advance(text) {
        return text.length * 10;
    }

    function elide(text, width) {
        return advance(text) <= width ? text : text.slice(0, Math.floor(width / 10) - 1) + "…";
    }

    function test_twoLinesKeepsAShortTextWhole() {
        compare(Claude.twoLines("Rate-limited, retrying at 11:34", 400, advance, elide), ["Rate-limited, retrying at 11:34"]);
        compare(Claude.twoLines("", 400, advance, elide), []);
    }

    // The first line takes whole words while they fit, the second the rest, elided.
    function test_twoLinesWrapsAtAWordAndElidesTheRest() {
        const lines = Claude.twoLines("aaaa bbbb cccc dddd eeee ffff", 100, advance, elide);
        compare(lines, ["aaaa bbbb", "cccc dddd…"]);
        for (const line of lines)
            verify(advance(line) <= 100, line);
        compare(Claude.twoLines("aaaa bbbb cccc", 100, advance, elide), ["aaaa bbbb", "cccc"]);
    }

    // A first word wider than the line leaves one elided line.
    function test_twoLinesElidesAWordWiderThanTheLine() {
        compare(Claude.twoLines("abcdefghijklmnop qr", 100, advance, elide), ["abcdefghi…"]);
    }

    function test_basename() {
        compare(Claude.basename("/home/ryutribal/programming/axon"), "axon");
        compare(Claude.basename("/home/ryutribal/programming/axon/"), "axon");
        compare(Claude.basename(""), "");
    }

    function test_zoneTime() {
        compare(Claude.zoneTime(new Date(2026, 9, 25, 9, 5).getTime()), "09:05");
    }
}
