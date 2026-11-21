import QtQuick
import Quickshell

/**
 * A model over a service's rows: plain objects with a unique `id`, which reuseRows keeps identical
 * while their `rev` holds. Use it where a list shows such rows, with `required property var modelData`
 * in the delegate, in place of `ScriptModel { values: rows; objectProp: "id" }`.
 *
 * Invariant: after `_show`, every delegate that existed before shows the row with the id it showed
 * before, and holds that row's current object. `rows` is bound to a service's rows, which change between
 * updates, never during one.
 *
 * Why two passes: Quickshell's ScriptModel (src/core/scriptmodel.cpp, updateValuesUnique, read at
 * 4f508be) moves a kept row as the object it already holds, with no data change (lines 150-175), and
 * once a row changes in place it goes on replacing the following positions while they hold other
 * objects, without comparing keys (lines 202-220), so a reorder below a changed row would rewrite the
 * delegates position by position. So the first pass sets the new order made of the objects the model
 * already holds, new rows as they come: a kept key then always compares as the same object, and the
 * model only removes, inserts and moves, which carries each delegate with its row. The second pass sets
 * the new rows with every key already in place, so the replace path updates each changed row where it
 * stands.
 *
 * The ListView showing it sets `currentIndex: -1`: when a move shifts the view's current item, ListView
 * gives that item the focus, which takes it from a field being typed in another row.
 */
ScriptModel {
    id: root

    required property var rows

    objectProp: "id"

    // The object the model holds for each id, as of the last `_show`; no prototype, so any id is a key.
    property var _held: Object.create(null)

    onRowsChanged: root._show()
    Component.onCompleted: root._show()

    function _show() {
        const held = _held;
        values = rows.map(row => held[row.id] ?? row);
        values = rows.slice();
        const next = Object.create(null);
        for (const row of rows)
            next[row.id] = row;
        _held = next;
    }
}
