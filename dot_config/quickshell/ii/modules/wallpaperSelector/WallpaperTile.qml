import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import QtQuick
import QtQuick.Layouts
import Qt5Compat.GraphicalEffects

/**
 * One wallpaper in the selector, from a row of `theme.list` (section 40.9): the thumbnail axon made,
 * or a placeholder until it has, and the file name. The theme's wallpaper is highlighted. A click
 * sets it as the wallpaper and closes the selector.
 */
MouseArea {
    id: root

    required property var row
    property bool current: false // the keyboard selection
    readonly property bool highlighted: current || containsMouse
    readonly property bool hasThumbnail: typeof row.thumbnail === "string"

    hoverEnabled: true
    onClicked: {
        Wallpapers.setWallpaper(row.id);
        GlobalStates.wallpaperSelectorOpen = false;
    }

    Rectangle {
        id: background
        objectName: "background" // the tests find the items by these names
        anchors.fill: parent
        anchors.margins: Appearance.sizes.wallpaperSelectorItemMargins
        radius: Appearance.rounding.normal
        color: root.highlighted ? Appearance.colors.colPrimary : root.row.active ? Appearance.colors.colSecondaryContainer : ColorUtils.transparentize(Appearance.colors.colPrimaryContainer)
        Behavior on color {
            animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: Appearance.sizes.wallpaperSelectorItemPadding
            spacing: 4

            Item {
                id: imageArea
                Layout.fillWidth: true
                Layout.fillHeight: true

                StyledRectangularShadow {
                    target: thumbnail
                    radius: Appearance.rounding.small
                    visible: thumbnail.visible && thumbnail.status === Image.Ready
                }

                // Thumbnail names are hex digits, so the path needs no encoding.
                Image {
                    id: thumbnail
                    objectName: "thumbnail"
                    anchors.fill: parent
                    visible: root.hasThumbnail
                    source: root.hasThumbnail ? "file://" + root.row.thumbnail : ""
                    asynchronous: true
                    fillMode: Image.PreserveAspectCrop
                    sourceSize.width: width
                    sourceSize.height: height
                    layer.enabled: true
                    layer.effect: OpacityMask {
                        maskSource: Rectangle {
                            width: imageArea.width
                            height: imageArea.height
                            radius: Appearance.rounding.small
                        }
                    }
                }

                Rectangle {
                    objectName: "placeholder"
                    anchors.fill: parent
                    visible: !root.hasThumbnail
                    radius: Appearance.rounding.small
                    color: Appearance.colors.colLayer1

                    MaterialSymbol {
                        anchors.centerIn: parent
                        text: "image"
                        iconSize: Appearance.font.pixelSize.huge
                        color: Appearance.colors.colSubtext
                    }
                }
            }

            StyledText {
                objectName: "name"
                Layout.fillWidth: true
                Layout.leftMargin: 10
                Layout.rightMargin: 10
                horizontalAlignment: Text.AlignHCenter
                elide: Text.ElideRight
                font.pixelSize: Appearance.font.pixelSize.smaller
                color: root.highlighted ? Appearance.colors.colOnPrimary : root.row.active ? Appearance.colors.colOnSecondaryContainer : Appearance.colors.colOnLayer0
                text: root.row.name
                Behavior on color {
                    animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                }
            }
        }
    }
}
