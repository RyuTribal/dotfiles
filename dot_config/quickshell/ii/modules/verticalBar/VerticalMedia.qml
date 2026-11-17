import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import qs.modules.common.functions

import QtQuick
import QtQuick.Layouts

import qs.modules.bar as Bar

MouseArea {
    id: root
    property bool borderless: Config.options.bar.borderless
    readonly property var activePlayer: MprisController.activePlayer
    readonly property string cleanedTitle: StringUtils.cleanMusicTitle(activePlayer?.trackTitle) || Translation.tr("No media")

    Layout.fillHeight: true
    implicitHeight: mediaCircProg.implicitHeight
    implicitWidth: Appearance.sizes.verticalBarWidth

    acceptedButtons: Qt.MiddleButton | Qt.BackButton | Qt.ForwardButton | Qt.RightButton | Qt.LeftButton
    hoverEnabled: true
    onPressed: (event) => {
        const id = activePlayer?.id;
        if (event.button === Qt.MiddleButton) {
            if (id)
                MprisController.togglePlaying(id);
        } else if (event.button === Qt.BackButton) {
            if (id)
                MprisController.previous(id);
        } else if (event.button === Qt.ForwardButton || event.button === Qt.RightButton) {
            if (id)
                MprisController.next(id);
        } else if (event.button === Qt.LeftButton) {
            GlobalStates.mediaControlsOpen = !GlobalStates.mediaControlsOpen
        }
    }

    ClippedFilledCircularProgress {
        id: mediaCircProg
        anchors.centerIn: parent
        implicitSize: 20

        lineWidth: Appearance.rounding.unsharpen
        value: activePlayer?.length > 0 ? MprisController.positionOf(activePlayer, Backend.now) / activePlayer.length : 0
        colPrimary: Appearance.colors.colOnSecondaryContainer
        enableAnimation: false

        Item {
            anchors.centerIn: parent
            width: mediaCircProg.implicitSize
            height: mediaCircProg.implicitSize
            
            MaterialSymbol {
                anchors.centerIn: parent
                fill: 1
                text: activePlayer?.isPlaying ? "pause" : "music_note"
                iconSize: Appearance.font.pixelSize.normal
                color: Appearance.m3colors.m3onSecondaryContainer
            }
        }
    }

    Bar.StyledPopup {
        hoverTarget: root
        active: GlobalStates.mediaControlsOpen ? false : root.containsMouse

        ColumnLayout {
            anchors.centerIn: parent
            RowLayout {
                spacing: 5

                MaterialSymbol {
                    fill: 0
                    font.weight: Font.Medium
                    text: "music_note"
                    iconSize: Appearance.font.pixelSize.large
                    color: Appearance.colors.colOnSurfaceVariant
                }

                StyledText {
                    text: "Media"
                    font {
                        weight: Font.Medium
                        pixelSize: Appearance.font.pixelSize.normal
                    }
                    color: Appearance.colors.colOnSurfaceVariant
                }
            }

            StyledText {
                text: `${cleanedTitle}${activePlayer?.trackArtist ? '\n' + activePlayer.trackArtist : ''}`
            }
        }
    }

}
