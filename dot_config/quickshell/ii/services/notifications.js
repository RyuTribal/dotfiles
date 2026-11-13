.pragma library

// Pure helpers for services/Notifications.qml, the client of axon's `notify` topic (section 31): the
// mapping of a topic row to the fields the notification widgets read, the popup rules, and the
// per-app grouping. Times are absolute wire milliseconds read against the `now` the caller passes.

// A topic row as the widgets read it; `notificationId` is their name for the id, which every method
// takes as a string. `iconPath` is Quickshell.iconPath: an `image` that is neither
// empty nor a file path (a path starts with "/") is an icon name, which the view cannot load as it is.
function mapRow(row, iconPath) {
    const isIconName = row.image !== "" && !row.image.startsWith("/");
    return {
        id: row.id,
        rev: row.rev,
        notificationId: row.id,
        appIcon: row.app_icon,
        appName: row.app_name,
        body: row.body,
        desktopEntry: row.desktop_entry,
        image: isIconName ? iconPath(row.image) : row.image,
        summary: row.summary,
        time: row.time,
        urgency: row.urgency,
        actions: row.actions.map(action => ({
                    identifier: action.id,
                    text: action.label
                })),
        popup: row.popup,
        popupAt: row.popup_at ?? null,
        popupUntil: row.popup_until ?? null
    };
}

// The rows shown as popups at `now`: it popped, its popupAt has arrived (popups are staggered, so it
// can be ahead of the frame; null counts as arrived, as in the engine) and its deadline has not
// (null is sticky or held). The open sidebar hides every popup.
function popupRows(rows, now, sidebarOpen) {
    if (sidebarOpen)
        return [];
    return rows.filter(row => row.popup && (row.popupAt === null || row.popupAt <= now) && (row.popupUntil === null || now < row.popupUntil));
}

// The earliest instant after `now` at which the popup rows change: a queued popup's popupAt or a
// running popup's popupUntil. -1 when there is none. The engine does not publish at either instant,
// so the service arms a timer for it.
function nextDeadline(rows, now) {
    const instants = [];
    rows.filter(row => row.popup).forEach(row => {
        instants.push(row.popupAt, row.popupUntil);
    });
    const future = instants.filter(instant => instant !== null && instant > now);
    return future.length > 0 ? Math.min(...future) : -1;
}

// The newest `time` of each app: { appName: time }.
function latestTimes(rows) {
    const latest = {};
    rows.forEach(row => {
        latest[row.appName] = Math.max(latest[row.appName] ?? 0, row.time);
    });
    return latest;
}

// The rows by app: { appName: { appName, appIcon, notifications, time } }. `rows` come newest first,
// as the engine sends them, and each group holds its rows oldest first, the order the old service's
// list had, which NotificationGroup reverses for display. So appIcon is the oldest row's, and time is
// the app's latest time in `latest` (its newest row's time when `latest` has none).
function groupsForList(rows, latest) {
    const groups = {};
    rows.slice().reverse().forEach(row => {
        if (!groups[row.appName]) {
            groups[row.appName] = {
                appName: row.appName,
                appIcon: row.appIcon,
                notifications: [],
                time: 0
            };
        }
        groups[row.appName].notifications.push(row);
        groups[row.appName].time = latest[row.appName] || row.time;
    });
    return groups;
}

// The app names of `groups`, the newest group first.
function appNameListForGroups(groups) {
    return Object.keys(groups).sort((first, second) => groups[second].time - groups[first].time);
}

// The ids of the wire rows whose popup a client held (D17: a held popup outlives no client).
function heldIds(rows) {
    return rows.filter(row => row.held).map(row => row.id);
}
