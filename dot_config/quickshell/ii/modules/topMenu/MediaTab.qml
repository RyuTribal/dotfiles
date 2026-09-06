import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import qs.services
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Services.Mpris

Item {
    id: root

    // The player shown in the big left-hand card. `: MprisController.activePlayer`
    // is a live QML binding, and that's intentional: until the first manual
    // selection, this is meant to track whatever MprisController considers
    // active (e.g. as playback switches players). Clicking a card in the
    // "Players" list on the right does a plain imperative assignment
    // (`root.selectedPlayer = modelData` below), which severs the binding
    // from then on, so a manual selection stops following activePlayer.
    // Typed (not `property var`) so QML's automatic QObject-destruction
    // guard applies to it directly, as defense-in-depth alongside the
    // Connections fallback below, which handles the same case (selected
    // player disappearing) via the meaningfulPlayers list instead.
    property MprisPlayer selectedPlayer: MprisController.activePlayer
    property list<real> visualizerPoints: []

    // If the selected player disappears (closed, or filtered out by the
    // duplicate/YouTube-preview pipeline), fall back to whatever is active
    // instead of pointing at a stale MprisPlayer.
    Connections {
        target: MprisController
        function onMeaningfulPlayersChanged() {
            if (MprisController.meaningfulPlayers.indexOf(root.selectedPlayer) === -1) {
                root.selectedPlayer = MprisController.activePlayer;
            }
        }
    }

    // MediaTabPlayer's internals (art download/state, ColorQuantizer-derived
    // colors, the animated title/artist StyledText labels) are modeled on
    // PlayerControl.qml, which always gets exactly one MprisPlayer for its
    // whole lifetime - the popup's Repeater creates a fresh PlayerControl
    // per player and never reassigns its `player` property afterward. This
    // pane instead reused a single persistent MediaTabPlayer and reassigned
    // `player` on every card click, which exercised a code path none of
    // that machinery was built or tested for: stale art/downloaded state,
    // a stale ColorQuantizer source, and (per the animated StyledText's own
    // internal x/y Behavior, which caches its "resting" position once at
    // Component.onCompleted) a stale title position all surviving a swap of
    // the underlying player object, rather than being freshly (re)computed
    // for it. Fixed by recreating the whole pane instead of reusing it:
    // toggling a Loader's `active` off then back on forces Qt to destroy
    // the old item and instantiate a brand-new one from sourceComponent,
    // exactly once per player-identity change, giving every player its own
    // one-per-lifetime MediaTabPlayer instance just like the popup does.
    Connections {
        target: root
        function onSelectedPlayerChanged() {
            mainPlayerLoader.active = false;
            mainPlayerLoader.active = true;
        }
    }

    // Same cava process MediaControls.qml runs for its waveform background,
    // copied so the embedded card gets the same audio-reactive visualizer
    // instead of a flat one. Gated the same way StatsTab gates its polling:
    // root.SwipeView.isCurrentItem AND GlobalStates.topMenuOpen, so it
    // doesn't run while the menu is closed OR while Media sits off to the
    // side of a SwipeView showing Calendar/Todo/Timer/Stats (SwipeView keeps
    // every page instantiated and does not clear a non-current page's
    // `visible`, so gating on topMenuOpen alone would leave this running
    // whenever the menu is open regardless of which tab is selected).
    Process {
        id: cavaProc
        running: root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
        onRunningChanged: {
            if (!cavaProc.running) {
                root.visualizerPoints = [];
            }
        }
        command: ["cava", "-p", `${FileUtils.trimFileProtocol(Directories.scriptPath)}/cava/raw_output_config.txt`]
        stdout: SplitParser {
            onRead: data => {
                // Parse `;`-separated values into the visualizerPoints array
                let points = data.split(";").map(p => parseFloat(p.trim())).filter(p => !isNaN(p));
                root.visualizerPoints = points;
            }
        }
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
                // Same gate as the cava Process above: only run the position
                // interpolation timers while this tab is actually the one on
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
                            values: MprisController.meaningfulPlayers
                        }
                        delegate: OtherPlayerCard {
                            required property MprisPlayer modelData
                            player: modelData
                            selected: modelData === root.selectedPlayer
                            onSelectedRequested: root.selectedPlayer = modelData
                        }
                    }
                }
            }
        }
    }
}
