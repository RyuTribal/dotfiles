pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import QtQuick
import Quickshell

/**
 * Media players, a thin client of axon's `media` topic (section 31, "media"). axon follows the
 * MPRIS players, filters and merges them, picks the active one, keeps each player's position as an
 * anchor and caches its cover art; this maps the rows to the field names the widgets read and
 * sends the controls. A row is a plain object, identical while its `rev` holds (reuseRows).
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "media"
        onSnapshot: data => root._apply(data)
    }

    readonly property var activePlayer: _index[_activeId] ?? null
    readonly property var meaningfulPlayers: _players
    // The rows' ids, for a Repeater's model: a row whose `rev` changed is a new object, while its
    // id stays, so a delegate keyed by id lives on and binds its row from `byId`.
    readonly property var playerIds: _ids
    readonly property var byId: _index

    property var _players: []
    property var _ids: []
    property var _index: ({})
    property string _activeId: ""

    // The position of `player`, a mapped row, at `nowMs`, in seconds.
    function positionOf(player, nowMs) {
        return player ? B.mediaPosition(player, nowMs) / 1000 : 0;
    }

    function togglePlaying(id) {
        _send("media.play_pause", id, {});
    }

    function play(id) {
        _send("media.play", id, {});
    }

    function pause(id) {
        _send("media.pause", id, {});
    }

    function next(id) {
        _send("media.next", id, {});
    }

    function previous(id) {
        _send("media.previous", id, {});
    }

    // Moves by `sec` seconds, back for a negative one.
    function seek(id, sec) {
        _send("media.seek", id, {
            offset_ms: Math.round(sec * 1000)
        });
    }

    // Moves to `sec` seconds into the track.
    function setPosition(id, sec) {
        _send("media.set_position", id, {
            position_ms: Math.max(0, Math.round(sec * 1000))
        });
    }

    // `state` is "none", "track" or "playlist".
    function setLoopState(id, state) {
        _send("media.set_loop", id, {
            loop: state
        });
    }

    function setShuffle(id, on) {
        _send("media.set_shuffle", id, {
            on: on
        });
    }

    function setActivePlayer(id) {
        Backend.call("media.set_active", {
            player: id
        });
    }

    function _send(method, id, args) {
        Backend.call(method, Object.assign({
            player: id
        }, args));
    }

    // A `media` row with the widgets' player field names, seconds for `length`, plus the anchor fields
    // backend.js mediaPosition reads.
    function _row(p) {
        return {
            id: p.id,
            rev: p.rev,
            dbusName: p.bus_name,
            identity: p.identity,
            desktopEntry: p.desktop_entry,
            trackTitle: p.title,
            trackArtist: p.artist,
            trackAlbum: p.album,
            url: p.url,
            artPath: p.art_path ?? "",
            artState: p.art_state,
            playbackState: p.status,
            isPlaying: p.status === "playing",
            length: p.length_ms === null ? 0 : p.length_ms / 1000,
            length_ms: p.length_ms,
            position_ms: p.position_ms,
            sampled_at: p.sampled_at,
            rate: p.rate,
            canControl: p.can_control,
            canPlay: p.can_play,
            canPause: p.can_pause,
            canTogglePlaying: p.can_play || p.can_pause,
            canGoNext: p.can_go_next,
            canGoPrevious: p.can_go_previous,
            canSeek: p.can_seek,
            positionSupported: p.can_seek && p.length_ms !== null,
            loopSupported: p.loop !== null,
            loopState: p.loop ?? "none",
            shuffleSupported: p.shuffle !== null,
            shuffle: p.shuffle === true,
            volume: p.volume,
            lastStartedAt: p.last_started_at
        };
    }

    // No change notification may list an id without its row in `byId`: the ids that leave go
    // before the index loses their rows, and the ids that arrive come after it has them.
    function _apply(data) {
        const rows = B.reuseRows(_index, data.players.map(_row));
        const ids = rows.rows.map(row => row.id);
        const kept = _ids.filter(id => rows.index[id] !== undefined);
        if (kept.length !== _ids.length)
            _ids = kept;
        _index = rows.index;
        _players = rows.rows;
        if (ids.join("\n") !== _ids.join("\n"))
            _ids = ids;
        _activeId = data.active_id ?? "";
    }
}
