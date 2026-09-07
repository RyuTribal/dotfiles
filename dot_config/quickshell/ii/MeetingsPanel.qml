// MeetingsPanel.qml — meeting recorder review UI for Quickshell (illogical-impulse)
// Toggle from anywhere:  qs -c ii ipc call meetings toggle
//
// Phase C (UI surfaces) over phase A/B's `mach meet` engine
// (engines/meet): this panel only ever reads meeting directories under
// ~/.local/share/mach/meetings/ and shells out to `mach meet
// start`/`stop`. Every actual state transition -- the marker-file state
// machine `needs-processing` -> `needs-summary` -> `processed` -- is owned
// entirely by `mach meet process` (see engines/meet/src/markers.rs); this
// panel never writes a marker itself, it only polls for them.
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import qs.modules.common
import qs.modules.common.widgets

Scope {
    id: rootScope

    property bool panelVisible: false

    // ---------- active-recording state (same FileView-poll idiom as
    // modules/bar/RecordingIndicator.qml, kept independent of it since the
    // two live in different Scopes with no shared state to reuse) ----------
    property bool activeRecording: false
    property string activeDir: ""
    property string activeStartedAt: ""

    // ---------- meetings listing ----------
    property var meetings: []
    property string selectedDir: ""
    property string selectedTitle: ""
    property bool showTranscript: false

    IpcHandler {
        target: "meetings"
        function toggle(): void { rootScope.panelVisible = !rootScope.panelVisible; }
        function open(): void { rootScope.panelVisible = true; }
        function close(): void { rootScope.panelVisible = false; }
    }

    GlobalShortcut {
        name: "meetingsToggle"
        description: "Toggles the meetings panel on press"
        onPressed: rootScope.panelVisible = !rootScope.panelVisible
    }

    onPanelVisibleChanged: {
        if (rootScope.panelVisible) {
            rootScope.refreshActive();
            rootScope.refreshMeetings();
        } else {
            rootScope.selectedDir = "";
            rootScope.showTranscript = false;
        }
    }

    // ---------- active meeting polling ----------
    FileView {
        id: activeFile
        path: Quickshell.env("HOME") + "/.local/share/mach/meetings/.active"
        onLoaded: {
            try {
                const j = JSON.parse(activeFile.text());
                rootScope.activeRecording = true;
                rootScope.activeDir = j.dir ?? "";
                rootScope.activeStartedAt = j.started_at ?? "";
            } catch (e) {
                rootScope.activeRecording = false;
                rootScope.activeDir = "";
                rootScope.activeStartedAt = "";
            }
        }
        onLoadFailed: {
            rootScope.activeRecording = false;
            rootScope.activeDir = "";
            rootScope.activeStartedAt = "";
        }
    }

    function refreshActive() {
        activeFile.reload();
    }

    // 2s poll while the panel is open only -- matches RecordingIndicator's
    // own interval; no reason to poll faster than the bar dot does.
    Timer {
        interval: 2000
        running: rootScope.panelVisible
        repeat: true
        onTriggered: rootScope.refreshActive()
    }

    // ---------- meetings listing ----------
    // One JSON object per meeting directory, newest first. Directory names
    // are timestamp-prefixed (YYYY-MM-DD-HHMM[-slug]) and "name" is the
    // first key emitted, so a plain reverse line-sort on the jq output is
    // already newest-first without a numeric sort. Status folds together
    // the marker-file state machine `mach meet process` drives (see
    // engines/meet/src/markers.rs and process.rs's NEEDS_PROCESSING /
    // NEEDS_SUMMARY / PROCESSED constants) with a "recording" override for
    // whichever directory .active currently names -- that directory has no
    // marker at all yet, so it would otherwise show as "unknown".
    property string listScript: [
        "active_dir=\"\"",
        "if [ -f \"$HOME/.local/share/mach/meetings/.active\" ]; then",
        "  active_dir=$(jq -r '.dir // empty' \"$HOME/.local/share/mach/meetings/.active\" 2>/dev/null)",
        "fi",
        "shopt -s nullglob",
        "for d in \"$HOME/.local/share/mach/meetings\"/*/; do",
        "  d=\"${d%/}\"",
        "  [ -f \"$d/meeting.json\" ] || continue",
        "  name=$(basename \"$d\")",
        "  if [ \"$d\" = \"$active_dir\" ]; then",
        "    status=recording",
        "  elif [ -f \"$d/processed\" ]; then",
        "    status=processed",
        "  elif [ -f \"$d/needs-summary\" ]; then",
        "    status=needs-summary",
        "  elif [ -f \"$d/needs-processing\" ]; then",
        "    status=needs-processing",
        "  else",
        "    status=unknown",
        "  fi",
        "  jq -c --arg name \"$name\" --arg status \"$status\" --arg dir \"$d\" '{name:$name,status:$status,dir:$dir,title:.title,started_at:.started_at,ended_at:.ended_at,duration_secs:.duration_secs,mode:.mode}' \"$d/meeting.json\"",
        "done | sort -r"
    ].join("\n")

    Process {
        id: listProc
        command: ["bash", "-c", rootScope.listScript]
        stdout: StdioCollector {
            id: listCollector
            onStreamFinished: {
                const lines = listCollector.text.split("\n").filter(l => l.trim().length > 0);
                const parsed = [];
                for (const line of lines) {
                    try {
                        parsed.push(JSON.parse(line));
                    } catch (e) {
                        // one malformed line (a meeting.json mid-write, say)
                        // must never take the whole listing down with it
                    }
                }
                rootScope.meetings = parsed;
            }
        }
    }

    function refreshMeetings() {
        listProc.running = false;
        listProc.running = true;
    }

    // 3s poll while open -- picks up a background `mach meet process` run
    // finishing (needs-processing -> needs-summary -> processed) without
    // requiring the panel to be closed and reopened.
    Timer {
        interval: 3000
        running: rootScope.panelVisible
        repeat: true
        onTriggered: rootScope.refreshMeetings()
    }

    function startMeeting(solo) {
        Quickshell.execDetached(solo ? ["mach", "meet", "start", "--solo"] : ["mach", "meet", "start"]);
        rootScope.refreshActive();
    }

    function stopMeeting() {
        Quickshell.execDetached(["mach", "meet", "stop"]);
        rootScope.refreshActive();
    }

    // ---------- summary/transcript content ----------
    // Only ever pointed at a real file while a meeting is actually
    // selected -- an empty path is harmless (FileView just reports a load
    // failure it never shows anywhere, since the detail view isn't visible
    // in that state either) but there is no reason to point it at nothing.
    FileView {
        id: summaryFile
        path: rootScope.selectedDir.length > 0 ? (rootScope.selectedDir + "/summary.md") : ""
    }
    FileView {
        id: transcriptFile
        path: (rootScope.selectedDir.length > 0 && rootScope.showTranscript) ? (rootScope.selectedDir + "/transcript.md") : ""
    }

    function selectMeeting(m) {
        if (m.status !== "processed") return;
        rootScope.selectedDir = m.dir;
        rootScope.selectedTitle = m.title || m.name;
        rootScope.showTranscript = false;
    }

    function backToList() {
        rootScope.selectedDir = "";
        rootScope.showTranscript = false;
    }

    function humanDuration(secs) {
        if (secs === undefined || secs === null) return "--";
        const m = Math.floor(secs / 60);
        const s = Math.floor(secs % 60);
        return m + "m" + (s < 10 ? "0" : "") + s + "s";
    }

    function statusLabel(status) {
        switch (status) {
            case "recording": return "recording";
            case "needs-processing": return "needs processing";
            case "needs-summary": return "needs summary";
            case "processed": return "processed";
            default: return status;
        }
    }

    LazyLoader {
        active: rootScope.panelVisible

        FloatingWindow {
            id: panel
            title: "meetings"
            implicitWidth: 720
            implicitHeight: 600
            color: Appearance.colors.colLayer0
            visible: true
            onVisibleChanged: if (!visible) rootScope.panelVisible = false

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 18
                spacing: 10

                // header
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 10
                    Text {
                        text: "meetings"
                        color: Appearance.colors.colPrimary
                        font { pixelSize: Appearance.font.pixelSize.huge; family: Appearance.font.family.title; weight: Font.DemiBold }
                    }
                    Item { Layout.fillWidth: true }
                    Text {
                        visible: rootScope.activeRecording
                        text: "● recording"
                        color: Appearance.colors.colError
                        font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.monospace }
                    }
                }

                // toolbar (list view only)
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8
                    visible: rootScope.selectedDir === ""

                    MeetingsButton {
                        label: "start meeting (dual)"
                        visible: !rootScope.activeRecording
                        onClicked: rootScope.startMeeting(false)
                    }
                    MeetingsButton {
                        label: "start solo"
                        visible: !rootScope.activeRecording
                        onClicked: rootScope.startMeeting(true)
                    }
                    MeetingsButton {
                        label: "stop"
                        danger: true
                        visible: rootScope.activeRecording
                        onClicked: rootScope.stopMeeting()
                    }
                    Item { Layout.fillWidth: true }
                    MeetingsButton {
                        label: "refresh"
                        onClicked: rootScope.refreshMeetings()
                    }
                }

                // ---------- list ----------
                ListView {
                    id: meetingsList
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: rootScope.selectedDir === ""
                    clip: true
                    model: rootScope.meetings
                    spacing: 4

                    delegate: Rectangle {
                        id: row
                        required property var modelData
                        width: meetingsList.width
                        height: 58
                        radius: Appearance.rounding.verysmall
                        color: rowArea.containsMouse && rowArea.enabled ? Appearance.colors.colLayer1Hover : Appearance.colors.colLayer1
                        opacity: row.modelData.status === "processed" || row.modelData.status === "recording" ? 1 : 0.7

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 12
                            anchors.rightMargin: 12
                            spacing: 10

                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 1
                                Text {
                                    text: row.modelData.title || row.modelData.name
                                    color: Appearance.colors.colOnLayer1
                                    font { pixelSize: Appearance.font.pixelSize.large; family: Appearance.font.family.main }
                                    elide: Text.ElideRight
                                }
                                Text {
                                    text: row.modelData.started_at || ""
                                    color: Appearance.colors.colSubtext
                                    font { pixelSize: Appearance.font.pixelSize.small; family: Appearance.font.family.monospace }
                                }
                            }
                            Text {
                                text: row.modelData.status === "recording" ? "recording…" : rootScope.humanDuration(row.modelData.duration_secs)
                                color: Appearance.colors.colSubtext
                                font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.monospace }
                            }
                            Rectangle {
                                Layout.preferredWidth: statusText.implicitWidth + 24
                                Layout.preferredHeight: 30
                                radius: Appearance.rounding.full
                                color: row.modelData.status === "recording" ? Qt.alpha(Appearance.m3colors.m3error, 0.2)
                                     : row.modelData.status === "processed" ? Qt.alpha(Appearance.m3colors.m3primary, 0.16)
                                     : Appearance.colors.colLayer2
                                Text {
                                    id: statusText
                                    anchors.centerIn: parent
                                    text: rootScope.statusLabel(row.modelData.status)
                                    color: row.modelData.status === "recording" ? Appearance.colors.colError
                                         : row.modelData.status === "processed" ? Appearance.colors.colPrimary
                                         : Appearance.colors.colOnLayer2
                                    font { pixelSize: Appearance.font.pixelSize.small; family: Appearance.font.family.main }
                                }
                            }
                        }

                        MouseArea {
                            id: rowArea
                            anchors.fill: parent
                            hoverEnabled: true
                            enabled: row.modelData.status === "processed"
                            onClicked: rootScope.selectMeeting(row.modelData)
                        }
                    }
                }

                Text {
                    Layout.fillWidth: true
                    visible: rootScope.selectedDir === "" && rootScope.meetings.length === 0
                    text: "no meetings yet — start one above"
                    color: Appearance.colors.colSubtext
                    font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.main }
                }

                // ---------- detail view: summary.md, or transcript.md when toggled ----------
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: rootScope.selectedDir !== ""
                    spacing: 8

                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 8
                        MeetingsButton {
                            label: "← back"
                            onClicked: rootScope.backToList()
                        }
                        Text {
                            Layout.fillWidth: true
                            text: rootScope.selectedTitle
                            color: Appearance.colors.colOnLayer0
                            font { pixelSize: Appearance.font.pixelSize.larger; family: Appearance.font.family.main; weight: Font.DemiBold }
                            elide: Text.ElideRight
                        }
                        MeetingsButton {
                            label: rootScope.showTranscript ? "show summary" : "show transcript"
                            onClicked: rootScope.showTranscript = !rootScope.showTranscript
                        }
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        radius: Appearance.rounding.small
                        color: Appearance.colors.colLayer1
                        clip: true

                        StyledFlickable {
                            anchors.fill: parent
                            anchors.margins: 12
                            contentWidth: width
                            contentHeight: detailText.implicitHeight

                            Text {
                                id: detailText
                                width: parent.width
                                wrapMode: Text.Wrap
                                text: rootScope.showTranscript
                                    ? (transcriptFile.text().length > 0 ? transcriptFile.text() : "loading transcript…")
                                    : (summaryFile.text().length > 0 ? summaryFile.text() : "loading summary…")
                                color: Appearance.colors.colOnLayer1
                                font {
                                    pixelSize: Appearance.font.pixelSize.large
                                    family: rootScope.showTranscript ? Appearance.font.family.monospace : Appearance.font.family.main
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    component MeetingsButton: Rectangle {
        id: btn
        property string label: ""
        property bool danger: false
        signal clicked()

        implicitWidth: btnText.implicitWidth + 28
        implicitHeight: 36
        radius: Appearance.rounding.full
        color: btnArea.containsMouse
            ? (btn.danger ? Appearance.colors.colErrorContainerHover : Appearance.colors.colLayer2Hover)
            : (btn.danger ? Appearance.colors.colErrorContainer : Appearance.colors.colLayer2)

        Text {
            id: btnText
            anchors.centerIn: parent
            text: btn.label
            color: btn.danger ? Appearance.m3colors.m3onErrorContainer : Appearance.colors.colOnLayer2
            font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.main }
        }
        MouseArea {
            id: btnArea
            anchors.fill: parent
            hoverEnabled: true
            onClicked: btn.clicked()
        }
    }
}
