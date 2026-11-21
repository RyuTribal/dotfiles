pragma Singleton

import QtQuick

// Stands in for modules/common/Config.qml in tst_network_views.qml: the two "Details" launchers, as
// names a test can tell apart.
QtObject {
    property var options: ({
            apps: ({
                    network: "network-details",
                    networkEthernet: "ethernet-details"
                })
        })
}
