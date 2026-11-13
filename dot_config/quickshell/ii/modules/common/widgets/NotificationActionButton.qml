import qs.modules.common
import qs.services
import QtQuick

RippleButton {
    id: button
    property string buttonText
    property string urgency

    implicitHeight: 30
    leftPadding: 15
    rightPadding: 15
    buttonRadius: Appearance.rounding.small
    colBackground: (urgency === "critical") ? Appearance.colors.colSecondaryContainer : Appearance.colors.colLayer4
    colBackgroundHover: (urgency === "critical") ? Appearance.colors.colSecondaryContainerHover : Appearance.colors.colLayer4Hover
    colRipple: (urgency === "critical") ? Appearance.colors.colSecondaryContainerActive : Appearance.colors.colLayer4Active

    contentItem: StyledText {
        horizontalAlignment: Text.AlignHCenter
        text: buttonText
        color: (urgency === "critical") ? Appearance.m3colors.m3onSurfaceVariant : Appearance.m3colors.m3onSurface
    }
}