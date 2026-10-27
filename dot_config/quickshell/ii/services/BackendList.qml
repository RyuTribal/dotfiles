import QtQuick
import "backend.js" as B

/**
 * Pages one lazy collection of an engine (section 20, "Lazy lists") through
 * `<engine>.list` with `collection`, `offset`, `limit` and the keys of `args`.
 * Page 0 is fetched on activation, when method, collection or args change,
 * when collectionRev changes, and when axon comes back up; the old items stay
 * on screen until it arrives. fetchMore() appends the next page. A new daemon
 * instance clears the list. Page 0 goes out once per cause: triggers in one
 * event loop turn share one request, and a collectionRev that arrives while
 * page 0 is in flight is checked against the reply instead of sending a
 * second request: a reply older than that rev is refetched once. Once, because
 * a topic restored from a persisted snapshot can carry a rev the running
 * engine never reaches.
 */
QtObject {
    id: root

    property string method
    property string collection
    property var args: ({})
    property var collectionRev: null // the rev in the owning topic's stub for this collection
    property bool active: true
    property int pageSize: 100

    readonly property var items: _items
    readonly property int total: _total
    // A request for the current method, collection, args and daemon instance is in flight.
    readonly property bool loading: _inFlightGeneration === _generation
    readonly property string errorCode: _errorCode
    readonly property var extra: _extra // page 0's fields beyond rev, total, offset, next and items

    property var _items: []
    property var _index: ({})    // row id -> row, so unchanged rows keep their objects
    property int _total: 0
    property string _errorCode: ""
    property var _extra: ({})
    property var _rev: null      // the collection rev of the pages held
    property var _next: null     // offset of the next page; null at the end or while page 0 is due
    property int _generation: 0  // a reply to a request from an older generation is dropped
    property int _inFlightGeneration: -1 // the generation of the request in flight; -1 when none
    property int _inFlightOffset: -1     // the offset of the request in flight; -1 when none
    property bool _firstPageDue: false   // page 0 is scheduled and not sent yet
    property bool _revMovedInFlight: false // collectionRev moved after page 0 was sent: check its reply once
    property bool _completed: false

    readonly property var _listFields: ["rev", "total", "offset", "next", "items"]
    readonly property bool _backendUp: Backend.up
    readonly property string _backendInstance: Backend.instance

    // Asks for the next page. Call it when the view nears the end.
    function fetchMore() {
        if (!active || loading || _next === null)
            return;
        _fetch(_next);
    }

    // Starts over at page 0. Several changes in one event loop turn fetch once: the flag stays set
    // until the request is sent.
    function _restart() {
        _generation++;
        _next = null;
        _revMovedInFlight = false; // the next page 0 is asked for after every rev seen so far
        if (_completed && active && !_firstPageDue) {
            _firstPageDue = true;
            Qt.callLater(root._fetchFirstPage);
        }
    }

    function _fetchFirstPage() {
        _firstPageDue = false;
        if (active)
            _fetch(0);
    }

    // The owning topic moved to a new collection rev. Page 0 already in flight was asked for after that
    // rev or will say which rev it holds, so _onPage decides whether it is current.
    function _onRevMoved() {
        if (loading && _inFlightOffset === 0) {
            _revMovedInFlight = true;
            return;
        }
        _restart();
    }

    function _fetch(offset) {
        const generation = _generation;
        const request = Object.assign({}, args, { collection: collection, offset: offset, limit: pageSize });
        _inFlightGeneration = generation;
        _inFlightOffset = offset; // before the call: a disconnected Backend replies at once
        Backend.call(method, request, reply => root._onPage(generation, offset, reply));
    }

    function _onPage(generation, offset, reply) {
        if (generation !== _generation)
            return; // asked with older args, collection or daemon instance; loading tracks the current one
        _inFlightGeneration = -1;
        _inFlightOffset = -1;
        const revMoved = offset === 0 && _revMovedInFlight;
        if (offset === 0)
            _revMovedInFlight = false;
        if (!reply.ok) {
            _errorCode = reply.error?.code ?? "internal";
            return;
        }
        const page = reply.result;
        if (offset > 0 && page.rev !== _rev) {
            _restart(); // the collection changed underneath: refetch from offset 0
            return;
        }
        if (revMoved && collectionRev !== null && page.rev < collectionRev) {
            _restart(); // the topic moved ahead of this page while it was in flight
            return;
        }
        _errorCode = "";
        const reused = B.reuseRows(_index, offset > 0 ? _items.concat(page.items) : page.items);
        _index = reused.index;
        _items = reused.rows;
        _rev = page.rev;
        _total = page.total;
        _next = page.next;
        if (offset === 0)
            _extra = _extraFields(page);
    }

    function _extraFields(page) {
        const extra = {};
        for (const key of Object.keys(page)) {
            if (!_listFields.includes(key))
                extra[key] = page[key];
        }
        return extra;
    }

    // A new daemon instance: rows, revs and offsets of the old one are void.
    function _clear() {
        _generation++;
        _items = [];
        _index = ({});
        _total = 0;
        _rev = null;
        _next = null;
        _extra = ({});
        _errorCode = "";
        _revMovedInFlight = false;
    }

    onMethodChanged: _restart()
    onCollectionChanged: _restart()
    onArgsChanged: _restart()
    onCollectionRevChanged: _onRevMoved()
    onActiveChanged: {
        if (active)
            _restart();
    }
    on_BackendUpChanged: {
        if (_backendUp)
            _restart();
    }
    on_BackendInstanceChanged: _clear()

    Component.onCompleted: {
        _completed = true;
        _restart();
    }
}
