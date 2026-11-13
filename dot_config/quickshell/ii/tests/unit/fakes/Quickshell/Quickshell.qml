pragma Singleton

import QtQuick

// Stands in for Quickshell's global object in tests: env(), over a fixed environment, and iconPath().
QtObject {
    property var environment: ({
            XDG_RUNTIME_DIR: "/run/user/test"
        })

    // Like Quickshell.env: the value, or null for an unset variable.
    function env(name) {
        return environment[name] ?? null;
    }

    // Like Quickshell.iconPath, marking the name so a test sees which names were resolved.
    function iconPath(name, check) {
        return "icon:" + name;
    }
}
