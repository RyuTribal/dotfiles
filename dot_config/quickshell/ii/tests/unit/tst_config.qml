import QtQuick
import QtTest
import Quickshell.Io
import ConfigUnderTest

// The real modules/common/Config.qml over a stand-in FileView: the validity check, the write debounce
// and flush(). Run it through backend_tests.sh, which builds the ConfigUnderTest module.
TestCase {
    id: tc
    name: "Config"
    when: windowShown

    property QtObject view: null

    function init() {
        Config.ready; // creates the singleton and with it the FileView
        view = FileViewProbe.view;
        wait(150); // lets the debounce timers of an earlier test run out
        view.content = "{}";
        view.loaded();
        view.writes = 0;
        view.reloads = 0;
    }

    function loadFile(content) {
        view.content = content;
        view.loaded();
    }

    function test_objectIsValid() {
        loadFile('{"a": 1}');
        verify(Config.fileValid);
        compare(Config.parseError, "");
    }

    function test_unparsableTextIsInvalid() {
        loadFile('{"a": ');
        verify(!Config.fileValid);
        verify(Config.parseError.length > 0);
    }

    function test_emptyFileIsInvalid() {
        loadFile("");
        verify(!Config.fileValid);
        verify(Config.parseError.length > 0);
    }

    function test_jsonThatIsNotAnObjectIsInvalid() {
        for (const text of ["[]", "null", '"text"', "3", "true"]) {
            loadFile(text);
            verify(!Config.fileValid, text);
            compare(Config.parseError, "the top level must be a JSON object", text);
        }
    }

    function test_fixingTheFileClearsTheError() {
        loadFile("[]");
        loadFile("{}");
        verify(Config.fileValid);
        compare(Config.parseError, "");
    }

    // The error toast is created when fileValid turns false and reads parseError at once.
    function test_parseErrorIsSetBeforeTheFileTurnsInvalid() {
        const seen = [];
        const record = () => seen.push(Config.parseError);
        Config.fileValidChanged.connect(record);
        loadFile('{"a": ');
        verify(seen.length === 1 && seen[0].length > 0, "text at the moment fileValid turned false: " + seen);
        Config.fileValidChanged.disconnect(record);
    }

    function test_aBurstOfChangesWritesOnce() {
        view.adapterUpdated();
        view.adapterUpdated();
        view.adapterUpdated();
        compare(view.writes, 0);
        tryCompare(view, "writes", 1);
        wait(150);
        compare(view.writes, 1);
    }

    function test_flushWritesAPendingChangeAtOnce() {
        view.adapterUpdated();
        compare(view.writes, 0);
        Config.flush();
        compare(view.writes, 1);
        wait(150);
        compare(view.writes, 1); // the timer was stopped, so no second write
    }

    function test_flushWithNothingPendingWritesNothing() {
        Config.flush();
        compare(view.writes, 0);
    }

    function test_noWriteWhileTheFileIsInvalid() {
        loadFile("[]");
        view.adapterUpdated();
        Config.flush();
        wait(150);
        compare(view.writes, 0);
    }

    function test_aPendingReloadHoldsBackTheWrite() {
        view.adapterUpdated();
        wait(50);
        view.fileChanged(); // a hand save arrives 50 ms into the write debounce
        wait(75); // the write timer has run out, the reload timer has not
        compare(view.writes, 0);
        tryCompare(view, "reloads", 1);
        wait(150);
        compare(view.writes, 0);
    }
}
