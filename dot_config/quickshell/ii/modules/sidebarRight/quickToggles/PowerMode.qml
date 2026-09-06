import qs.modules.common
import qs.modules.common.widgets
import qs
import QtQuick
import Quickshell.Io
import Quickshell

QuickToggleButton {
    id: root
    property string mode: "auto"
    property int watts: 0
    readonly property var modeIcons: ({
        "auto": "hdr_auto",
        "lap": "airline_seat_recline_normal",
        "desk": "desktop_windows",
        "custom": "tune"
    })
    readonly property var nextMode: ({
        "auto": "lap",
        "lap": "desk",
        "desk": "auto",
        "custom": "auto"
    })

    buttonIcon: modeIcons[root.mode] ?? "tune"
    toggled: root.mode !== "auto"

    onClicked: {
        modeProc.command = ["/home/ryutribal/.local/bin/powermode", nextMode[root.mode] ?? "auto"]
        modeProc.running = true
    }

    function parseState(text) {
        const parts = text.trim().split(" ")
        if (parts.length >= 2) {
            root.mode = parts[0]
            root.watts = parseInt(parts[1])
        }
    }

    Process {
        id: modeProc
        stdout: StdioCollector {
            id: modeCollector
            onStreamFinished: root.parseState(modeCollector.text)
        }
    }

    // Poll so the button follows external changes (EC re-clamping,
    // dytc_lapmode flips while in auto, manual sysfs writes).
    Process {
        id: syncProc
        command: ["/home/ryutribal/.local/bin/powermode", "sync"]
        stdout: StdioCollector {
            id: syncCollector
            onStreamFinished: root.parseState(syncCollector.text)
        }
    }
    Timer {
        interval: 5000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (!modeProc.running && !syncProc.running)
                syncProc.running = true
        }
    }

    StyledToolTip {
        content: Translation.tr("Power mode: %1 (%2 W)").arg(root.mode).arg(root.watts)
    }
}
