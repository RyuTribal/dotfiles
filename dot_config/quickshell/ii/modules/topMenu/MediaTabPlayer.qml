import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import Quickshell

// The Media tab's main player card. The cover art and the adaptive
// color-extraction mechanism (ColorQuantizer -> artDominantColor ->
// blendedColors) are PlayerControl.qml's, reused exactly, unchanged.
// PlayerControl.qml itself is left untouched - this is a sibling, not a
// subclass, since PlayerControl reads two properties (contentPadding,
// artRounding) unqualified off an enclosing `id: root` Scope that only
// exists in MediaControls.qml. Everything else here (layout, circular art +
// PulseRing, pill control cluster) is a from-scratch redesign: batch 6's
// version mirrored PlayerControl's own compact RowLayout almost verbatim,
// just scaled up, and the result read as a stretched popup rather than a
// tab-native player, per user feedback.
Item {
    id: mediaTabPlayer
    required property var player // a row of MprisController.meaningfulPlayers
    // axon's cached cover, written whole before the row names it (section 31, Cover art).
    property string artSource: player?.artPath ? "file://" + player.artPath : ""
    property color artDominantColor: ColorUtils.mix((colorQuantizer?.colors[0] ?? Appearance.colors.colPrimary), Appearance.colors.colPrimaryContainer, 0.8) || Appearance.m3colors.m3secondaryContainer
    property list<real> visualizerPoints: []
    property real maxVisualizerValue: 1000 // Max value in the data points
    property int visualizerSmoothing: 2 // Number of points to average for smoothing
    property real radius: Appearance.rounding.normal
    property bool seeking: false
    property real seekPreview: 0

    // Set by MediaTab.qml to root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
    // - the same gate its spectrum topic uses - so the position tick below
    // doesn't run while this tab isn't the one visible (or the menu is
    // closed) even though this Item, like the rest of the SwipeView's pages,
    // stays alive and instantiated regardless.
    property bool isTabActive: false

    // The position shown, in seconds: axon's anchor for the player
    // extrapolated to now (MprisController.positionOf). Every row axon
    // publishes carries a fresh anchor, so it is recomputed on each new row,
    // when the tab turns active, and every 100 ms while it plays.
    property real displayPosition: 0

    function syncPosition() {
        mediaTabPlayer.displayPosition = MprisController.positionOf(mediaTabPlayer.player, Date.now());
    }
    onPlayerChanged: mediaTabPlayer.syncPosition()
    Component.onCompleted: mediaTabPlayer.syncPosition()
    onIsTabActiveChanged: if (mediaTabPlayer.isTabActive)
        mediaTabPlayer.syncPosition()

    // Sizing tokens for this box. Art is circular now: artSize is the
    // circle's diameter, and the PulseRing sits in the ring of pixels
    // between that circle and ringMaxAmplitude further out.
    property real contentPadding: 14
    property real artSize: 104
    property real ringMaxAmplitude: 32
    property real shuffleLoopSize: 36
    property real prevNextSize: 44
    property real playPauseSize: 60

    property bool isYouTube: {
        const url = mediaTabPlayer.player?.url || "";
        const id = (mediaTabPlayer.player?.identity || "").toLowerCase();
        return /youtube\.com|youtu\.be/.test(url) || id.includes("youtube");
    }

    property bool canPrevBtn: !isYouTube && (mediaTabPlayer.player?.canGoPrevious || false)
    property bool canNextBtn: !isYouTube && (mediaTabPlayer.player?.canGoNext || false)

    property bool shuffleAvail: (mediaTabPlayer.player?.canControl && mediaTabPlayer.player?.shuffleSupported) || false
    property bool loopAvail: (mediaTabPlayer.player?.canControl && mediaTabPlayer.player?.loopSupported) || false

    component TrackChangeButton: RippleButton {
        property var iconName
        property real size: mediaTabPlayer.shuffleLoopSize
        property real iconScale: 1
        implicitWidth: size
        implicitHeight: size

        colBackground: ColorUtils.transparentize(blendedColors.colSecondaryContainer, 1)
        colBackgroundHover: blendedColors.colSecondaryContainerHover
        colRipple: blendedColors.colSecondaryContainerActive

        contentItem: MaterialSymbol {
            iconSize: Appearance.font.pixelSize.hugeass * iconScale
            fill: 1
            horizontalAlignment: Text.AlignHCenter
            color: blendedColors.colOnSecondaryContainer
            text: iconName

            Behavior on color {
                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
            }
        }
    }

    Timer {
        // The tick that makes the seek bar and time label move
        // continuously. Gated on both "tab actually visible" and "playing" -
        // a paused position doesn't move, and a non-visible/closed tab has
        // nothing to show.
        running: mediaTabPlayer.isTabActive && (mediaTabPlayer.player?.isPlaying ?? false)
        interval: 100
        repeat: true
        onTriggered: mediaTabPlayer.syncPosition()
    }

    ColorQuantizer {
        id: colorQuantizer
        source: mediaTabPlayer.artSource
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
    Rectangle { // Background - blurred art fills this box only (batch 7
                // order 1: this Rectangle IS the main player box, so the
                // blur below has always been box-scoped; batch 6's bug was
                // MediaTab.qml layering a second, whole-tab-covering copy of
                // this same treatment behind everything, now removed there).
        id: background
        anchors.fill: parent
        anchors.margins: Appearance.sizes.elevationMargin
        color: blendedColors.colLayer0
        radius: mediaTabPlayer.radius

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
            source: mediaTabPlayer.artSource
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
                color: ColorUtils.transparentize(blendedColors.colLayer0, 0.25)
                radius: mediaTabPlayer.radius
            }
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: mediaTabPlayer.contentPadding
            spacing: 8

            Item {
                Layout.fillHeight: true
            }

            // Circular art + audio-reactive pulse ring, centered.
            Item {
                id: artCluster
                Layout.alignment: Qt.AlignHCenter
                implicitWidth: mediaTabPlayer.artSize + (mediaTabPlayer.ringMaxAmplitude + 6) * 2
                implicitHeight: implicitWidth

                PulseRing {
                    anchors.fill: parent
                    points: mediaTabPlayer.visualizerPoints
                    maxVisualizerValue: mediaTabPlayer.maxVisualizerValue
                    smoothing: mediaTabPlayer.visualizerSmoothing
                    live: mediaTabPlayer.player?.isPlaying ?? false
                    color: blendedColors.colPrimary
                    innerRadius: mediaTabPlayer.artSize / 2
                    maxAmplitude: mediaTabPlayer.ringMaxAmplitude
                }

                Rectangle { // Circular art mask
                    id: artCircle
                    anchors.centerIn: parent
                    width: mediaTabPlayer.artSize
                    height: mediaTabPlayer.artSize
                    radius: width / 2
                    color: ColorUtils.transparentize(blendedColors.colLayer1, 0.5)

                    layer.enabled: true
                    layer.effect: OpacityMask {
                        maskSource: Rectangle {
                            width: artCircle.width
                            height: artCircle.height
                            radius: width / 2
                        }
                    }

                    Image { // Art image
                        id: mediaArt
                        anchors.fill: parent

                        source: mediaTabPlayer.artSource
                        fillMode: Image.PreserveAspectCrop
                        antialiasing: true
                        asynchronous: true

                        sourceSize.width: mediaTabPlayer.artSize
                        sourceSize.height: mediaTabPlayer.artSize
                    }
                }
            }

            StyledText {
                id: trackTitle
                Layout.fillWidth: true
                Layout.topMargin: 4
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: Appearance.font.pixelSize.hugeass
                font.weight: Font.Bold
                color: blendedColors.colOnLayer0
                elide: Text.ElideRight
                text: StringUtils.cleanMusicTitle(mediaTabPlayer.player?.trackTitle) || "Untitled"
                animateChange: true
                animationDistanceX: 0
                animationDistanceY: 6
            }
            StyledText {
                id: trackArtist
                Layout.fillWidth: true
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: Appearance.font.pixelSize.large
                color: blendedColors.colSubtext
                elide: Text.ElideRight
                text: mediaTabPlayer.player?.trackArtist ?? ""
                animateChange: true
                animationDistanceX: 0
                animationDistanceY: 6
            }

            Item {
                Layout.fillHeight: true
            }

            // Seek row: time - progress bar - time.
            RowLayout {
                Layout.fillWidth: true
                spacing: 10

                StyledText {
                    font.pixelSize: Appearance.font.pixelSize.small
                    color: blendedColors.colSubtext
                    text: StringUtils.friendlyTimeForSeconds(mediaTabPlayer.seeking ? mediaTabPlayer.seekPreview : mediaTabPlayer.displayPosition)
                }

                Item {
                    id: progressBarContainer
                    Layout.fillWidth: true
                    implicitHeight: progressBar.implicitHeight

                    StyledProgressBar {
                        id: progressBar
                        anchors.fill: parent
                        valueBarHeight: 6
                        highlightColor: blendedColors.colPrimary
                        trackColor: blendedColors.colSecondaryContainer

                        // show the interpolated live position unless we're dragging; then show the preview
                        value: {
                            const len = mediaTabPlayer.player?.length || 0;
                            const pos = mediaTabPlayer.seeking ? mediaTabPlayer.seekPreview : mediaTabPlayer.displayPosition;
                            return len > 0 ? pos / len : 0;
                        }

                        sperm: mediaTabPlayer.player?.isPlaying ?? false
                    }

                    // Seek by clicking/dragging (and with mouse wheel for fine steps)
                    MouseArea {
                        anchors.fill: progressBar
                        cursorShape: Qt.PointingHandCursor
                        enabled: (mediaTabPlayer.player?.canSeek ?? false) && (mediaTabPlayer.player?.positionSupported ?? false)
                        // This card lives inside TopMenuContent's SwipeView, which is
                        // interactive (swipe-to-change-tab). Without this, a horizontal
                        // drag here gets stolen by the SwipeView's own drag handling
                        // partway through and switches tabs instead of seeking.
                        // preventStealing keeps the grab on this MouseArea once pressed,
                        // which is the standard Qt Quick mechanism for exactly this
                        // conflict (a Flickable/SwipeView-like ancestor filtering a
                        // child's mouse events to pan/swipe). Scoped to just this
                        // MouseArea rather than making the whole SwipeView
                        // non-interactive, so swiping tabs elsewhere on the page (and
                        // the other four tabs) is unaffected.
                        preventStealing: true

                        function posFromX(x) {
                            const len = mediaTabPlayer.player?.length || 0;
                            if (len <= 0)
                                return 0;
                            const ratio = Math.min(1, Math.max(0, x / width));
                            return ratio * len;
                        }

                        onPressed: mouse => {
                            mediaTabPlayer.seeking = true;
                            mediaTabPlayer.seekPreview = posFromX(mouse.x);
                        }
                        onPositionChanged: mouse => {
                            if (pressed)
                                mediaTabPlayer.seekPreview = posFromX(mouse.x);
                        }
                        onReleased: mouse => {
                            const newPos = posFromX(mouse.x);
                            mediaTabPlayer.seeking = false;
                            if (mediaTabPlayer.player?.canSeek && mediaTabPlayer.player?.positionSupported)
                                MprisController.setPosition(mediaTabPlayer.player.id, newPos);
                            // Show the seek target at once rather than the
                            // pre-seek position until axon's re-read anchor arrives.
                            mediaTabPlayer.displayPosition = newPos;
                        }
                        onWheel: wheel => {
                            if (!(mediaTabPlayer.player?.canSeek && mediaTabPlayer.player?.positionSupported))
                                return;
                            // ~5s per wheel notch; use MPRIS relative seek
                            const step = 5;
                            const delta = wheel.angleDelta.y !== 0 ? wheel.angleDelta.y : wheel.angleDelta.x;
                            MprisController.seek(mediaTabPlayer.player.id, delta > 0 ? step : -step);
                            wheel.accepted = true;
                        }
                    }
                }

                StyledText {
                    font.pixelSize: Appearance.font.pixelSize.small
                    color: blendedColors.colSubtext
                    text: StringUtils.friendlyTimeForSeconds(mediaTabPlayer.player?.length)
                }
            }

            // Pill-shaped control cluster: shuffle - prev - play/pause - next - loop,
            // all riding one rounded surface so they read as a single unit
            // instead of five loose buttons, with play/pause standing out by
            // size and fill color.
            Rectangle {
                id: controlPill
                Layout.alignment: Qt.AlignHCenter
                Layout.topMargin: 4
                implicitWidth: controlRow.implicitWidth + 32
                implicitHeight: mediaTabPlayer.playPauseSize + 16
                radius: height / 2
                color: ColorUtils.transparentize(blendedColors.colLayer1, 0.35)

                RowLayout {
                    id: controlRow
                    anchors.centerIn: parent
                    spacing: 12

                    TrackChangeButton {
                        iconName: "shuffle"
                        size: mediaTabPlayer.shuffleLoopSize
                        iconScale: 0.85
                        enabled: mediaTabPlayer.shuffleAvail
                        opacity: enabled ? 1.0 : 0.35
                        contentItem: MaterialSymbol {
                            iconSize: Appearance.font.pixelSize.hugeass * 0.85
                            fill: 1
                            horizontalAlignment: Text.AlignHCenter
                            color: !mediaTabPlayer.player?.shuffle ? blendedColors.colOnSecondaryContainer : blendedColors.colPrimary
                            text: "shuffle"

                            Behavior on color {
                                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                            }
                        }
                        onClicked: if (mediaTabPlayer.shuffleAvail)
                            MprisController.setShuffle(mediaTabPlayer.player.id, !mediaTabPlayer.player.shuffle)
                    }

                    TrackChangeButton {
                        iconName: "skip_previous"
                        size: mediaTabPlayer.prevNextSize
                        iconScale: 1.2
                        enabled: mediaTabPlayer.canPrevBtn
                        opacity: enabled ? 1.0 : 0.35
                        onClicked: if (enabled)
                            MprisController.previous(mediaTabPlayer.player.id)
                    }

                    RippleButton {
                        id: playPauseButton
                        implicitWidth: mediaTabPlayer.playPauseSize
                        implicitHeight: mediaTabPlayer.playPauseSize
                        onClicked: MprisController.togglePlaying(mediaTabPlayer.player.id)

                        buttonRadius: mediaTabPlayer.player?.isPlaying ? Appearance.rounding.normal : implicitWidth / 2
                        colBackground: mediaTabPlayer.player?.isPlaying ? blendedColors.colPrimary : blendedColors.colSecondaryContainer
                        colBackgroundHover: mediaTabPlayer.player?.isPlaying ? blendedColors.colPrimaryHover : blendedColors.colSecondaryContainerHover
                        colRipple: mediaTabPlayer.player?.isPlaying ? blendedColors.colPrimaryActive : blendedColors.colSecondaryContainerActive

                        contentItem: MaterialSymbol {
                            iconSize: Appearance.font.pixelSize.hugeass * 1.6
                            fill: 1
                            horizontalAlignment: Text.AlignHCenter
                            color: mediaTabPlayer.player?.isPlaying ? blendedColors.colOnPrimary : blendedColors.colOnSecondaryContainer
                            text: mediaTabPlayer.player?.isPlaying ? "pause" : "play_arrow"

                            Behavior on color {
                                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                            }
                        }
                    }

                    TrackChangeButton {
                        iconName: "skip_next"
                        size: mediaTabPlayer.prevNextSize
                        iconScale: 1.2
                        enabled: mediaTabPlayer.canNextBtn
                        opacity: enabled ? 1.0 : 0.35
                        onClicked: if (enabled)
                            MprisController.next(mediaTabPlayer.player.id)
                    }

                    TrackChangeButton {
                        size: mediaTabPlayer.shuffleLoopSize
                        iconScale: 0.85
                        enabled: mediaTabPlayer.loopAvail
                        opacity: enabled ? 1.0 : 0.35
                        contentItem: MaterialSymbol {
                            iconSize: Appearance.font.pixelSize.hugeass * 0.85
                            fill: 1
                            horizontalAlignment: Text.AlignHCenter
                            color: mediaTabPlayer.player?.loopState === "none" ? blendedColors.colOnSecondaryContainer : blendedColors.colPrimary
                            text: mediaTabPlayer.player?.loopState === "track" ? "repeat_one" : "repeat"

                            Behavior on color {
                                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                            }
                        }
                        onClicked: {
                            if (!mediaTabPlayer.loopAvail)
                                return;
                            const s = mediaTabPlayer.player.loopState;
                            const next = (s === "none") ? "playlist" : (s === "playlist") ? "track" : "none";
                            MprisController.setLoopState(mediaTabPlayer.player.id, next);
                        }
                    }
                }
            }
        }
    }
}
