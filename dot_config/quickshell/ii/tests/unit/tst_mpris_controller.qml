import QtQuick
import QtTest
import ShellUnderTest

// The real services/MprisController.qml over the stand-in Backend of fakes/Backend.qml: the rows it
// maps from the `media` topic, the active player, positions, and the calls its functions send
// (section 31 media, D11). backend_tests.sh copies it without its `pragma Singleton` line into
// ShellUnderTest as MprisControllerService, beside backend.js, so each test makes its own instance.
// Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "MprisControllerService"

    Component {
        id: controllerComponent
        MprisControllerService {}
    }

    Component {
        id: spectrumComponent
        BackendTopic {
            name: "media.spectrum"
        }
    }

    property double rev: 0

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 1792922460000;
        Backend.downSince = 0;
        tc.rev = 0;
    }

    function frame(topic, status, data) {
        tc.rev += 1;
        topic._apply({
            t: "snapshot",
            topic: topic.name,
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1792922458200,
            ops: [],
            data: data
        });
    }

    // Section 31's two example rows.
    function brave(overrides) {
        return Object.assign({
            id: "brave.instance3292441",
            rev: 57,
            bus_name: "org.mpris.MediaPlayer2.brave.instance3292441",
            identity: "Brave",
            desktop_entry: "brave-browser",
            status: "playing",
            title: "Mitt hjärta är ditt",
            artist: "Example Artist",
            album: "",
            url: "https://music.youtube.com/watch?v=abc123",
            length_ms: 214000,
            position_ms: 61500,
            sampled_at: 1792922458200,
            rate: 1.0,
            art_path: "/home/ryutribal/.cache/axon/media/art/5e0c3f2a9b71d4e8c03a6f19d2b8e475.jpg",
            art_state: "cached",
            can_control: true,
            can_play: true,
            can_pause: true,
            can_go_next: true,
            can_go_previous: true,
            can_seek: true,
            loop: null,
            shuffle: null,
            volume: 1.0,
            last_started_at: 1792922011000
        }, overrides ?? {});
    }

    function mpv(overrides) {
        return Object.assign({
            id: "mpv.instance81234",
            rev: 4,
            bus_name: "org.mpris.MediaPlayer2.mpv.instance81234",
            identity: "mpv",
            desktop_entry: "mpv",
            status: "paused",
            title: "lecture-07.mkv",
            artist: "",
            album: "",
            url: "file:///home/ryutribal/Videos/lecture-07.mkv",
            length_ms: 3412000,
            position_ms: 1290400,
            sampled_at: 1792918860000,
            rate: 0.0,
            art_path: null,
            art_state: "none",
            can_control: true,
            can_play: true,
            can_pause: true,
            can_go_next: false,
            can_go_previous: false,
            can_seek: true,
            loop: "none",
            shuffle: false,
            volume: 1.0,
            last_started_at: 1792916760000
        }, overrides ?? {});
    }

    function mediaData(activeId, players) {
        return {
            active_id: activeId,
            players: players
        };
    }

    function test_rowsMapFieldNames() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv()]));
        const row = media.meaningfulPlayers[0];
        compare(row.id, "brave.instance3292441");
        compare(row.dbusName, "org.mpris.MediaPlayer2.brave.instance3292441");
        compare(row.trackTitle, "Mitt hjärta är ditt");
        compare(row.trackArtist, "Example Artist");
        compare(row.isPlaying, true);
        compare(row.playbackState, "playing");
        compare(row.length, 214);
        compare(row.positionSupported, true);
        compare(row.loopSupported, false);
        compare(row.loopState, "none");
        compare(row.shuffleSupported, false);
        compare(row.artPath, "/home/ryutribal/.cache/axon/media/art/5e0c3f2a9b71d4e8c03a6f19d2b8e475.jpg");
        const other = media.meaningfulPlayers[1];
        compare(other.isPlaying, false);
        compare(other.artPath, "");
        compare(other.loopSupported, true);
        compare(other.shuffleSupported, true);
        compare(other.shuffle, false);
        compare(other.canGoNext, false);
    }

    function test_unknownLengthHasNoPosition() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData(null, [brave({
                length_ms: null
            })]));
        compare(media.meaningfulPlayers[0].length, 0);
        compare(media.meaningfulPlayers[0].positionSupported, false);
    }

    function test_activePlayerIsTheActiveRow() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("mpv.instance81234", [brave(), mpv()]));
        compare(media.activePlayer.id, "mpv.instance81234");
        verify(media.activePlayer === media.meaningfulPlayers[1]);
        frame(media.topic, "ok", mediaData(null, []));
        compare(media.activePlayer, null);
    }

    function test_reuseRowsKeepsObjects() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv()]));
        const first = media.meaningfulPlayers[0];
        const second = media.meaningfulPlayers[1];
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv({
                rev: 5,
                status: "playing"
            })]));
        verify(media.meaningfulPlayers[0] === first);
        verify(media.meaningfulPlayers[1] !== second);
        compare(media.meaningfulPlayers[1].isPlaying, true);
        verify(media.activePlayer === first);
    }

    // A Repeater keyed by id keeps its delegates across a new rev and finds the new row by id.
    function test_idsStayWhileRowsChange() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv()]));
        const ids = media.playerIds;
        compare(ids, ["brave.instance3292441", "mpv.instance81234"]);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv({
                rev: 5,
                status: "playing"
            })]));
        verify(media.playerIds === ids);
        compare(media.byId["mpv.instance81234"].isPlaying, true);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave()]));
        compare(media.playerIds, ["brave.instance3292441"]);
        compare(media.byId["mpv.instance81234"], undefined);
    }

    // A delegate keyed by id reads byId[id]: no change may list an id whose row is gone, or a
    // removed player's delegate binds undefined before it is destroyed.
    function test_noIdWithoutRow() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv()]));
        const missing = [];
        const check = () => {
            for (const id of media.playerIds) {
                if (!media.byId[id])
                    missing.push(id);
            }
        };
        media.byIdChanged.connect(check);
        media.playerIdsChanged.connect(check);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave()]));
        frame(media.topic, "ok", mediaData("brave.instance3292441", [mpv(), brave()]));
        frame(media.topic, "ok", mediaData(null, []));
        media.byIdChanged.disconnect(check);
        media.playerIdsChanged.disconnect(check);
        compare(missing, []);
        compare(media.playerIds, []);
    }

    function test_positionOfExtrapolates() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave(), mpv()]));
        const playing = media.meaningfulPlayers[0];
        compare(media.positionOf(playing, 1792922458200), 61.5);
        compare(media.positionOf(playing, 1792922460200), 63.5);
        compare(media.positionOf(playing, 1792922458200 + 300000), 214);
        compare(media.positionOf(media.meaningfulPlayers[1], 1792922460200), 1290.4);
        compare(media.positionOf(null, 1792922460200), 0);
    }

    function test_serviceFunctionsCallMedia() {
        const media = createTemporaryObject(controllerComponent, tc);
        const id = "mpv.instance81234";
        media.togglePlaying(id);
        media.play(id);
        media.pause(id);
        media.next(id);
        media.previous(id);
        media.seek(id, -10);
        media.setPosition(id, 61.5);
        media.setLoopState(id, "track");
        media.setShuffle(id, true);
        media.setActivePlayer(id);
        const sent = Backend.calls.map(call => call.method + " " + JSON.stringify(call.args));
        compare(sent, ['media.play_pause {"player":"mpv.instance81234"}', 'media.play {"player":"mpv.instance81234"}', 'media.pause {"player":"mpv.instance81234"}', 'media.next {"player":"mpv.instance81234"}', 'media.previous {"player":"mpv.instance81234"}', 'media.seek {"player":"mpv.instance81234","offset_ms":-10000}', 'media.set_position {"player":"mpv.instance81234","position_ms":61500}', 'media.set_loop {"player":"mpv.instance81234","loop":"track"}', 'media.set_shuffle {"player":"mpv.instance81234","on":true}', 'media.set_active {"player":"mpv.instance81234"}']);
    }

    function test_noPlayersBeforeFirstSnapshot() {
        const media = createTemporaryObject(controllerComponent, tc);
        compare(media.topic.status, "waiting");
        compare(media.meaningfulPlayers, []);
        compare(media.activePlayer, null);
    }

    function test_dataNullKeepsRows() {
        const media = createTemporaryObject(controllerComponent, tc);
        frame(media.topic, "ok", mediaData("brave.instance3292441", [brave()]));
        const row = media.meaningfulPlayers[0];
        frame(media.topic, "starting", null);
        compare(media.meaningfulPlayers.length, 1);
        verify(media.activePlayer === row);
    }

    // A widget's spectrum topic is wanted only while its gate holds: Backend attaches it when it
    // turns active and detaches it when it turns inactive, and counts both widgets for one `sub`.
    function test_spectrumTopicGatedByActive() {
        const spectrum = createTemporaryObject(spectrumComponent, tc, {
            active: false
        });
        compare(Backend.attachments, []);
        spectrum.active = true;
        compare(Backend.attachments, ["+media.spectrum"]);
        spectrum.active = false;
        compare(Backend.attachments, ["+media.spectrum", "-media.spectrum"]);
    }
}
