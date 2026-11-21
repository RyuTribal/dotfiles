import QtQuick

// Stands in for Quickshell's ScriptModel in tst_bluetooth_views.qml and tst_network_views.qml with its
// real update rules, ported from ScriptModel::updateValuesUnique (src/core/scriptmodel.cpp at Quickshell
// 4f508be, the installed quickshell-git 0.3.2.r0.g4f508be). Values are matched by `objectProp`. A kept
// value whose position is unchanged and that is not the same object is replaced and announced
// (dataChanged, cpp lines 202-220), and the replacing goes on over the following positions while they
// hold other objects, keys unchecked. A kept value that moves is moved as the object the model already
// holds, with no data change (lines 150-175): its delegate keeps the old object. Without `objectProp`
// the real model compares values structurally; this compares their JSON, enough for these tests.
ListModel {
    id: root

    property var values: []
    property string objectProp: ""
    property var _stored: [] // the values the model holds, as mValues

    onValuesChanged: root._update(root.values ?? [])

    function _key(value) {
        if (objectProp !== "" && value !== null && typeof value === "object" && objectProp in value)
            return value[objectProp];
        return value;
    }

    function _same(a, b) {
        if (a === b)
            return true;
        return JSON.stringify(_key(a)) === JSON.stringify(_key(b));
    }

    function _find(list, from, value) {
        for (let j = from; j < list.length; j++) {
            if (_same(list[j], value))
                return j;
        }
        return -1;
    }

    function _update(fresh) {
        const held = _stored;
        let i = 0;
        let n = 0;
        while (true) {
            if (n === fresh.length) {
                if (i === held.length)
                    break;
                remove(i, held.length - i);
                held.splice(i);
                break;
            } else if (i === held.length) {
                for (; n < fresh.length; n++) {
                    append({
                        modelData: fresh[n]
                    });
                    held.push(fresh[n]);
                }
                break;
            } else if (!_same(fresh[n], held[i])) {
                let old = _find(held, i, fresh[n]);
                if (old !== -1) {
                    if (_find(fresh, n, held[i]) === -1) {
                        // Remove the values at i that are not in the rest of the new list.
                        let j = i;
                        do {
                            j++;
                        } while (j < held.length && _find(fresh, n, held[j]) === -1)
                        remove(i, j - i);
                        held.splice(i, j - i);
                    } else {
                        // Move the run of kept values found further down to i, as they are held.
                        const start = old;
                        do {
                            old++;
                            n++;
                        } while (old < held.length && n < fresh.length && _same(held[old], fresh[n]))
                        const length = old - start;
                        move(start, i, length);
                        held.splice(i, 0, ...held.splice(start, length));
                        i += length;
                    }
                } else {
                    // Insert the run of new values that the model does not hold.
                    const start = n;
                    do {
                        n++;
                    } while (n < fresh.length && _find(held, i, fresh[n]) === -1)
                    for (let k = start; k < n; k++) {
                        insert(i, {
                            modelData: fresh[k]
                        });
                        held.splice(i, 0, fresh[k]);
                        i++;
                    }
                }
            } else if (fresh[n] !== held[i]) {
                do {
                    held[i] = fresh[n];
                    set(i, {
                        modelData: fresh[n]
                    });
                    i++;
                    n++;
                } while (i < held.length && n < fresh.length && fresh[n] !== held[i])
            } else {
                i++;
                n++;
            }
        }
    }
}
