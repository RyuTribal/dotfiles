import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris

// The Media tab's main player card. Art download and the adaptive
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
    required property MprisPlayer player
    property var artUrl: player?.trackArtUrl
    property string artDownloadLocation: Directories.coverArt
    property string artFileName: Qt.md5(artUrl) + ".jpg"
    property string artFilePath: `${artDownloadLocation}/${artFileName}`
    property color artDominantColor: ColorUtils.mix((colorQuantizer?.colors[0] ?? Appearance.colors.colPrimary), Appearance.colors.colPrimaryContainer, 0.8) || Appearance.m3colors.m3secondaryContainer
    property bool downloaded: false
    property list<real> visualizerPoints: []
    property real maxVisualizerValue: 1000 // Max value in the data points
    property int visualizerSmoothing: 2 // Number of points to average for smoothing
    property real radius: Appearance.rounding.normal
    property bool seeking: false
    property real seekPreview: 0

    // Set by MediaTab.qml to root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
    // - the same gate the cava Process there uses - so the position
    // interpolation timers below don't run while this tab isn't the one
    // visible (or the menu is closed) even though this Item, like the rest
    // of the SwipeView's pages, stays alive and instantiated regardless.
    property bool isTabActive: false

    // MPRIS position is a poll-on-demand value, not a pushed stream: without
    // this, the displayed position only advances once per positionChanged()
    // poll and visibly steps/lags rather than moving continuously. Fixed by
    // interpolating between polls: positionAnchor/positionAnchorTime capture
    // a real (position, wall-clock-time) pair whenever we resync, and
    // displayPosition is computed from elapsed time * playback rate off that
    // anchor every interpolation tick, then used everywhere the UI shows
    // "now" instead of directly reading player.position.
    property real positionAnchor: 0
    property real positionAnchorTime: 0
    property real displayPosition: 0

    function resyncPosition() {
        mediaTabPlayer.positionAnchor = mediaTabPlayer.player?.position || 0;
        mediaTabPlayer.positionAnchorTime = Date.now();
        mediaTabPlayer.displayPosition = mediaTabPlayer.positionAnchor;
    }
    onPlayerChanged: mediaTabPlayer.resyncPosition()
    Component.onCompleted: mediaTabPlayer.resyncPosition()
    onIsTabActiveChanged: if (mediaTabPlayer.isTabActive)
        mediaTabPlayer.resyncPosition()
    // Both interpolation timers are gated off while the tab isn't current
    // (see isTabActive above), so positionAnchor/positionAnchorTime just
    // freeze at whatever they were when it last went inactive - playback
    // keeps advancing underneath, unseen. Without this, returning to the
    // tab would make the fast tick compute elapsed time from that stale
    // anchor and show a sudden forward jump (the whole away-duration) before
    // correcting itself on the next 400ms poll. Resyncing immediately on
    // reactivation means displayPosition is correct from the first tick.

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
        const url = (mediaTabPlayer.player?.metadata["xesam:url"] || "").toString();
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
        // Slow poll: asks the MPRIS backend for a fresh position and
        // resyncs the interpolation anchor to it, so drift between the
        // real position and displayPosition can't accumulate over a long
        // playback session. Gated on both "tab actually visible" and
        // "playing" - a paused position doesn't move, and a
        // non-visible/closed tab has nothing to show.
        running: mediaTabPlayer.isTabActive && (mediaTabPlayer.player?.playbackState == MprisPlaybackState.Playing)
        interval: 400
        repeat: true
        onTriggered: {
            mediaTabPlayer.player.positionChanged();
            mediaTabPlayer.resyncPosition();
        }
    }

    Timer {
        // Fast tick: recomputes displayPosition from the anchor via elapsed
        // wall-clock time, which is what actually makes the seek bar/time
        // label move continuously instead of stepping once per slow-poll
        // tick. Same gating as the slow poll above for CPU sanity.
        running: mediaTabPlayer.isTabActive && (mediaTabPlayer.player?.playbackState == MprisPlaybackState.Playing)
        interval: 100
        repeat: true
        onTriggered: {
            const rate = mediaTabPlayer.player?.rate || 1;
            const elapsed = (Date.now() - mediaTabPlayer.positionAnchorTime) / 1000;
            const len = mediaTabPlayer.player?.length || 0;
            let next = mediaTabPlayer.positionAnchor + elapsed * rate;
            if (len > 0)
                next = Math.min(next, len);
            mediaTabPlayer.displayPosition = Math.max(0, next);
        }
    }

    function refreshArt() {
        const url = mediaTabPlayer.player?.trackArtUrl || "";
        if (!url)
            return;
        downloaded = false;      // important: gate images/quantizer until new file lands
        coverArtDownloader.targetFile = url;
        coverArtDownloader.running = true;
    }
    Connections {
        target: mediaTabPlayer.player
        function onTrackChanged() {
            mediaTabPlayer.refreshArt();
            mediaTabPlayer.resyncPosition();
        }          // fires every song
        function onTrackArtUrlChanged() {
            mediaTabPlayer.refreshArt();
        }    // some players change URL too
        function onUniqueIdChanged() {
            mediaTabPlayer.refreshArt();
        }
        function onPositionChanged() {
            // Catches seeks/position pushes from outside this card (e.g. the
            // popup, or the player's own UI) so the interpolation anchor
            // doesn't keep coasting from a now-stale point.
            mediaTabPlayer.resyncPosition();
        }
        function onPlaybackStateChanged() {
            // Freezes displayPosition at the true value the instant
            // playback pauses/stops, rather than leaving it wherever the
            // interpolation timer last left it.
            mediaTabPlayer.resyncPosition();
        }
    }

    onArtUrlChanged: mediaTabPlayer.refreshArt()

    Process {
        id: coverArtDownloader
        property string targetFile: mediaTabPlayer.artUrl ?? ""
        // overwrite every time
        command: ["bash", "-c", `curl -fsSL '${targetFile}' -o '${mediaTabPlayer.artFilePath}'`]
        onExited: code => mediaTabPlayer.downloaded = (code === 0)
    }
    property int _quantBust: 0
    onDownloadedChanged: if (downloaded)
        _quantBust++   // bump when new art is in place

    ColorQuantizer {
        id: colorQuantizer
        // cache-bust just like the Image
        source: mediaTabPlayer.downloaded ? Qt.resolvedUrl(mediaTabPlayer.artFilePath) + "?r=" + mediaTabPlayer._quantBust : ""
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
            source: mediaTabPlayer.downloaded ? (Qt.resolvedUrl(mediaTabPlayer.artFilePath) + "?r=" + __bust) : ""
            sourceSize.width: background.width
            sourceSize.height: background.height
            fillMode: Image.PreserveAspectCrop
            cache: false
            antialiasing: true
            asynchronous: true

            property int __bust: 0
            property int __retry: 0
            property int __retryMax: 6

            layer.enabled: true
            layer.effect: MultiEffect {
                source: blurredArt
                saturation: 0.2
                blurEnabled: true
                blurMax: 100
                blur: 1
            }

            onStatusChanged: {
                if (status === Image.Error && __retry < __retryMax && source !== "") {
                    __retry++;
                    retryTimer.interval = Math.min(2000, 150 * Math.pow(2, __retry - 1));
                    retryTimer.restart();
                } else if (status === Image.Ready) {
                    __retry = 0;
                }
            }

            Timer {
                id: retryTimer
                repeat: false
                onTriggered: {
                    // force Qt to reload by changing the URL string
                    blurredArt.__bust++;
                }
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

                        property int __bust: 0
                        property int __retry: 0
                        property int __retryMax: 6

                        source: mediaTabPlayer.downloaded ? (Qt.resolvedUrl(mediaTabPlayer.artFilePath) + "?r=" + __bust) : ""
                        fillMode: Image.PreserveAspectCrop
                        cache: false
                        antialiasing: true
                        asynchronous: true

                        onStatusChanged: {
                            if (status === Image.Error && __retry < __retryMax && source !== "") {
                                __retry++;
                                mediaArtRetryTimer.interval = Math.min(2000, 150 * Math.pow(2, __retry - 1));
                                mediaArtRetryTimer.restart();
                            } else if (status === Image.Ready) {
                                __retry = 0;
                            }
                        }

                        Timer {
                            id: mediaArtRetryTimer
                            repeat: false
                            onTriggered: {
                                mediaArt.__bust++;
                            }
                        }

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
                            if (mediaTabPlayer.player?.canSeek && mediaTabPlayer.player?.positionSupported) {
                                // Set absolute position
                                mediaTabPlayer.player.position = newPos;
                                // force a UI tick so the bar updates immediately
                                mediaTabPlayer.player.positionChanged();
                            }
                            // Snap the interpolation anchor to the seek target
                            // immediately rather than waiting for the next slow-poll
                            // tick, so the bar doesn't visibly creep back toward the
                            // pre-seek position for up to 400ms after release.
                            mediaTabPlayer.positionAnchor = newPos;
                            mediaTabPlayer.positionAnchorTime = Date.now();
                            mediaTabPlayer.displayPosition = newPos;
                        }
                        onWheel: wheel => {
                            if (!(mediaTabPlayer.player?.canSeek && mediaTabPlayer.player?.positionSupported))
                                return;
                            // ~5s per wheel notch; use MPRIS relative seek
                            const step = 5;
                            const delta = wheel.angleDelta.y !== 0 ? wheel.angleDelta.y : wheel.angleDelta.x;
                            mediaTabPlayer.player.seek(delta > 0 ? step : -step);
                            mediaTabPlayer.player.positionChanged();
                            mediaTabPlayer.resyncPosition();
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
                            mediaTabPlayer.player.shuffle = !mediaTabPlayer.player.shuffle
                    }

                    TrackChangeButton {
                        iconName: "skip_previous"
                        size: mediaTabPlayer.prevNextSize
                        iconScale: 1.2
                        enabled: mediaTabPlayer.canPrevBtn
                        opacity: enabled ? 1.0 : 0.35
                        onClicked: if (enabled)
                            mediaTabPlayer.player.previous()
                    }

                    RippleButton {
                        id: playPauseButton
                        implicitWidth: mediaTabPlayer.playPauseSize
                        implicitHeight: mediaTabPlayer.playPauseSize
                        onClicked: mediaTabPlayer.player.togglePlaying()

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
                            mediaTabPlayer.player.next()
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
                            color: mediaTabPlayer.player?.loopState === MprisLoopState.None ? blendedColors.colOnSecondaryContainer : blendedColors.colPrimary
                            text: mediaTabPlayer.player?.loopState === MprisLoopState.Track ? "repeat_one" : "repeat"

                            Behavior on color {
                                animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                            }
                        }
                        onClicked: {
                            if (!mediaTabPlayer.loopAvail)
                                return;
                            const s = mediaTabPlayer.player.loopState;
                            const next = (s === MprisLoopState.None) ? MprisLoopState.Playlist : (s === MprisLoopState.Playlist) ? MprisLoopState.Track : MprisLoopState.None;
                            mediaTabPlayer.player.loopState = next;
                        }
                    }
                }
            }
        }
    }
}
