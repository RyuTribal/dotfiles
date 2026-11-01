.pragma library

// Pure helpers for services/Hypr.qml and the widgets that read it. Rows are the hypr topic's
// (section 30): a window's `workspace` and a workspace's `ws_id` are null on a special or named
// workspace (ruling D1).

// The rows by the value of `key`, for lookups. A row whose value is null or missing is left out,
// so looking up null never finds a workspace without a number. The index has no prototype, so an
// id such as "constructor" finds nothing it does not hold.
function indexBy(rows, key) {
    const index = Object.create(null);
    for (const row of rows) {
        const value = row[key];
        if (value !== null && value !== undefined)
            index[value] = row;
    }
    return index;
}

// The bar's two lines for one monitor: the active window when it is on that monitor, else the
// workspace's icon window, else the given desktop and workspace texts.
function barTitle(active, monitorName, icon, desktopText, workspaceText) {
    if (active && active.monitor === monitorName)
        return { subtitle: active.class, title: active.title };
    if (icon)
        return { subtitle: icon.class, title: icon.title };
    return { subtitle: desktopText, title: workspaceText };
}

// Whether workspace number `workspaceNumber` is in group `group` when `shown` workspaces make a
// group: group 0 holds 1 to `shown`. A null number is in no group, so a scratchpad window shows in
// none (E5).
function inGroup(workspaceNumber, group, shown) {
    if (typeof workspaceNumber !== "number")
        return false;
    return group * shown < workspaceNumber && workspaceNumber <= (group + 1) * shown;
}
