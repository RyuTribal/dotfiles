import QtQuick
import qs.modules.common
import qs.modules.common.widgets
import qs
import qs.services

QuickToggleButton {
    id: nightLightButton
    property bool enabled: Hyprsunset.active
    toggled: enabled
    visible: Hyprsunset.available
    buttonIcon: Hyprsunset.automatic ? "night_sight_auto" : "bedtime"
    onClicked: {
        Hyprsunset.toggle()
    }

    altAction: () => {
        Hyprsunset.setAutomatic(!Hyprsunset.automatic)
    }

    StyledToolTip {
        content: Translation.tr("Night Light | Right-click to toggle Auto mode")
    }
}
