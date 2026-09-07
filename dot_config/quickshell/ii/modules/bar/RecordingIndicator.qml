import qs.modules.common
import qs.modules.common.widgets
import qs
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

// Bar recording indicator for `mach meet` (phase C, UI surfaces). Visible
// only while ~/.local/share/mach/meetings/.active exists -- polled every
// 2s via a plain FileView reload rather than an inotify watch, since a
// recording started from a bare terminal (no quickshell involvement at
// all) still needs to show up here, and a couple of seconds' latency
// before the dot appears or disappears is unnoticeable for something that
// runs for tens of minutes. Clicking stops the recording immediately:
// `mach meet stop` finalizes the WAV files and queues background
// processing itself (phase B), so this component's job ends the moment
// `.active` is gone again.
MouseArea {
    id: root
    implicitWidth: recording ? rowLayout.implicitWidth : 0
    implicitHeight: Appearance.sizes.barHeight
    visible: recording
    hoverEnabled: true
    property bool hovered: containsMouse

    property bool recording: false
    property string startedAt: ""
    property double nowMs: Date.now()

    readonly property double startedMs: {
        const t = Date.parse(root.startedAt);
        return isNaN(t) ? root.nowMs : t;
    }
    readonly property int elapsedSecs: Math.max(0, Math.round((root.nowMs - root.startedMs) / 1000))
    readonly property string elapsedText: {
        const m = Math.floor(root.elapsedSecs / 60);
        const s = root.elapsedSecs % 60;
        return m + ":" + (s < 10 ? "0" : "") + s;
    }

    FileView {
        id: activeFile
        path: Quickshell.env("HOME") + "/.local/share/mach/meetings/.active"
        onLoaded: {
            try {
                const j = JSON.parse(activeFile.text());
                root.recording = true;
                root.startedAt = j.started_at ?? "";
            } catch (e) {
                root.recording = false;
                root.startedAt = "";
            }
        }
        onLoadFailed: {
            root.recording = false;
            root.startedAt = "";
        }
    }

    // The existence poll this component is built around: .active has no
    // reason to change from outside quickshell most of the time, so a 2s
    // reload (rather than an inotify watch) is the lightest idiom that
    // still catches a `mach meet start`/`stop` run from a plain terminal.
    Timer {
        interval: 2000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: activeFile.reload()
    }

    // Local 1s tick purely to keep the elapsed-time label counting up
    // between the slower existence polls above -- same "fast clock beside
    // a slow data poll" idiom ClaudeIndicator.qml uses for its own
    // countdown.
    Timer {
        interval: 1000
        running: root.recording
        repeat: true
        onTriggered: root.nowMs = Date.now()
    }

    onClicked: Quickshell.execDetached(["mach", "meet", "stop"])

    RowLayout {
        id: rowLayout
        anchors.centerIn: parent
        spacing: 6

        Rectangle {
            id: dot
            Layout.preferredWidth: 8
            Layout.preferredHeight: 8
            radius: 4
            color: Appearance.colors.colError

            SequentialAnimation on opacity {
                running: root.recording
                loops: Animation.Infinite
                NumberAnimation {
                    from: 1
                    to: 0.3
                    duration: 700
                    easing.type: Easing.InOutQuad
                }
                NumberAnimation {
                    from: 0.3
                    to: 1
                    duration: 700
                    easing.type: Easing.InOutQuad
                }
            }
        }

        StyledText {
            text: root.elapsedText
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colOnLayer1
        }
    }

    StyledToolTip {
        content: Translation.tr("Recording — click to stop")
    }
}
