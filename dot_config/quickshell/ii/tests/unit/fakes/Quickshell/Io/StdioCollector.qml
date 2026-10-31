import QtQuick

// Stands in for Quickshell's StdioCollector in tst_nerd_glyphs.qml: the test sets `text` to what the
// process printed, then ends the Process with exited(), as Quickshell sets it before that signal.
QtObject {
    property string text: ""
}
