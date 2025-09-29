pragma Singleton
pragma ComponentBehavior: Bound

import qs.modules.common
import qs.modules.common.functions
import QtQuick
import Quickshell
import Quickshell.Io

/**
 * Nerd Fonts glyphs provider.
 * With the new script, we read from stdout instead of a ### DATA ### section.
 * Each line format (from --dump):
 *   <glyph> <name words...> <keywords...> <nf-id>
 * Example:
 *    terminal prompt shell code nf-fa-terminal
 */
Singleton {
    id: root

    // Path to your new generator script
    // (the one that intersects installed fonts and i_all.json, and supports --dump / --update)
    property string glyphScriptExecPath: `${Directories.config}/hypr/hyprland/scripts/fuzzel-nerdfont.sh`

    // Optional: pass a font filter regex like --font "Symbols Nerd"
    property string fontRegex: ""

    // Data (one string per line)
    property list<var> list: []
    property bool loading: false
    property string lastError: ""

    // Fuzzy precomputation
    readonly property var preparedEntries: list.map(a => ({
                name: Fuzzy.prepare(String(a)),
                entry: a
            }))

    // Configs for sloppy fuzzy
    property bool sloppySearch: false
    property real scoreThreshold: 0.3

    function fuzzyQuery(search) {
        if (!search || !search.trim())
            return list;

        if (root.sloppySearch) {
            const needle = search.toLowerCase();
            return list.slice(0, 5000).map(str => ({
                        entry: str,
                        score: Levendist.computeTextMatchScore(String(str).toLowerCase(), needle)
                    })).filter(x => x.score > root.scoreThreshold).sort((a, b) => b.score - a.score).map(x => x.entry);
        }

        return Fuzzy.go(search, preparedEntries, {
            all: true,
            key: "name"
        }).map(r => r.obj.entry);
    }

    // Public API
    function load(update = false) {
        // Run: nerdfont-picker-local.sh --dump [--update] [--font <re>]
        let cmd = [glyphScriptExecPath, "--dump"];
        if (update)
            cmd.push("--update");
        if (fontRegex && fontRegex.length > 0) {
            cmd.push("--font");
            cmd.push(fontRegex);
        }
        glyphProc.running = false;
        glyphProc.command = cmd;
        glyphProc.running = true;
        loading = true;
        lastError = "";
        glyphProcBuffer = "";
    }

    // Back-compat helper: if you still keep a legacy file with ### DATA ###,
    // you can call this to parse it – not used by default anymore.
    function updateGlyphsFromFileLikeContent(fileContent) {
        const lines = fileContent.split("\n");
        const i = lines.indexOf("### DATA ###");
        if (i === -1) {
            console.warn("NerdGlyphs: No DATA section in legacy file.");
            return;
        }
        root.list = lines.slice(i + 1).map(l => l.trim()).filter(Boolean);
    }

    // Internal: accumulate stdout and commit to `list`
    property string glyphProcBuffer: ""

    Process {
        id: glyphProc
        command: [glyphScriptExecPath, "--dump"] // will be overwritten in load()
        stdout: SplitParser {
            onRead: data => {
                root.glyphProcBuffer += data + "\n";
            }
        }
        onExited: code => {
            root.loading = false;
            if (code !== 0) {
                root.lastError = `nerdfont picker exited with code ${code}`;
                console.warn(root.lastError);
                return;
            }
            const lines = root.glyphProcBuffer.split("\n").map(l => l.trim()).filter(l => l.length > 0);
            root.list = lines;
            root.glyphProcBuffer = "";
        }
    }

    // Kick an initial load on first use
    Component.onCompleted: {
        // first try without update (fast), user can call load(true) later
        load(false);
    }
}
