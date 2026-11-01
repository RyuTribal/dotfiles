pragma Singleton
pragma ComponentBehavior: Bound

import QtQuick
import Quickshell

/**
 * The cheatsheet's keybind tree, a thin client of axon's `hypr.keybinds` topic (section 30). axon
 * parses the keybind files Hyprland loads and sends the tree in the shape the cheatsheet already
 * reads, the default file's sections first, then the custom file's. A file axon could not read
 * leaves the rest of the tree in place.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "hypr.keybinds"
        onSnapshot: data => root._apply(data)
    }

    // { name, keybinds: [{ mods, key, dispatcher, params, comment }], children: [the same shape] }
    readonly property var keybinds: _keybinds

    property var _keybinds: ({
            children: []
        })

    function _apply(data) {
        _keybinds = data.root;
    }
}
