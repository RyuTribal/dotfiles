.pragma library

// Pure helpers for the axon client (Backend.qml and the services built on it).
// Displayed times derive from absolute wire timestamps and the `now` the caller
// passes (Backend.now), never from the moment a frame arrived, so a reconnect
// replay shows the same thing.

const SECOND_MS = 1000;
const MINUTE_MS = 60 * SECOND_MS;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const SHORT_WEEKDAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

// Keeps the previous row object whenever id and rev match, so a
// ScriptModel { objectProp: "id" } sees an identical object and keeps that
// row's delegate. Returns the rows in wire order and an index by id, which
// the caller passes back as prevIndex next time. Rows missing from `rows`
// are dropped from the index.
function reuseRows(prevIndex, rows) {
    const index = Object.create(null); // no prototype, so "__proto__" is an ordinary id
    const kept = rows.map(row => {
        const previous = prevIndex[row.id];
        const reused = previous !== undefined && previous.rev === row.rev ? previous : row;
        index[row.id] = reused;
        return reused;
    });
    return { rows: kept, index: index };
}

// The position of a media player row (section 31) at `now`. Its rate is 0
// unless it plays, so a paused player stays put. Clamped to 0..length_ms;
// length_ms is null when the player reports no length.
function mediaPosition(player, now) {
    const position = player.position_ms + player.rate * (now - player.sampled_at);
    const capped = typeof player.length_ms === "number" ? Math.min(position, player.length_ms) : position;
    return Math.max(0, capped);
}

// Time left until an absolute deadline (pomodoro ends_at, claude resets_at).
function remaining(endsAt, now) {
    return Math.max(0, endsAt - now);
}

// Stopwatch time (section 32): counted from started_at while it runs, else
// the elapsed_ms it was paused at. Never negative, even right after the wall
// clock stepped back.
function elapsed(stopwatch, now) {
    if (stopwatch.started_at !== null)
        return Math.max(0, now - stopwatch.started_at);
    return stopwatch.elapsed_ms;
}

// Time since boot from the system topic's boot_at, or null when boot_at is
// unknown (null while /proc/stat has no readable btime). A wall clock step can
// make it negative for a moment; uptimeText reads that as 0.
function uptime(bootAt, now) {
    if (!Number.isFinite(bootAt))
        return null;
    return now - bootAt;
}

// How old a value is, for a widget that shows its last known value: "now",
// "45 s", "12 min", "3 h", then the weekday it was known on ("since Sun").
// Floors throughout; a negative age (the wall clock stepped back) is "now".
// Every word goes through the caller's tr (Translation.tr in the shell).
function ageText(ms, now, tr) {
    if (ms < 5 * SECOND_MS)
        return tr("now");
    if (ms < MINUTE_MS)
        return tr("%1 s").arg(Math.floor(ms / SECOND_MS));
    if (ms < HOUR_MS)
        return tr("%1 min").arg(Math.floor(ms / MINUTE_MS));
    if (ms < DAY_MS)
        return tr("%1 h").arg(Math.floor(ms / HOUR_MS));
    return tr("since %1").arg(tr(SHORT_WEEKDAYS[new Date(now - ms).getDay()]));
}

// Uptime in the bar's format from before axon (DateTime.qml): "1d, 5m", "2h",
// "0m". Units that are zero are left out, minutes show when nothing else
// does, and a negative input counts as 0. An unknown uptime (null) is "--",
// never a made-up value.
function uptimeText(ms) {
    if (!Number.isFinite(ms))
        return "--";
    const totalMinutes = Math.floor(Math.max(0, ms) / MINUTE_MS);
    const days = Math.floor(totalMinutes / (24 * 60));
    const hours = Math.floor(totalMinutes / 60) % 24;
    const minutes = totalMinutes % 60;
    const parts = [];
    if (days > 0)
        parts.push(days + "d");
    if (hours > 0)
        parts.push(hours + "h");
    if (minutes > 0 || parts.length === 0)
        parts.push(minutes + "m");
    return parts.join(", ");
}
