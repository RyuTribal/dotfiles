import QtQuick
import QtTest
import Quickshell.Io
import ShellUnderTest

// The real services/NerdGlyphs.qml over a stand-in Process: the script's dump becomes `list`, a plain
// JS array of its trimmed, non-empty lines, and preparedEntries follows it. backend_tests.sh copies
// NerdGlyphs.qml without its `pragma Singleton` line into ShellUnderTest as NerdGlyphsService, so each
// test makes its own instance. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "NerdGlyphs"

    Component {
        id: glyphsComponent
        NerdGlyphsService {}
    }

    // Reads the list the way SearchWidget does, in a binding.
    Component {
        id: readerComponent
        QtObject {
            property var glyphs
            readonly property int count: glyphs?.list?.length ?? 0
        }
    }

    SignalSpy {
        id: listChanges
        signalName: "listChanged"
    }

    readonly property int glyphCount: 300

    function init() {
        ProcessProbe.started = [];
        listChanges.clear();
    }

    // A new instance, which starts its dump at creation as in the shell.
    function startGlyphs() {
        const glyphs = createTemporaryObject(glyphsComponent, tc);
        verify(glyphs);
        compare(ProcessProbe.started.length, 1);
        listChanges.target = glyphs;
        return glyphs;
    }

    function glyphLine(index) {
        return ` terminal ${index} nf-fa-terminal_${index}`;
    }

    // The dump as the script prints it, with padding, blank lines and a \r for the trim to drop.
    function scriptDump() {
        let dump = "";
        for (let index = 0; index < tc.glyphCount; index++)
            dump += index % 50 === 0 ? `  ${glyphLine(index)}\r\n\n   \n` : `${glyphLine(index)}\n`;
        return dump;
    }

    function test_theDumpBecomesAPlainArrayOfItsTrimmedLines() {
        const glyphs = startGlyphs();
        const process = ProcessProbe.started[0];
        compare(process.command, ["/nonexistent/config/hypr/hyprland/scripts/fuzzel-nerdfont.sh", "--dump"]);
        verify(glyphs.loading);

        process.stdout.text = scriptDump();
        process.exited(0, 0);

        verify(!glyphs.loading);
        verify(Array.isArray(glyphs.list), "list is a JS array, not a list<var> sequence");
        const expected = [];
        for (let index = 0; index < tc.glyphCount; index++)
            expected.push(glyphLine(index));
        compare(glyphs.list, expected);
        compare(glyphs.preparedEntries.length, tc.glyphCount);
        compare(glyphs.preparedEntries[7].entry, glyphLine(7));
        compare(listChanges.count, 1);
    }

    // SearchWidget's reads: list?.length in a binding, fuzzyQuery("") for the whole list, and a search.
    function test_consumersSeeTheLoadedList() {
        const glyphs = startGlyphs();
        const reader = createTemporaryObject(readerComponent, tc, {
            glyphs: glyphs
        });
        compare(reader.count, 0);

        ProcessProbe.started[0].stdout.text = scriptDump();
        ProcessProbe.started[0].exited(0, 0);

        compare(reader.count, tc.glyphCount);
        compare(glyphs.fuzzyQuery("").length, tc.glyphCount);
        compare(glyphs.fuzzyQuery("terminal_123"), [glyphLine(123)]);
    }

    function test_aFailedDumpKeepsTheListAndSaysWhy() {
        const glyphs = startGlyphs();
        ProcessProbe.started[0].stdout.text = " partial";
        ignoreWarning("nerdfont picker exited with code 1");
        ProcessProbe.started[0].exited(1, 0);

        verify(!glyphs.loading);
        compare(glyphs.lastError, "nerdfont picker exited with code 1");
        compare(glyphs.list, []);
        compare(listChanges.count, 0);
    }
}
