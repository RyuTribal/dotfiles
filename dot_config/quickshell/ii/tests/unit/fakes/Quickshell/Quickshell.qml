pragma Singleton

import QtQuick

// Stands in for Quickshell's global object in tests: only env(), over a fixed environment.
QtObject {
    property var environment: ({
            XDG_RUNTIME_DIR: "/run/user/test"
        })

    // Like Quickshell.env: the value, or null for an unset variable.
    function env(name) {
        return environment[name] ?? null;
    }
}
