pragma Singleton
pragma ComponentBehavior: Bound

import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Io

/**
 * The shell's one door to axon's theme engine in stage 1 (section 40.7.14): the active generation's
 * iris.json, whose colours it writes into Appearance, and the `theme` topic for the rest. Other files
 * read Theme.* and call its functions, never `Backend.call("theme.…")`, so stage 2 replaces this file
 * alone. The colours come from the file, so they stay while axon is down.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "theme"
        onSnapshot: data => root._applyTopic(data)
    }

    // From the loaded iris.json, which is what the colours show; the topic stands in before the first load.
    readonly property string themeId: _tokens?.theme?.id ?? _topicData?.active ?? ""
    readonly property string mode: _tokens?.mode ?? _topicData?.mode ?? "" // "dark" or "light"
    readonly property string generation: _tokens?.generation ?? _topicData?.generation ?? ""
    readonly property var wallpaper: _tokens?.wallpaper ?? _topicData?.wallpaper ?? null // {name, path, width, height}
    readonly property string wallpaperUrl: wallpaper ? _fileUrl(wallpaper.path) : ""

    // From the active theme's row of the topic: the theme axon renders, which the knobs and the
    // wallpaper list follow. Between a switch and its reload it is not yet `themeId`.
    readonly property string activeThemeId: _activeRow?.id ?? ""
    readonly property var modes: _activeRow?.modes ?? []
    readonly property double wallpapersRev: _activeRow?.wallpapers?.rev ?? 0 // a 53-bit row_rev: too wide for int
    readonly property var knobs: _activeRow?.knobs ?? []
    readonly property var knobWarnings: _messagesByKey(_activeRow?.warnings ?? []) // "theme.<id>.<knob>" -> text
    readonly property bool hasSeedKnob: knobs.some(knob => knob.id === "seed")

    // Only the main shell reports its loads to axon: shell.qml sets this, and the settings, screenshot and
    // kill-dialog processes never do, so they cannot mark the iris target.
    property bool reportsIris: false

    property var _tokens: null // the last iris.json that parsed
    property var _topicData: null
    property string _reloadedFor: "" // the topic generation the last reload was for
    property var _unreported: null // the args of the last load's report, until it is sent
    readonly property var _activeRow: _topicData?.themes?.find(row => row.id === _topicData.active) ?? null
    readonly property bool _backendUp: Backend.up

    // Reads iris.json again.
    function reload() {
        tokensFile.reload();
    }

    function setMode(mode, done) {
        Backend.call("theme.set_mode", {
            mode: mode
        }, done);
    }

    // Writes a knob of the topic's active theme to axon.toml; `done` gets null or the error. The knobs
    // follow the topic, not iris.json: a write targets the theme axon renders, which between a switch
    // and its reload is not the one the colours on screen came from.
    function setKnob(name, value, done) {
        AxonConfig.set(_knobKey(name), value, done);
    }

    // The warning text of a knob of the topic's active theme, or "".
    function knobWarning(name) {
        return knobWarnings[_knobKey(name)] ?? "";
    }

    function _knobKey(name) {
        return "theme." + activeThemeId + "." + name;
    }

    // The least busy spot of the active wallpaper for the background clock (40.7.15).
    function clockSpot(args, done) {
        Backend.call("theme.clock_spot", args, done);
    }

    function _applyTopic(data) {
        _topicData = data;
        const topicGeneration = data.generation;
        if (topicGeneration && topicGeneration !== _tokens?.generation && topicGeneration !== _reloadedFor) {
            _reloadedFor = topicGeneration;
            tokensFile.reload();
        }
    }

    // A file that does not parse, or holds anything but colours of roles Appearance declares, applies
    // nothing: the colours stay and the failure is reported against the topic's generation.
    function _load(text) {
        let tokens;
        try {
            tokens = JSON.parse(text);
            if (tokens?.schema !== 1)
                throw new Error("schema " + tokens?.schema + ", this shell reads 1");
            _checkColors(tokens.colors);
        } catch (error) {
            _report(_topicData?.generation ?? "", "iris.json: " + error.message);
            return;
        }
        for (const [role, value] of Object.entries(tokens.colors))
            Appearance.m3colors[_m3Key(role)] = _qtColor(value);
        Appearance.m3colors.darkmode = tokens.mode === "dark";
        _tokens = tokens;
        _report(tokens.generation, "");
    }

    function _report(loadedGeneration, error) {
        const report = {
            generation: loadedGeneration,
            target: "iris",
            ok: error === ""
        };
        if (error !== "")
            report.error = error;
        _unreported = report;
        _sendReport();
    }

    function _sendReport() {
        if (!reportsIris || !Backend.up || _unreported === null)
            return;
        Backend.call("theme.report_target", _unreported);
        _unreported = null;
    }

    // Throws unless `colors` maps roles Appearance declares to #rrggbb or #rrggbbaa strings.
    function _checkColors(colors) {
        if (typeof colors !== "object" || colors === null || Array.isArray(colors))
            throw new Error("colors is not an object");
        for (const [role, value] of Object.entries(colors)) {
            if (!(_m3Key(role) in Appearance.m3colors))
                throw new Error("colors." + role + " is no role this shell knows");
            if (typeof value !== "string" || !/^#[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$/.test(value))
                throw new Error("colors." + role + " is not a colour: " + JSON.stringify(value));
        }
    }

    // The stage 1 key mapping of the old colour loader: on_primary_container is m3onPrimaryContainer.
    function _m3Key(role) {
        return "m3" + role.replace(/_([a-z])/g, match => match[1].toUpperCase());
    }

    // iris.json writes #rrggbbaa, Qt reads eight digits as #aarrggbb.
    function _qtColor(hex) {
        return hex.length === 9 ? "#" + hex.slice(7) + hex.slice(1, 7) : hex;
    }

    // Every byte outside [A-Za-z0-9/._-] percent-encoded, so names with spaces, # or quotes load.
    function _fileUrl(path) {
        const encoded = encodeURIComponent(path).replace(/[!'()*~]/g, character => {
            return "%" + character.charCodeAt(0).toString(16).toUpperCase();
        });
        return "file://" + encoded.replace(/%2F/g, "/");
    }

    function _messagesByKey(rows) {
        const messages = {};
        for (const row of rows.filter(warning => warning.key)) {
            const text = row.message ?? row.code;
            messages[row.key] = messages[row.key] ? messages[row.key] + "\n" + text : text;
        }
        return messages;
    }

    onReportsIrisChanged: _sendReport()
    on_BackendUpChanged: _sendReport()

    // No watchChanges: a new generation moves the `current` link, which changes no watched inode, so
    // the file is read again when the topic's generation moves.
    FileView {
        id: tokensFile
        path: Directories.axonThemeCurrent + "/iris.json"
        onLoaded: root._load(text())
    }
}
