pragma Singleton

import QtQuick

// Stands in for modules/common/Config.qml in tst_bluetooth_views.qml: the "Details" launcher, as a name
// the test can tell from the real `kcmshell6 kcm_bluetooth`.
QtObject {
    property var options: ({
            apps: ({
                    bluetooth: "bluetooth-details"
                })
        })
}
