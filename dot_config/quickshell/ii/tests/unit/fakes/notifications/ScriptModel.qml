import QtQuick

// Stands in for Quickshell's ScriptModel in tst_notification_list.qml. Unlike fakes/Quickshell's, it
// diffs like the real one: a value that stays (===) keeps its row, a vanished value is a remove, a
// new one an insert at its index, a reordered one a move. So a ListView over it gets the same insert,
// remove and move signals as over the real model. It holds strings only, which the group list's app
// names are.
ListModel {
    id: root

    property var values: []
    property var shown: []

    onValuesChanged: {
        const next = root.values ?? [];
        const rows = root.shown.slice();
        for (let row = rows.length - 1; row >= 0; row--) {
            if (next.indexOf(rows[row]) < 0) {
                root.remove(row, 1);
                rows.splice(row, 1);
            }
        }
        for (let row = 0; row < next.length; row++) {
            if (rows[row] === next[row])
                continue;
            const from = rows.indexOf(next[row], row + 1);
            if (from >= 0) {
                root.move(from, row, 1);
                rows.splice(row, 0, rows.splice(from, 1)[0]);
            } else {
                root.insert(row, {
                    modelData: next[row]
                });
                rows.splice(row, 0, next[row]);
            }
        }
        root.shown = rows;
    }
}
