.pragma library

.import "backend.js" as BackendHelpers

// Pure helpers for the claude views: services/ClaudeUsage.qml, the bar's ClaudeIndicator and the top
// menu's ClaudePanel. A function that returns text for people takes the caller's tr last
// (Translation.tr in the shell), and each text is one tr template filled with .arg. Times are
// absolute wire milliseconds read against the `now` the caller passes (Backend.now).

const MINUTE_MS = 60 * 1000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const AGE_HIDDEN_MS = 2 * MINUTE_MS;

// Time left as "45m", "2h 13m" or "6d 7h", floored; under a minute, nothing left included, is
// "<1m".
function formatCountdown(ms, tr) {
    const left = Math.max(0, ms);
    if (left < MINUTE_MS)
        return tr("<1m");
    if (left < HOUR_MS)
        return tr("%1m").arg(Math.floor(left / MINUTE_MS));
    if (left < DAY_MS)
        return tr("%1h %2m").arg(Math.floor(left / HOUR_MS)).arg(Math.floor(left % HOUR_MS / MINUTE_MS));
    return tr("%1d %2h").arg(Math.floor(left / DAY_MS)).arg(Math.floor(left % DAY_MS / HOUR_MS));
}

// One `windows` entry of the `claude` topic as the views read it: the percentage (-1 when not
// known), whether the current value is unknown, and the reset time (-1 without a window). A window
// is unknown when the engine says so, or when its reset passed and nothing was observed after it,
// so the view flips on time while axon is down. An unknown window never shows its old percentage.
function windowView(window, now) {
    if (window === null || window === undefined)
        return { pct: -1, unknown: false, resetsAt: -1 };
    const resetPassed = window.resets_at <= now && window.observed_at < window.resets_at;
    const unknown = window.state === "reset_unknown" || resetPassed;
    const pct = unknown || typeof window.used_pct !== "number" ? -1 : window.used_pct;
    return { pct: pct, unknown: unknown, resetsAt: window.resets_at };
}

// The reset row of a window: "resets in 2h 13m", or "Reset at 13:14, current unknown" once the
// current value is unknown. "" without a window.
function resetText(window, now, zoneTime, tr) {
    const view = windowView(window, now);
    if (view.resetsAt < 0)
        return "";
    if (view.unknown)
        return tr("Reset at %1, current unknown").arg(zoneTime(view.resetsAt));
    return tr("resets in %1").arg(formatCountdown(view.resetsAt - now, tr));
}

// The label beside a panel gauge: its reset text, or "--" while the window was never seen.
function gaugeLabel(window, now, zoneTime, tr) {
    return resetText(window, now, zoneTime, tr) || "--";
}

// The panel's session line, "axon design · session $18.42 · context 61%"; a cost or a context the
// session did not report (-1) is "--".
function sessionLine(label, costUsd, contextPct, tr) {
    const cost = costUsd >= 0 ? formatCost(costUsd) : "--";
    const context = contextPct >= 0 ? Math.round(contextPct) + "%" : "--";
    return tr("%1 · session %2 · context %3").arg(label).arg(cost).arg(context);
}

// A USD amount as the panel writes it, "$18.42".
function formatCost(usd) {
    return "$" + usd.toFixed(2);
}

// How old the shown value is: nothing up to 2 min, "as of 12 min ago" within a day, then the
// weekday it was known on ("since Sun").
function ageLine(ageMs, now, tr) {
    if (ageMs <= AGE_HIDDEN_MS)
        return "";
    const age = BackendHelpers.ageText(ageMs, now, tr);
    if (ageMs < DAY_MS)
        return tr("as of %1 ago").arg(age);
    return age;
}

// The fixed text of an error code the view words itself, "" for the others. The rate-limit and
// probe-failure lines are the engine's own messages instead, which carry the retry time.
function codeText(errorCode, tr) {
    switch (errorCode) {
    case "sign_in_required":
        return tr("Sign-in needed: run claude, then /login");
    case "not_configured":
        return tr("Probe off, waiting for a Claude Code session");
    case "not_installed":
        return tr("claude not installed");
    case "disabled":
        return tr("Claude engine off");
    }
    return "";
}

// What a status means when its error carries no message, so the bare wire word never shows.
function statusText(status, tr) {
    switch (status) {
    case "offline":
        return tr("Offline");
    case "unconfigured":
        return tr("Not configured");
    case "stale":
        return tr("No fresh data");
    }
    return tr("Error");
}

// The reason a topic in trouble gives: the code's own text, else the engine's message, else the
// status's text.
function reasonText(status, errorCode, errorMessage, tr) {
    return codeText(errorCode, tr) || errorMessage || statusText(status, tr);
}

function isQuiet(status) {
    return status === "ok" || status === "starting" || status === "waiting";
}

// The one line for the `claude` topic's status and error code (section 32, "Offline and
// failure"). knownAt is the newest datum's time, null without data; "" when there is nothing to
// say.
function statusLine(status, errorCode, errorMessage, hasData, knownAt, zoneTime, tr) {
    if (status === "failed")
        return errorMessage ? tr("claude engine failed: %1").arg(errorMessage) : tr("claude engine failed");
    if (!hasData) {
        if (status === "starting" || status === "waiting")
            return tr("Waiting for the first reading");
        if (status === "ok")
            return "";
        if (status === "offline")
            return tr("Offline");
        return tr("Claude usage unavailable: %1").arg(reasonText(status, errorCode, errorMessage, tr));
    }
    if (status === "offline")
        return tr("Offline, as of %1").arg(zoneTime(knownAt));
    if (isQuiet(status))
        return "";
    return reasonText(status, errorCode, errorMessage, tr);
}

// The status line of the `claude.stats` block: the error while the topic has one, "Waiting for the
// first reading" before its first data, the indexing progress ("Indexing transcripts 40 %",
// floored) while it catches up, else "".
function statsStatus(status, hasData, index, errorMessage, tr) {
    if (status === "failed")
        return errorMessage ? tr("claude engine failed: %1").arg(errorMessage) : tr("claude engine failed");
    if (!isQuiet(status))
        return errorMessage || statusText(status, tr);
    if (!hasData)
        return tr("Waiting for the first reading");
    if (index === null || index === undefined || index.state !== "indexing")
        return "";
    const percent = index.files_total > 0 ? Math.floor(index.files_done * 100 / index.files_total) : 0;
    return tr("Indexing transcripts %1 %").arg(percent);
}

// The last component of a path, ignoring a trailing slash.
function basename(path) {
    const parts = path.split("/").filter(part => part.length > 0);
    return parts.length > 0 ? parts[parts.length - 1] : "";
}

// An instant as HH:MM in the system zone, the zoneTime the text functions above take.
function zoneTime(ms) {
    const date = new Date(ms);
    return String(date.getHours()).padStart(2, "0") + ":" + String(date.getMinutes()).padStart(2, "0");
}
