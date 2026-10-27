import QtQuick

// Stands in for Quickshell's Singleton in tests: a plain object that holds children.
QtObject {
    default property list<QtObject> data
}
