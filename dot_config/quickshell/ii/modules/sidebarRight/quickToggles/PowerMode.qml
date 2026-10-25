import qs.modules.common
import qs.modules.common.widgets
import qs
import QtQuick
import Quickshell.Io
import Quickshell

QuickToggleButton {
    id: root
    property string mode: "balanced"
    property int watts: 0
    readonly property var modeIcons: ({
        "lap": "airline_seat_recline_normal",
        "balanced": "balance",
        "max": "local_fire_department"
    })
    readonly property var nextMode: ({
        "lap": "balanced",
        "balanced": "max",
        "max": "lap"
    })

    buttonIcon: modeIcons[root.mode]
    toggled: root.mode !== "balanced"

    onClicked: {
        modeProc.command = ["/home/ryutribal/.local/bin/powermode", nextMode[root.mode]]
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

    // Poll so the chosen mode is re-applied when the EC or firmware moves
    // PL1 or the platform profile (dytc_lapmode flips, EC re-clamping).
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
