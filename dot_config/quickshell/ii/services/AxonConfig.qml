pragma Singleton
pragma ComponentBehavior: Bound

import QtQuick
import Quickshell

/**
 * A thin store over axon's `config` topic (section 50.1): the effective
 * axon.toml values for settings controls, and the two calls that change
 * them. Settings never edits axon.toml itself.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "config"
        onSnapshot: data => root._apply(data)
    }
    readonly property bool known: topic.hasData // a config snapshot has arrived
    readonly property bool up: Backend.up
    // Stage 1 keeps controls enabled while axon is down (ledger B4); stage 2 adds `&& up`.
    readonly property bool writable: known && _issues.length === 0
    readonly property var values: _values // the full effective config, defaults included
    readonly property var issues: _issues // { path, line, column, key, message } while axon.toml is invalid

    property var _values: ({})
    property var _issues: []

    function _apply(data) {
        _values = data.values ?? {};
        _issues = data.issues ?? [];
    }

    // The effective value at a dotted key such as "power.battery.low";
    // undefined when there is none.
    function value(key) {
        return key.split(".").reduce((table, part) => table?.[part], values);
    }

    // `done` gets null when axon took the value, else the reply's error.
    function set(key, newValue, done) {
        const args = { key: key, value: newValue };
        Backend.call("config.set", args, reply => done?.(reply.ok ? null : reply.error));
    }

    // Removes the key from axon.toml, so its default applies again.
    function unset(key, done) {
        Backend.call("config.unset", { key: key }, reply => done?.(reply.ok ? null : reply.error));
    }
}
