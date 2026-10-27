import QtQuick
import Quickshell
import qs.modules.common

// Shows ConfigErrorToast at the top of the screen while config.json does not parse. It stays until
// the file is fixed: Config.fileValid turns true again on the next valid reload.
Scope {
    LazyLoader {
        active: Config.ready && !Config.fileValid

        PanelWindow {
            exclusiveZone: 0
            anchors.top: true
            margins.top: 0
            implicitWidth: toast.implicitWidth
            implicitHeight: toast.implicitHeight
            color: "transparent"

            ConfigErrorToast {
                id: toast
                anchors.fill: parent
            }
        }
    }
}
