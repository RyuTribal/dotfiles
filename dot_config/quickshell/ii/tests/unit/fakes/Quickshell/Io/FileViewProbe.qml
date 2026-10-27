pragma Singleton

import QtQuick

// Hands the test the FileView that Config.qml created, whose id the test cannot reach.
QtObject {
    property QtObject view: null
}
