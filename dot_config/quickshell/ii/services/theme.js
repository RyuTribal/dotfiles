.pragma library

// Pure helpers for the theme controls outside services/Theme.qml: the overview's accentcolor action
// (SearchWidget.qml) and the clock format selector (GeneralConfig.qml), sections 40.13 and 50.1.

// The colour the accentcolor action writes to the theme's seed knob: six hex digits with or without
// the leading `#`, as the old wallpaper script took them, with the `#` added when missing. Null for anything else.
function accentColour(argument) {
    if (!/^#?[A-Fa-f0-9]{6}$/.test(argument))
        return null;
    return argument.startsWith("#") ? argument : "#" + argument;
}

// axon.toml's `theme.lock_clock` for a `time.format` of the clock selector: the fallback locker
// shows a 24-hour clock for "hh:mm" and a 12-hour one for the two am/pm formats.
function lockClock(timeFormat) {
    return timeFormat === "hh:mm" ? "24h" : "12h";
}
