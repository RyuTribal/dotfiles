import qs.modules.common
import qs.modules.common.widgets
import qs
import qs.services
import QtQuick
import Quickshell
import "../../../services/power.js" as P

QuickToggleButton {
    id: root
    readonly property var modeIcons: ({
        "lap": "airline_seat_recline_normal",
        "balanced": "balance",
        "max": "local_fire_department"
    })

    visible: Power.modeAvailable
    buttonIcon: modeIcons[Power.mode]
    toggled: Power.mode !== "balanced"

    // axon applies the mode and puts it back when the EC or firmware moves it.
    onClicked: Power.setMode(P.nextMode(Power.mode))

    StyledToolTip {
        content: Translation.tr("Power mode: %1 (%2 W)").arg(Power.mode).arg(Power.watts)
    }
}
