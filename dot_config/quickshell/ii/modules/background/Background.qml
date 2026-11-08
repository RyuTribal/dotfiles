pragma ComponentBehavior: Bound

import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions as CF
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland


Variants {
    id: root
    readonly property bool fixedClockPosition: Config.options.background.fixedClockPosition
    readonly property real fixedClockX: Config.options.background.clockX
    readonly property real fixedClockY: Config.options.background.clockY
    readonly property real clockSizePadding: 20
    readonly property real screenSizePadding: 50
    model: Quickshell.screens

    PanelWindow {
        id: bgRoot

        required property var modelData

        // Hide when fullscreen
        property bool activeWorkspaceHasFullscreen: Hypr.workspace(monitor?.active_workspace)?.has_fullscreen ?? false
        visible: GlobalStates.screenLocked || !activeWorkspaceHasFullscreen || !Config?.options.background.hideWhenFullscreen

        // Workspaces
        property var monitor: Hypr.monitorFor(modelData)
        // Windows on numbered workspaces only: a special or named workspace has no number (null).
        property list<var> relevantWindows: Hypr.windows.filter(w => w.monitor === monitor?.id && w.workspace >= 1).sort((a, b) => a.workspace - b.workspace)
        property int firstWorkspaceId: relevantWindows[0]?.workspace || 1
        property int lastWorkspaceId: relevantWindows[relevantWindows.length - 1]?.workspace || 10
        // Wallpaper: its pixel size comes from iris.json; the screen's size stands in until it is known.
        property real wallpaperToScreenRatio: Math.min(wallpaperWidth / screen.width, wallpaperHeight / screen.height)
        property real preferredWallpaperScale: Config.options.background.parallax.workspaceZoom
        property real effectiveWallpaperScale: zoomScale(wallpaperWidth, wallpaperHeight, screen.width, screen.height, preferredWallpaperScale)
        property int wallpaperWidth: Theme.wallpaper?.width ?? modelData.width
        property int wallpaperHeight: Theme.wallpaper?.height ?? modelData.height
        property real movableXSpace: ((wallpaperWidth / wallpaperToScreenRatio * effectiveWallpaperScale) - screen.width) / 2
        property real movableYSpace: ((wallpaperHeight / wallpaperToScreenRatio * effectiveWallpaperScale) - screen.height) / 2
        // Position
        // axon's theme.clock_spot places the clock in the wallpaper's least busy region. While it
        // refuses (axon down, no wallpaper), the clock stays in the center of the screen.
        property real clockX: modelData.width / 2
        property real clockY: modelData.height / 2
        property var textHorizontalAlignment: clockX < screen.width / 3 ? Text.AlignLeft :
            (clockX > screen.width * 2 / 3 ? Text.AlignRight : Text.AlignHCenter)
        // Colors
        property color dominantColor: Appearance.colors.colPrimary
        property bool dominantColorIsDark: dominantColor.hslLightness < 0.5
        property color colText: CF.ColorUtils.colorWithLightness(dominantColor, (dominantColorIsDark ? 0.8 : 0.12))

        // Layer props
        screen: modelData
        exclusionMode: ExclusionMode.Ignore
        WlrLayershell.layer: GlobalStates.screenLocked ? WlrLayer.Overlay : WlrLayer.Bottom
        // WlrLayershell.layer: WlrLayer.Bottom
        WlrLayershell.namespace: "quickshell:background"
        anchors {
            top: true
            bottom: true
            left: true
            right: true
        }
        color: "transparent"

        // Wallpaper zoom scale, recomputed by its binding whenever the size or the screen changes
        function zoomScale(width, height, screenWidth, screenHeight, preferredScale) {
            if (width <= screenWidth || height <= screenHeight) { // Undersized/perfectly sized wallpapers
                return Math.max(screenWidth / width, screenHeight / height);
            }
            // Oversized = can be zoomed for parallax, yay
            return Math.min(preferredScale, width / screenWidth, height / screenHeight);
        }

        // Clock positioning: a new wallpaper or scale, and every time axon's theme topic is synced again
        // (a call made while axon is down is refused, as at shell start before the connection is up).
        // Qt.callLater folds the triggers of one change into one call.
        onEffectiveWallpaperScaleChanged: Qt.callLater(bgRoot.updateClockPosition)
        Component.onCompleted: Qt.callLater(bgRoot.updateClockPosition)
        Connections {
            target: Theme
            function onWallpaperUrlChanged() {
                Qt.callLater(bgRoot.updateClockPosition);
            }
        }
        Connections {
            target: Theme.topic
            function onSyncedChanged() {
                if (Theme.topic.synced)
                    Qt.callLater(bgRoot.updateClockPosition);
            }
        }
        function updateClockPosition() {
            const args = {
                screen_w: Math.round(bgRoot.screen.width / bgRoot.effectiveWallpaperScale),
                screen_h: Math.round(bgRoot.screen.height / bgRoot.effectiveWallpaperScale),
                box_w: Math.round(clockLoader.implicitWidth + root.clockSizePadding * 2),
                box_h: Math.round(clockLoader.implicitHeight + root.clockSizePadding * 2),
                pad_x: Math.round(bgRoot.movableXSpace + root.screenSizePadding * 2),
                pad_y: Math.round(bgRoot.movableYSpace + root.screenSizePadding * 2)
            };
            Theme.clockSpot(args, reply => {
                if (!reply.ok)
                    return;
                bgRoot.clockX = reply.result.center_x * bgRoot.effectiveWallpaperScale;
                bgRoot.clockY = reply.result.center_y * bgRoot.effectiveWallpaperScale;
                bgRoot.dominantColor = reply.result.dominant_color || Appearance.colors.colPrimary;
            });
        }

        // Wallpaper
        Image {
            id: wallpaper
            visible: opacity > 0
            opacity: status === Image.Ready ? 1 : 0
            Behavior on opacity {
                animation: Appearance.animation.elementMoveEnter.numberAnimation.createObject(this)
            }
            cache: false
            asynchronous: true
            retainWhileLoading: true
            smooth: false
            // Range = groups that workspaces span on
            property int chunkSize: Config?.options.bar.workspaces.shown ?? 10;
            property int lower: Math.floor(bgRoot.firstWorkspaceId / chunkSize) * chunkSize;
            property int upper: Math.ceil(bgRoot.lastWorkspaceId / chunkSize) * chunkSize;
            property int range: upper - lower;
            property real valueX: {
                let result = 0.5;
                if (Config.options.background.parallax.enableWorkspace) {
                    result = (((bgRoot.monitor?.active_workspace ?? 1) - lower) / range);
                }
                if (Config.options.background.parallax.enableSidebar) {
                    result += (0.15 * GlobalStates.sidebarRightOpen);
                }
                return result;
            }
            property real effectiveValueX: Math.max(0, Math.min(1, valueX))
            x: -(bgRoot.movableXSpace) - (effectiveValueX - 0.5) * 2 * bgRoot.movableXSpace
            y: -bgRoot.movableYSpace
            source: Theme.wallpaperUrl
            fillMode: Image.PreserveAspectCrop
            Behavior on x {
                NumberAnimation {
                    duration: 600
                    easing.type: Easing.OutCubic
                }
            }
            Behavior on y {
                NumberAnimation {
                    duration: 600
                    easing.type: Easing.OutCubic
                }
            }
            sourceSize {
                width: bgRoot.screen.width * bgRoot.effectiveWallpaperScale * (bgRoot.monitor?.scale ?? 1)
                height: bgRoot.screen.height * bgRoot.effectiveWallpaperScale * (bgRoot.monitor?.scale ?? 1)
            }
            width: bgRoot.wallpaperWidth / bgRoot.wallpaperToScreenRatio * bgRoot.effectiveWallpaperScale
            height: bgRoot.wallpaperHeight / bgRoot.wallpaperToScreenRatio * bgRoot.effectiveWallpaperScale
            // scale: GlobalStates.screenLocked ? 1.04 : 1
            // Behavior on scale {
            //     NumberAnimation {
            //         duration: 400
            //         easing.type: Easing.BezierSpline
            //         easing.bezierCurve: Appearance.animationCurves.expressiveDefaultSpatial
            //     }
            // }
        }

        // The clock
        Loader {
            id: clockLoader
            active: Config.options.background.showClock
            anchors {
                left: wallpaper.left
                top: wallpaper.top
                leftMargin: bgRoot.movableXSpace + ((root.fixedClockPosition ? root.fixedClockX : bgRoot.clockX * bgRoot.effectiveWallpaperScale) - implicitWidth / 2)
                topMargin: bgRoot.movableYSpace + ((root.fixedClockPosition ? root.fixedClockY : bgRoot.clockY * bgRoot.effectiveWallpaperScale) - implicitHeight / 2)
                Behavior on leftMargin {
                    animation: Appearance.animation.elementMove.numberAnimation.createObject(this)
                }
                Behavior on topMargin {
                    animation: Appearance.animation.elementMove.numberAnimation.createObject(this)
                }
            }
            sourceComponent: Item {
                id: clock
                implicitWidth: clockColumn.implicitWidth
                implicitHeight: clockColumn.implicitHeight

                ColumnLayout {
                    id: clockColumn
                    anchors.centerIn: parent
                    spacing: 6

                    StyledText {
                        Layout.fillWidth: true
                        horizontalAlignment: bgRoot.textHorizontalAlignment
                        font {
                            family: Appearance.font.family.expressive
                            pixelSize: 90
                            weight: Font.Bold
                        }
                        color: bgRoot.colText
                        style: Text.Raised
                        styleColor: Appearance.colors.colShadow
                        text: DateTime.time
                    }
                    StyledText {
                        Layout.fillWidth: true
                        Layout.topMargin: -5
                        horizontalAlignment: bgRoot.textHorizontalAlignment
                        font {
                            family: Appearance.font.family.expressive
                            pixelSize: 20
                            weight: Font.DemiBold
                        }
                        color: bgRoot.colText
                        style: Text.Raised
                        styleColor: Appearance.colors.colShadow
                        text: DateTime.date
                        animateChange: true
                    }
                    StyledText {
                        Layout.fillWidth: true
                        horizontalAlignment: bgRoot.textHorizontalAlignment
                        font {
                            family: Appearance.font.family.expressive
                            pixelSize: 20
                            weight: Font.DemiBold
                        }
                        color: bgRoot.colText
                        style: Text.Raised
                        visible: Config.options.background.quote !== ""
                        styleColor: Appearance.colors.colShadow
                        text: Config.options.background.quote
                    }
                }

                RowLayout {
                    anchors {
                        top: clockColumn.bottom
                        left: bgRoot.textHorizontalAlignment === Text.AlignLeft ? clockColumn.left : undefined
                        right: bgRoot.textHorizontalAlignment === Text.AlignRight ? clockColumn.right : undefined
                        horizontalCenter: bgRoot.textHorizontalAlignment === Text.AlignHCenter ? clockColumn.horizontalCenter : undefined
                        topMargin: 5
                        leftMargin: -5
                        rightMargin: -5
                    }
                    opacity: GlobalStates.screenLocked ? 1 : 0
                    visible: opacity > 0
                    Behavior on opacity {
                        animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                    }
                    Item { Layout.fillWidth: bgRoot.textHorizontalAlignment !== Text.AlignLeft; implicitWidth: 1 }
                    MaterialSymbol {
                        text: "lock"
                        Layout.fillWidth: false
                        iconSize: Appearance.font.pixelSize.huge
                        color: bgRoot.colText
                        style: Text.Raised
                        styleColor: Appearance.colors.colShadow
                    }
                    StyledText {
                        Layout.fillWidth: false
                        text: "Locked"
                        color: bgRoot.colText
                        font.pixelSize: Appearance.font.pixelSize.larger
                        style: Text.Raised
                        styleColor: Appearance.colors.colShadow
                    }
                    Item { Layout.fillWidth: bgRoot.textHorizontalAlignment !== Text.AlignRight; implicitWidth: 1 }

                }
            }
        }
    }
}
