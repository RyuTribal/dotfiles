pragma Singleton

import QtQuick

// Stands in for modules/common/Directories.qml next to the real Config.qml in tst_config.qml.
QtObject {
    property string shellConfigPath: "/nonexistent/config.json"
}
