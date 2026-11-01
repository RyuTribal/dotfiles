import QtQuick
import QtTest
import "../../services/input.js" as I

// The pure parts of services/Input.qml. Needs nothing else:
// QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -input tests/unit/tst_input.qml
TestCase {
    name: "Input"

    // The method rows of section 30's input example.
    readonly property var methods: [
        {
            id: "keyboard-us",
            rev: 1,
            name: "Keyboard - English (US)",
            code: "us"
        },
        {
            id: "keyboard-se",
            rev: 1,
            name: "Keyboard - Swedish",
            code: "se"
        },
        {
            id: "keyboard-ru",
            rev: 1,
            name: "Keyboard - Russian",
            code: "ru"
        },
        {
            id: "keyboard-jp",
            rev: 1,
            name: "Keyboard - Japanese",
            code: "jp"
        }
    ]

    function test_codesEmptyUnlessOk() {
        for (const status of ["stale", "waiting", "unconfigured", "error", "failed"]) {
            compare(I.layoutCodes(status, methods), [], status);
            compare(I.currentCode(status, methods, "keyboard-se"), "", status);
        }
    }

    function test_codesInOrder() {
        compare(I.layoutCodes("ok", methods), ["us", "se", "ru", "jp"]);
    }

    function test_currentCode() {
        compare(I.currentCode("ok", methods, "keyboard-se"), "se");
        compare(I.currentCode("ok", methods, "mozc"), "", "an id not in the list");
    }
}
