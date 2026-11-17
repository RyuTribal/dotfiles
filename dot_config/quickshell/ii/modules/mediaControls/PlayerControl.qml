import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import Quickshell

Item { // Player instance
    id: playerController
    required property var player // a row of MprisController.meaningfulPlayers
    // axon's cached cover, written whole before the row names it (section 31, Cover art).
    property string artSource: player?.artPath ? "file://" + player.artPath : ""
    property color artDominantColor: ColorUtils.mix((colorQuantizer?.colors[0] ?? Appearance.colors.colPrimary), Appearance.colors.colPrimaryContainer, 0.8) || Appearance.m3colors.m3secondaryContainer
    property list<real> visualizerPoints: []
    property real maxVisualizerValue: 1000 // Max value in the data points
    property int visualizerSmoothing: 2 // Number of points to average for smoothing
    property real radius
    property bool seeking: false
    property real seekPreview: 0

    property bool isYouTube: {
        const url = playerController.player?.url || "";
        const id = (playerController.player?.identity || "").toLowerCase();
        return /youtube\.com|youtu\.be/.test(url) || id.includes("youtube");
    }

    property bool canPrevBtn: !isYouTube && (playerController.player?.canGoPrevious || false)
    property bool canNextBtn: !isYouTube && (playerController.player?.canGoNext || false)

    property bool shuffleAvail: (playerController.player?.canControl && playerController.player?.shuffleSupported) || false
    property bool loopAvail: (playerController.player?.canControl && playerController.player?.loopSupported) || false

    component TrackChangeButton: RippleButton {
        implicitWidth: 24
        implicitHeight: 24

        property var iconName
        colBackground: ColorUtils.transparentize(blendedColors.colSecondaryContainer, 1)
        colBackgroundHover: blendedColors.colSecondaryContainerHover
        colRipple: blendedColors.colSecondaryContainerActive

        contentItem: MaterialSymbol {
            iconSize: Appearance.font.pixelSize.huge
            fill: 1
            horizontalAlignment: Text.AlignHCenter
            color: blendedColors.colOnSecondaryContainer
            text: iconName

            Behavior on color {
                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
            }
        }
    }

    ColorQuantizer {
        id: colorQuantizer
        source: playerController.artSource
        depth: 0
        rescaleSize: 1
    }
    property bool backgroundIsDark: artDominantColor.hslLightness < 0.5
    property QtObject blendedColors: QtObject {
        property color colLayer0: ColorUtils.mix(Appearance.colors.colLayer0, artDominantColor, (backgroundIsDark && Appearance.m3colors.darkmode) ? 0.6 : 0.5)
        property color colLayer1: ColorUtils.mix(Appearance.colors.colLayer1, artDominantColor, 0.5)
        property color colOnLayer0: ColorUtils.mix(Appearance.colors.colOnLayer0, artDominantColor, 0.5)
        property color colOnLayer1: ColorUtils.mix(Appearance.colors.colOnLayer1, artDominantColor, 0.5)
        property color colSubtext: ColorUtils.mix(Appearance.colors.colOnLayer1, artDominantColor, 0.5)
        property color colPrimary: ColorUtils.mix(ColorUtils.adaptToAccent(Appearance.colors.colPrimary, artDominantColor), artDominantColor, 0.5)
        property color colPrimaryHover: ColorUtils.mix(ColorUtils.adaptToAccent(Appearance.colors.colPrimaryHover, artDominantColor), artDominantColor, 0.3)
        property color colPrimaryActive: ColorUtils.mix(ColorUtils.adaptToAccent(Appearance.colors.colPrimaryActive, artDominantColor), artDominantColor, 0.3)
        property color colSecondaryContainer: ColorUtils.mix(Appearance.m3colors.m3secondaryContainer, artDominantColor, 0.15)
        property color colSecondaryContainerHover: ColorUtils.mix(Appearance.colors.colSecondaryContainerHover, artDominantColor, 0.3)
        property color colSecondaryContainerActive: ColorUtils.mix(Appearance.colors.colSecondaryContainerActive, artDominantColor, 0.5)
        property color colOnPrimary: ColorUtils.mix(ColorUtils.adaptToAccent(Appearance.m3colors.m3onPrimary, artDominantColor), artDominantColor, 0.5)
        property color colOnSecondaryContainer: ColorUtils.mix(Appearance.m3colors.m3onSecondaryContainer, artDominantColor, 0.5)
    }

    StyledRectangularShadow {
        target: background
    }
    Rectangle { // Background
        id: background
        anchors.fill: parent
        anchors.margins: Appearance.sizes.elevationMargin
        color: blendedColors.colLayer0
        radius: playerController.radius

        layer.enabled: true
        layer.effect: OpacityMask {
            maskSource: Rectangle {
                width: background.width
                height: background.height
                radius: background.radius
            }
        }

        Image {
            id: blurredArt
            anchors.fill: parent
            source: playerController.artSource
            sourceSize.width: background.width
            sourceSize.height: background.height
            fillMode: Image.PreserveAspectCrop
            antialiasing: true
            asynchronous: true

            layer.enabled: true
            layer.effect: MultiEffect {
                source: blurredArt
                saturation: 0.2
                blurEnabled: true
                blurMax: 100
                blur: 1
            }

            Rectangle {
                anchors.fill: parent
                color: ColorUtils.transparentize(blendedColors.colLayer0, 0.3)
                radius: playerController.radius
            }
        }

        WaveVisualizer {
            id: visualizerCanvas
            anchors.fill: parent
            live: playerController.player?.isPlaying
            points: playerController.visualizerPoints
            maxVisualizerValue: playerController.maxVisualizerValue
            smoothing: playerController.visualizerSmoothing
            color: blendedColors.colPrimary
        }

        RowLayout {
            anchors.fill: parent
            anchors.margins: root.contentPadding
            spacing: 15

            Rectangle { // Art background
                id: artBackground
                Layout.fillHeight: true
                implicitWidth: height
                radius: root.artRounding
                color: ColorUtils.transparentize(blendedColors.colLayer1, 0.5)

                layer.enabled: true
                layer.effect: OpacityMask {
                    maskSource: Rectangle {
                        width: artBackground.width
                        height: artBackground.height
                        radius: artBackground.radius
                    }
                }

                Image { // Art image
                    id: mediaArt
                    property int size: parent.height
                    anchors.fill: parent

                    source: playerController.artSource
                    fillMode: Image.PreserveAspectCrop
                    antialiasing: true
                    asynchronous: true

                    width: size
                    height: size
                    sourceSize.width: size
                    sourceSize.height: size
                }
            }

            ColumnLayout {
                // Info & controls
                Layout.fillHeight: true
                spacing: 2

                StyledText {
                    id: trackTitle
                    Layout.fillWidth: true
                    font.pixelSize: Appearance.font.pixelSize.large
                    color: blendedColors.colOnLayer0
                    elide: Text.ElideRight
                    text: StringUtils.cleanMusicTitle(playerController.player?.trackTitle) || "Untitled"
                    animateChange: true
                    animationDistanceX: 6
                    animationDistanceY: 0
                }
                StyledText {
                    id: trackArtist
                    Layout.fillWidth: true
                    font.pixelSize: Appearance.font.pixelSize.smaller
                    color: blendedColors.colSubtext
                    elide: Text.ElideRight
                    text: playerController.player?.trackArtist
                    animateChange: true
                    animationDistanceX: 6
                    animationDistanceY: 0
                }
                Item {
                    Layout.fillHeight: true
                }
                Item {
                    Layout.fillWidth: true
                    implicitHeight: trackTime.implicitHeight + sliderRow.implicitHeight

                    StyledText {
                        id: trackTime
                        anchors.bottom: sliderRow.top
                        anchors.bottomMargin: 5
                        anchors.left: parent.left
                        font.pixelSize: Appearance.font.pixelSize.small
                        color: blendedColors.colSubtext
                        elide: Text.ElideRight
                        text: `${StringUtils.friendlyTimeForSeconds(MprisController.positionOf(playerController.player, Backend.now))} / ${StringUtils.friendlyTimeForSeconds(playerController.player?.length)}`
                    }
                    RowLayout {
                        id: sliderRow
                        anchors {
                            bottom: parent.bottom
                            left: parent.left
                            right: parent.right
                        }
                        TrackChangeButton {
                            // prev
                            iconName: "skip_previous"
                            enabled: playerController.canPrevBtn
                            opacity: enabled ? 1.0 : 0.35
                            onClicked: if (enabled)
                                MprisController.previous(playerController.player.id)
                        }
                        Item {
                            id: progressBarContainer
                            Layout.fillWidth: true
                            implicitHeight: progressBar.implicitHeight

                            StyledProgressBar {
                                id: progressBar
                                anchors.fill: parent
                                highlightColor: blendedColors.colPrimary
                                trackColor: blendedColors.colSecondaryContainer

                                // show live position unless we're dragging; then show the preview
                                value: {
                                    const len = playerController.player?.length || 0;
                                    const pos = playerController.seeking ? playerController.seekPreview : MprisController.positionOf(playerController.player, Backend.now);
                                    return len > 0 ? pos / len : 0;
                                }

                                sperm: playerController.player?.isPlaying
                            }

                            // Seek by clicking/dragging (and with mouse wheel for fine steps)
                            MouseArea {
                                anchors.fill: progressBar
                                cursorShape: Qt.PointingHandCursor
                                enabled: playerController.player?.canSeek && playerController.player?.positionSupported

                                function posFromX(x) {
                                    const len = playerController.player?.length || 0;
                                    if (len <= 0)
                                        return 0;
                                    const ratio = Math.min(1, Math.max(0, x / width));
                                    return ratio * len;
                                }

                                onPressed: mouse => {
                                    playerController.seeking = true;
                                    playerController.seekPreview = posFromX(mouse.x);
                                }
                                onPositionChanged: mouse => {
                                    if (pressed)
                                        playerController.seekPreview = posFromX(mouse.x);
                                }
                                onReleased: mouse => {
                                    const newPos = posFromX(mouse.x);
                                    playerController.seeking = false;
                                    if (playerController.player?.canSeek && playerController.player?.positionSupported)
                                        MprisController.setPosition(playerController.player.id, newPos);
                                }
                                onWheel: wheel => {
                                    if (!(playerController.player?.canSeek && playerController.player?.positionSupported))
                                        return;
                                    // ~5s per wheel notch; use MPRIS relative seek
                                    const step = 5;
                                    const delta = wheel.angleDelta.y !== 0 ? wheel.angleDelta.y : wheel.angleDelta.x;
                                    MprisController.seek(playerController.player.id, delta > 0 ? step : -step);
                                    wheel.accepted = true;
                                }
                            }
                        }
                        TrackChangeButton {
                            // next
                            iconName: "skip_next"
                            enabled: playerController.canNextBtn
                            opacity: enabled ? 1.0 : 0.35
                            onClicked: if (enabled)
                                MprisController.next(playerController.player.id)
                        }
                    }

                    RowLayout {
                        id: extraControls
                        anchors.right: playPauseButton.left
                        anchors.rightMargin: 8
                        anchors.bottom: playPauseButton.bottom
                        spacing: 6

                        // Shuffle
                        TrackChangeButton {

                            contentItem: MaterialSymbol {
                                iconSize: Appearance.font.pixelSize.huge
                                fill: 1
                                horizontalAlignment: Text.AlignHCenter
                                color: !playerController.player?.shuffle ? blendedColors.colOnPrimary : blendedColors.colOnSecondaryContainer
                                text: "shuffle"

                                Behavior on color {
                                    animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                                }
                            }
                            onClicked: if (playerController.shuffleAvail)
                                MprisController.setShuffle(playerController.player.id, !playerController.player.shuffle)
                        }

                        // Repeat (cycle None → Playlist → Track)
                        RippleButton {

                            contentItem: MaterialSymbol {
                                iconSize: Appearance.font.pixelSize.huge
                                fill: 1
                                horizontalAlignment: Text.AlignHCenter
                                color: playerController.player?.loopState === "none" ? blendedColors.colOnPrimary : blendedColors.colOnSecondaryContainer
                                text: playerController.player?.loopState === "track" ? "repeat_one" : "repeat"

                                Behavior on color {
                                    animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                                }
                            }

                            onClicked: {
                                if (!playerController.loopAvail)
                                    return;
                                const s = playerController.player.loopState;
                                const next = (s === "none") ? "playlist" : (s === "playlist") ? "track" : "none";
                                MprisController.setLoopState(playerController.player.id, next);
                            }
                        }
                    }

                    RippleButton {
                        id: playPauseButton
                        anchors.right: parent.right
                        anchors.bottom: sliderRow.top
                        anchors.bottomMargin: 5
                        property real size: 44
                        implicitWidth: size
                        implicitHeight: size
                        onClicked: MprisController.togglePlaying(playerController.player.id)

                        buttonRadius: playerController.player?.isPlaying ? Appearance?.rounding.normal : size / 2
                        colBackground: playerController.player?.isPlaying ? blendedColors.colPrimary : blendedColors.colSecondaryContainer
                        colBackgroundHover: playerController.player?.isPlaying ? blendedColors.colPrimaryHover : blendedColors.colSecondaryContainerHover
                        colRipple: playerController.player?.isPlaying ? blendedColors.colPrimaryActive : blendedColors.colSecondaryContainerActive

                        contentItem: MaterialSymbol {
                            iconSize: Appearance.font.pixelSize.huge
                            fill: 1
                            horizontalAlignment: Text.AlignHCenter
                            color: playerController.player?.isPlaying ? blendedColors.colOnPrimary : blendedColors.colOnSecondaryContainer
                            text: playerController.player?.isPlaying ? "pause" : "play_arrow"

                            Behavior on color {
                                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                            }
                        }
                    }
                }
            }
        }
    }
}
