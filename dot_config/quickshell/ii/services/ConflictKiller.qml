pragma Singleton

import qs
import qs.modules.common
import qs.modules.common.functions
import QtQuick
import Quickshell
import Quickshell.Io

Singleton {
    id: root

    property string killDialogQmlPath: FileUtils.trimFileProtocol(Quickshell.shellPath("killDialog.qml"))

    function load() {
        // dummy to force init
    }

    Connections {
        target: Config
        function onReadyChanged() {
            if (Config.ready) checkConflictsProc.running = true
        }
    }

    // kded6 is not killed: KCrash restarts it at once and it takes over the tray watcher, leaving dead items.
    Process {
        id: checkConflictsProc
        command: ["pidof", "mako", "dunst"]
        stdout: StdioCollector {
            onStreamFinished: {
                const conflictingNotifications = this.text.trim().length > 0;
                if (!conflictingNotifications)
                    return;
                if (Config.options.conflictKiller.autoKillNotificationDaemons)
                    Quickshell.execDetached(["killall", "mako", "dunst"]);
                else
                    Quickshell.execDetached(["qs", "-p", root.killDialogQmlPath]);
            }
        }
    }
}
