import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import qs.modules.sidebarRight
import qs
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland

QuickToggleButton {
    id: root

    // The "Details" launcher: apps.networkEthernet on a wired connection, else apps.network.
    function openDetails() {
        Quickshell.execDetached(["bash", "-c", `${Network.ethernet ? Config.options.apps.networkEthernet : Config.options.apps.network}`]);
        GlobalStates.sidebarRightOpen = false;
    }

    toggled: Network.wifiEnabled
    buttonIcon: Network.materialSymbol
    onClicked: Network.toggleWifi()
    altAction: () => root.openDetails()
    StyledToolTip {
        content: Translation.tr("%1 | Right-click to configure").arg(Network.networkName)
    }
}
