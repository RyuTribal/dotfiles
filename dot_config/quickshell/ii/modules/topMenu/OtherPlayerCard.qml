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

// A row in the Media tab's "Players" list: its own small blurred-art
// background (same MultiEffect blur + scrim + OpacityMask rounding
// technique as MediaTabPlayer's box background, scaled down to card size),
// a thumbnail, the app/player name, the current track title, and a playing
// indicator. Clicking one hands its player up to the Media tab as the
// selected player.
RippleButton {
    id: root
    required property var player // a row of MprisController.meaningfulPlayers
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

    // axon's cached cover, written whole before the row names it (section 31, Cover art).
    property string artSource: player?.artPath ? "file://" + player.artPath : ""

    HoverHandler {
        id: cardHover
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
                visible: root.artSource !== ""
                source: root.artSource
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
                    visible: root.artSource !== "" && status === Image.Ready
                    source: root.artSource
                    fillMode: Image.PreserveAspectCrop
                    asynchronous: true
                    sourceSize.width: 44
                    sourceSize.height: 44
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
