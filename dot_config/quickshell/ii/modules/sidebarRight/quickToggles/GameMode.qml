import qs.modules.common
import qs.modules.common.widgets
import qs
import qs.services

QuickToggleButton {
    id: root
    buttonIcon: "gamepad"
    toggled: Hypr.gameMode
    onClicked: Hypr.setGameMode(!Hypr.gameMode)
    StyledToolTip {
        content: Translation.tr("Game mode")
    }
}
