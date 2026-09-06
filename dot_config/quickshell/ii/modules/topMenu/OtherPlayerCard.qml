import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import qs.services
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Effects
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris

// A row in the Media tab's "Players" list: its own small blurred-art
// background (same MultiEffect blur + scrim + OpacityMask rounding
// technique as MediaTabPlayer's box background, scaled down to card size),
// a thumbnail, the app/player name, the current track title, and a playing
// indicator. Clicking one hands its player up to the Media tab as the
// selected player.
RippleButton {
    id: root
    required property MprisPlayer player
    property bool selected: false
    signal selectedRequested

    Layout.fillWidth: true
    implicitHeight: 64
    buttonRadius: Appearance.rounding.small

    // The card's own blurred-art background (in contentItem, below) covers
    // the whole surface, so RippleButton's own background color would never
    // be seen either way; left transparent/ripple-only. Hover feedback is
    // instead given by the scrim brightening slightly (see HoverHandler
    // below), since the ripple flash itself is now z-under the art layer.
    colBackground: "transparent"
    colBackgroundHover: "transparent"
    colRipple: Appearance.colors.colPrimary

    onClicked: root.selectedRequested()

    property var artUrl: player?.trackArtUrl
    property string artFileName: artUrl ? (Qt.md5(artUrl) + ".jpg") : ""
    property string artFilePath: artFileName ? `${Directories.coverArt}/${artFileName}` : ""
    property bool downloaded: false
    property int _bust: 0

    HoverHandler {
        id: cardHover
    }

    function refreshArt() {
        const url = root.artUrl || "";
        if (!url) {
            root.downloaded = false;
            return;
        }
        root.downloaded = false;
        thumbDownloader.targetFile = url;
        thumbDownloader.running = true;
    }
    onArtUrlChanged: root.refreshArt()
    Component.onCompleted: root.refreshArt()

    // Every card downloads the same URL MediaTabPlayer may be fetching at the
    // same moment for the selected player (they can point at the same track),
    // both writing to the same Qt.md5(artUrl)-named cache file. Two plain
    // `curl -o` writes to one path can interleave and tear whichever read
    // loses the race. Guarded here on the card's side two ways: skip the
    // fetch entirely if the file is already there (same URL means it's the
    // same content, however it got written), and when a fetch is needed,
    // write to a private per-process temp file and atomically `mv` it into
    // place, so this card's own contribution to the shared path is always
    // either absent or a complete file, never a torn one.
    Process {
        id: thumbDownloader
        property string targetFile: root.artUrl
        command: ["bash", "-c", `test -f '${root.artFilePath}' || { tmp='${root.artFilePath}.tmp.'$$; curl -fsSL '${targetFile}' -o "$tmp" && mv -f "$tmp" '${root.artFilePath}'; }`]
        onExited: code => {
            root.downloaded = (code === 0);
            root._bust++;
        }
    }

    contentItem: Item {
        anchors.fill: parent

        // Per-card blurred art background: same technique as
        // MediaTabPlayer's box background (MultiEffect blur, OpacityMask
        // rounding), scaled down, with a scrim so the foreground row stays
        // readable, and a selected-state border on top of that.
        Rectangle {
            id: cardArtBg
            anchors.fill: parent
            radius: root.buttonRadius
            color: Appearance.colors.colLayer1
            clip: true

            layer.enabled: true
            layer.effect: OpacityMask {
                maskSource: Rectangle {
                    width: cardArtBg.width
                    height: cardArtBg.height
                    radius: cardArtBg.radius
                }
            }

            Image {
                id: cardBlurredArt
                anchors.fill: parent
                visible: root.downloaded
                source: root.downloaded ? (Qt.resolvedUrl(root.artFilePath) + "?r=" + root._bust) : ""
                // Decode small regardless of the card's actual (fillWidth,
                // so potentially quite wide) on-screen size: MultiEffect
                // blurs this so hard that decoding at full card resolution
                // buys no visible detail, just a bigger texture for both
                // the decode and the blur's offscreen passes to chew
                // through, multiplied by however many cards are in the
                // list. PreserveAspectCrop still scales this small decode
                // back up to fill the card - only the blur's input
                // resolution shrinks, not the rendered size.
                sourceSize.width: 56
                sourceSize.height: 56
                fillMode: Image.PreserveAspectCrop
                cache: false
                asynchronous: true

                layer.enabled: true
                layer.effect: MultiEffect {
                    source: cardBlurredArt
                    saturation: 0.2
                    blurEnabled: true
                    blurMax: 48
                    blur: 1
                }
            }

            Rectangle {
                // Scrim: darkens the art so the name/title row reads
                // clearly; brightens slightly on hover as this card's
                // stand-in for the ripple/hover wash it visually covers.
                anchors.fill: parent
                color: ColorUtils.transparentize(Appearance.m3colors.m3scrim, cardHover.hovered ? 0.55 : 0.45)

                Behavior on color {
                    animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                }
            }

            Rectangle {
                // Selected highlight: a border, so it stays visible over
                // any art/scrim combination rather than depending on a fill
                // tint that busy album art could wash out.
                anchors.fill: parent
                radius: cardArtBg.radius
                color: "transparent"
                border.width: root.selected ? 2 : 0
                border.color: Appearance.colors.colPrimary

                Behavior on border.width {
                    animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                }
            }
        }

        RowLayout {
            anchors.fill: parent
            anchors.margins: 8
            spacing: 10

            Rectangle {
                id: thumbBackground
                Layout.preferredWidth: 44
                Layout.preferredHeight: 44
                radius: Appearance.rounding.verysmall
                color: Appearance.colors.colLayer1
                clip: true

                Image {
                    id: thumbArt
                    anchors.fill: parent
                    visible: root.downloaded && status === Image.Ready
                    source: root.downloaded ? (Qt.resolvedUrl(root.artFilePath) + "?r=" + root._bust) : ""
                    fillMode: Image.PreserveAspectCrop
                    cache: false
                    asynchronous: true
                    sourceSize.width: 44
                    sourceSize.height: 44

                    // Same retry-on-error loop PlayerControl.qml/MediaTabPlayer.qml
                    // use for their art Images: a single failed load (e.g. this
                    // card's read landing between another writer's temp-file
                    // download and its rename) shouldn't permanently pin the
                    // music_note fallback, so re-attempt with backoff before
                    // giving up.
                    property int __retry: 0
                    property int __retryMax: 6

                    onStatusChanged: {
                        if (status === Image.Error && __retry < __retryMax && source !== "") {
                            __retry++;
                            thumbArtRetryTimer.interval = Math.min(2000, 150 * Math.pow(2, __retry - 1));
                            thumbArtRetryTimer.restart();
                        } else if (status === Image.Ready) {
                            __retry = 0;
                        }
                    }

                    Timer {
                        id: thumbArtRetryTimer
                        repeat: false
                        onTriggered: {
                            root._bust++;
                        }
                    }
                }

                MaterialSymbol {
                    anchors.centerIn: parent
                    visible: !thumbArt.visible
                    iconSize: Appearance.font.pixelSize.large
                    color: Appearance.colors.colSubtext
                    text: "music_note"
                }
            }

            ColumnLayout {
                Layout.fillWidth: true
                Layout.fillHeight: true
                spacing: 1

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 4

                    StyledText {
                        Layout.fillWidth: true
                        font.pixelSize: Appearance.font.pixelSize.small
                        font.weight: Font.Medium
                        color: Appearance.colors.colOnLayer0
                        elide: Text.ElideRight
                        text: root.player?.identity || Translation.tr("Unknown Player")
                    }

                    MaterialSymbol {
                        visible: root.player?.isPlaying ?? false
                        iconSize: Appearance.font.pixelSize.small
                        fill: 1
                        color: Appearance.colors.colPrimary
                        text: "graphic_eq"
                    }
                }

                StyledText {
                    Layout.fillWidth: true
                    font.pixelSize: Appearance.font.pixelSize.smaller
                    color: Appearance.colors.colSubtext
                    elide: Text.ElideRight
                    text: StringUtils.cleanMusicTitle(root.player?.trackTitle) || Translation.tr("Unknown Title")
                }
            }
        }
    }
}
