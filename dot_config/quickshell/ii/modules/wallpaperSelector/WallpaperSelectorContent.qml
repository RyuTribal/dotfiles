import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt5Compat.GraphicalEffects
import Quickshell

MouseArea {
    id: root
    property int columns: 4
    property real previewCellAspectRatio: 4 / 3

    Keys.onPressed: event => {
        if (event.key === Qt.Key_Escape) {
            GlobalStates.wallpaperSelectorOpen = false;
            event.accepted = true;
        } else if (event.key === Qt.Key_Left) {
            grid.moveSelection(-1);
            event.accepted = true;
        } else if (event.key === Qt.Key_Right) {
            grid.moveSelection(1);
            event.accepted = true;
        } else if (event.key === Qt.Key_Up) {
            grid.moveSelection(-grid.columns);
            event.accepted = true;
        } else if (event.key === Qt.Key_Down) {
            grid.moveSelection(grid.columns);
            event.accepted = true;
        } else if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
            grid.activateCurrent();
            event.accepted = true;
        } else if (event.key === Qt.Key_Backspace) {
            if (filterField.text.length > 0) {
                filterField.text = filterField.text.substring(0, filterField.text.length - 1);
            }
            filterField.forceActiveFocus();
            event.accepted = true;
        } else if (event.key === Qt.Key_Slash) {
            filterField.forceActiveFocus();
            event.accepted = true;
        } else {
            if (event.text.length > 0) {
                filterField.text += event.text;
                filterField.cursorPosition = filterField.text.length;
                filterField.forceActiveFocus();
            }
            event.accepted = true;
        }
    }

    // The search field opens empty, so the list does too: the query outlives the selector in the
    // Wallpapers singleton.
    Component.onCompleted: Wallpapers.searchQuery = ""

    StyledRectangularShadow {
        target: wallpaperGridBackground
    }
    Rectangle {
        id: wallpaperGridBackground
        anchors {
            fill: parent
            margins: Appearance.sizes.elevationMargin
        }
        focus: true
        border.width: 1
        border.color: Appearance.colors.colLayer0Border
        color: Appearance.colors.colLayer0
        radius: Appearance.rounding.screenRounding - Appearance.sizes.hyprlandGapsOut + 1

        Item {
            id: gridDisplayRegion
            anchors.fill: parent
            anchors.margins: 4

            GridView {
                id: grid

                readonly property int columns: root.columns
                property int currentIndex: 0

                anchors.fill: parent
                cellWidth: width / root.columns
                cellHeight: cellWidth / root.previewCellAspectRatio
                interactive: true
                clip: true
                keyNavigationWraps: true
                boundsBehavior: Flickable.StopAtBounds
                bottomMargin: extraOptions.implicitHeight
                ScrollBar.vertical: StyledScrollBar {}

                function moveSelection(delta) {
                    currentIndex = Math.max(0, Math.min(count - 1, currentIndex + delta));
                    positionViewAtIndex(currentIndex, GridView.Contain);
                }

                function activateCurrent() {
                    const row = Wallpapers.list.items[currentIndex];
                    if (!row)
                        return;
                    Wallpapers.setWallpaper(row.id);
                    GlobalStates.wallpaperSelectorOpen = false;
                }

                // The next page once the view shows the last row; a page that does not fill the view
                // leaves it at the end, so a new count asks too.
                function fetchMoreAtEnd() {
                    if (atYEnd)
                        Wallpapers.list.fetchMore();
                }

                onAtYEndChanged: fetchMoreAtEnd()
                onCountChanged: fetchMoreAtEnd()

                model: ScriptModel {
                    values: Wallpapers.list.items
                    objectProp: "id"
                }
                delegate: WallpaperTile {
                    required property var modelData
                    required property int index
                    row: modelData
                    width: grid.cellWidth
                    height: grid.cellHeight
                    current: index === grid.currentIndex
                    onEntered: grid.currentIndex = index
                }

                layer.enabled: true
                layer.effect: OpacityMask {
                    maskSource: Rectangle {
                        width: gridDisplayRegion.width
                        height: gridDisplayRegion.height
                        radius: wallpaperGridBackground.radius
                    }
                }
            }

            Toolbar {
                id: extraOptions
                anchors {
                    bottom: parent.bottom
                    horizontalCenter: parent.horizontalCenter
                    bottomMargin: 8
                }

                // A theme that declares one mode has nothing to switch.
                ToolbarButton {
                    implicitWidth: height
                    visible: Theme.modes.length > 1
                    onClicked: Theme.setMode(Theme.mode === "dark" ? "light" : "dark")
                    contentItem: MaterialSymbol {
                        text: Theme.mode === "dark" ? "dark_mode" : "light_mode"
                        iconSize: Appearance.font.pixelSize.larger
                    }
                    StyledToolTip {
                        content: Translation.tr("Switch between light and dark mode")
                    }
                }

                ToolbarTextField {
                    id: filterField
                    placeholderText: focus ? Translation.tr("Search wallpapers") : Translation.tr("Hit \"/\" to search")

                    // Style
                    clip: true
                    font.pixelSize: Appearance.font.pixelSize.small

                    // Search
                    onTextChanged: {
                        Wallpapers.searchQuery = text;
                    }

                    Keys.onPressed: event => {
                        if (text.length !== 0) {
                            // No filtering, just navigate grid
                            if (event.key === Qt.Key_Down) {
                                grid.moveSelection(grid.columns);
                                event.accepted = true;
                                return;
                            }
                            if (event.key === Qt.Key_Up) {
                                grid.moveSelection(-grid.columns);
                                event.accepted = true;
                                return;
                            }
                        }
                        event.accepted = false;
                    }
                }

                ToolbarButton {
                    onClicked: {
                        GlobalStates.wallpaperSelectorOpen = false;
                    }
                    contentItem: StyledText {
                        text: "Cancel"
                    }
                }
            }
        }
    }

    Connections {
        target: GlobalStates
        function onWallpaperSelectorOpenChanged() {
            if (GlobalStates.wallpaperSelectorOpen && monitorIsFocused) {
                filterField.forceActiveFocus();
            }
        }
    }
}
