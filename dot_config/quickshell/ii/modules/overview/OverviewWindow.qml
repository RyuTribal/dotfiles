import qs
import qs.services
import qs.modules.common
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland

Item { // Window
    id: root
    property var toplevel
    property var windowData
    property var monitorData
    property var scale
    property var availableWorkspaceWidth
    property var availableWorkspaceHeight
    property bool restrictToWorkspace: true
    // A window row has `at` and `size` only once the geometry check is armed (section 30), so until
    // that frame the window is a zero-size placeholder at its slot's corner. monitorData is null when
    // the row names no known monitor.
    property real initX: Math.max(((windowData?.at?.[0] ?? 0) - (monitorData?.x ?? 0) - (monitorData?.reserved[0] ?? 0)) * root.scale, 0) + xOffset
    property real initY: Math.max(((windowData?.at?.[1] ?? 0) - (monitorData?.y ?? 0) - (monitorData?.reserved[1] ?? 0)) * root.scale, 0) + yOffset
    property real xOffset: 0
    property real yOffset: 0
    property string widgetMonitorName: ""
    
    property var targetWindowWidth: (windowData?.size?.[0] ?? 0) * scale
    property var targetWindowHeight: (windowData?.size?.[1] ?? 0) * scale
    property bool hovered: false
    property bool pressed: false

    property var iconToWindowRatio: 0.35
    property var xwaylandIndicatorToIconRatio: 0.35
    property var iconToWindowRatioCompact: 0.6
    property var iconPath: Quickshell.iconPath(AppSearch.guessIcon(windowData?.class), "image-missing")
    property bool compactMode: Appearance.font.pixelSize.smaller * 4 > targetWindowHeight || Appearance.font.pixelSize.smaller * 4 > targetWindowWidth

    property bool indicateXWayland: windowData?.xwayland ?? false
    
    x: initX
    y: initY
    width: (windowData?.size?.[0] ?? 0) * root.scale
    height: (windowData?.size?.[1] ?? 0) * root.scale
    opacity: windowData?.monitor === widgetMonitorName ? 1 : 0.4

    // A drag sets x and y and so ends their bindings. Each new row puts them back, so the snapshot
    // after a move places the window in its new slot (N30-4); not while it is being dragged.
    onWindowDataChanged: {
        if (root.pressed)
            return;
        root.x = Qt.binding(() => root.initX);
        root.y = Qt.binding(() => root.initY);
    }

    layer.enabled: true
    layer.effect: OpacityMask {
        maskSource: Rectangle {
            width: root.width
            height: root.height
            radius: Appearance.rounding.windowRounding * root.scale
        }
    }

    Behavior on x {
        animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
    }
    Behavior on y {
        animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
    }
    Behavior on width {
        animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
    }
    Behavior on height {
        animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
    }

    ScreencopyView {
        id: windowPreview
        anchors.fill: parent
        captureSource: GlobalStates.overviewOpen ? root.toplevel : null
        live: true

        // Color overlay for interactions
        Rectangle {
            anchors.fill: parent
            radius: Appearance.rounding.windowRounding * root.scale
            color: pressed ? ColorUtils.transparentize(Appearance.colors.colLayer2Active, 0.5) : 
                hovered ? ColorUtils.transparentize(Appearance.colors.colLayer2Hover, 0.7) : 
                ColorUtils.transparentize(Appearance.colors.colLayer2)
            border.color : ColorUtils.transparentize(Appearance.m3colors.m3outline, 0.7)
            border.width : 1
        }

        ColumnLayout {
            anchors.verticalCenter: parent.verticalCenter
            anchors.left: parent.left
            anchors.right: parent.right
            spacing: Appearance.font.pixelSize.smaller * 0.5

            Image {
                id: windowIcon
                property var iconSize: {
                    // console.log("-=-=-", root.toplevel.title, "-=-=-")
                    // console.log("Target window size:", targetWindowWidth, targetWindowHeight)
                    // console.log("Icon ratio:", root.compactMode ? root.iconToWindowRatioCompact : root.iconToWindowRatio)
                    // console.log("Scale:", root.monitorData.scale)
                    // console.log("Final:", Math.min(targetWindowWidth, targetWindowHeight) * (root.compactMode ? root.iconToWindowRatioCompact : root.iconToWindowRatio) / root.monitorData.scale)
                    return Math.min(targetWindowWidth, targetWindowHeight) * (root.compactMode ? root.iconToWindowRatioCompact : root.iconToWindowRatio) / (root.monitorData?.scale ?? 1);
                }
                // mipmap: true
                Layout.alignment: Qt.AlignHCenter
                source: root.iconPath
                width: iconSize
                height: iconSize
                sourceSize: Qt.size(iconSize, iconSize)

                Behavior on width {
                    animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
                }
                Behavior on height {
                    animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
                }
            }
        }
    }
}