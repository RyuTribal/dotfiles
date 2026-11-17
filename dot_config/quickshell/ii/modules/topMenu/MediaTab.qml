import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import qs.services
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell

Item {
    id: root

    // The player shown in the big left-hand card. Until the first manual
    // selection this follows whatever axon considers active (e.g. as playback
    // switches players). Clicking a card in the "Players" list on the right
    // sets selectedId, and from then on the card shows that player. A row is a
    // plain object that is replaced when anything in it changes, so the choice
    // is kept as the player's id, never as the row.
    property string selectedId: ""
    readonly property var selectedPlayer: (selectedId !== "" ? MprisController.byId[selectedId] : null) ?? MprisController.activePlayer
    readonly property string shownId: selectedPlayer?.id ?? ""
    property list<real> visualizerPoints: []

    // If the selected player disappears (closed, or filtered out by axon),
    // fall back to whatever is active.
    Connections {
        target: MprisController
        function onPlayerIdsChanged() {
            if (root.selectedId !== "" && MprisController.playerIds.indexOf(root.selectedId) === -1) {
                root.selectedId = "";
            }
        }
    }

    // MediaTabPlayer's internals (ColorQuantizer-derived colors, the animated
    // title/artist StyledText labels) are modeled on PlayerControl.qml, which
    // always shows exactly one player for its whole lifetime - the popup's
    // Repeater creates a fresh PlayerControl per player. A single persistent
    // MediaTabPlayer switched between players kept a stale ColorQuantizer
    // source and (per the animated StyledText's own internal x/y Behavior,
    // which caches its "resting" position once at Component.onCompleted) a
    // stale title position. So the pane is recreated whenever it switches to
    // another player: toggling a Loader's `active` off then back on forces Qt
    // to destroy the old item and instantiate a brand-new one from
    // sourceComponent, once per player id. A new row of the same player only
    // updates the existing pane.
    onShownIdChanged: {
        mainPlayerLoader.active = false;
        mainPlayerLoader.active = true;
    }

    // axon's one shared spectrum, the same topic MediaControls.qml shows, so
    // the embedded card gets the same audio-reactive visualizer. Wanted the
    // way StatsTab gates its polling: root.SwipeView.isCurrentItem AND
    // GlobalStates.topMenuOpen, so it is not wanted while the menu is closed
    // OR while Media sits off to the side of a SwipeView showing
    // Calendar/Todo/Timer/Stats (SwipeView keeps every page instantiated and
    // does not clear a non-current page's `visible`). The bars clear when it
    // stops being wanted.
    BackendTopic {
        name: "media.spectrum"
        active: root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
        onSnapshot: data => root.visualizerPoints = data.bars
        onActiveChanged: if (!active)
            root.visualizerPoints = []
    }

    // Empty state: no players at all. This is the tab's previous single
    // full-size placeholder, unchanged, shown instead of the split layout
    // below rather than alongside it.
    Rectangle {
        anchors.fill: parent
        visible: MprisController.meaningfulPlayers.length === 0
        color: Appearance.colors.colLayer1
        radius: Appearance.rounding.normal
        clip: true

        StyledText {
            anchors.centerIn: parent
            color: Appearance.colors.colSubtext
            text: Translation.tr("No active player")
        }
    }

    // Batch 6 had a second copy of the blurred-art treatment here, blown up
    // to cover the whole tab (main player box AND the Players list beside
    // it). Per user feedback that read as the backdrop bleeding past where
    // it belonged; MediaTabPlayer already has its own box-scoped blurred-art
    // background (see its `background` Rectangle), so the tab itself just
    // needs a plain panel surface behind that box and the Players list.
    Rectangle {
        anchors.fill: parent
        visible: MprisController.meaningfulPlayers.length > 0
        radius: Appearance.rounding.small
        color: Appearance.colors.colLayer1
    }

    RowLayout {
        anchors.fill: parent
        anchors.margins: 6
        spacing: 10
        visible: MprisController.meaningfulPlayers.length > 0

        // Left ~2/3: main player card. Loader instead of an inline
        // MediaTabPlayer so the Connections above can force a fresh
        // instance per player (see its comment for why).
        Loader {
            id: mainPlayerLoader
            Layout.preferredWidth: (root.width - 6 * 2 - 10) * 2 / 3
            Layout.fillHeight: true
            // Always active (not conditioned on meaningfulPlayers - the
            // enclosing RowLayout's own `visible` binding above already
            // hides this whole side of the tab when there are none, and
            // `active` here needs to stay a plain imperative value rather
            // than a live binding, since the Connections below reassigns
            // it directly to force recreation; a declarative binding here
            // would just get silently overwritten/broken by that first
            // assignment).
            active: true
            sourceComponent: mediaTabPlayerComponent
        }

        Component {
            id: mediaTabPlayerComponent
            MediaTabPlayer {
                player: root.selectedPlayer
                visualizerPoints: root.visualizerPoints
                radius: Appearance.rounding.normal
                // Same gate as the spectrum topic above: only run the position
                // tick while this tab is actually the one on
                // screen with the menu open, not whenever this Item merely
                // exists off to the side of the SwipeView.
                isTabActive: root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
            }
        }

        // Right ~1/3: other players list
        ColumnLayout {
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 8

            StyledText {
                Layout.leftMargin: 4
                text: Translation.tr("Players")
                font.pixelSize: Appearance.font.pixelSize.small
                font.weight: Font.Medium
                color: Appearance.colors.colSubtext
            }

            StyledFlickable {
                Layout.fillWidth: true
                Layout.fillHeight: true
                contentWidth: width
                contentHeight: othersColumn.implicitHeight
                clip: true

                ColumnLayout {
                    id: othersColumn
                    width: parent.width
                    spacing: 4

                    Repeater {
                        model: ScriptModel {
                            values: MprisController.playerIds
                        }
                        delegate: OtherPlayerCard {
                            required property string modelData
                            player: MprisController.byId[modelData] ?? null
                            selected: modelData === root.shownId
                            onSelectedRequested: root.selectedId = modelData
                        }
                    }
                }
            }
        }
    }
}
