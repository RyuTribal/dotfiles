import QtQuick

// Stands in for Quickshell's SplitParser in tests: the test emits read(line) itself.
QtObject {
    signal read(string data)
}
