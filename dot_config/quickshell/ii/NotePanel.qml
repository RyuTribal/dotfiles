// NotePanel.qml — quick note capture for Quickshell (illogical-impulse)
// Toggle from anywhere:  qs -c ii ipc call note toggle
//
// Mirrors SweepPanel.qml's shape: a Scope holding all state/process/IPC
// logic (so an in-flight `mach note` classify call survives the panel
// being closed early), with a LazyLoader'd FloatingWindow underneath that
// is purely a view over that state.
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Controls
import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import qs.modules.common
import qs.modules.common.widgets

Scope {
    id: rootScope

    property bool panelVisible: false

    // ---------- draft state ----------
    property string noteText: ""
    property bool hasImage: false
    property int imageVersion: 0
    // XDG_RUNTIME_DIR is tmpfs and per-session — a fine home for an
    // ephemeral clipboard-paste scratch file that must never survive
    // longer than this capture.
    property string tempImagePath: Quickshell.env("XDG_RUNTIME_DIR") + "/mach-note-paste-" + imageVersion + ".png"

    // ---------- submission state ----------
    // idle -> submitting -> done (auto-closes) or error (stays, keeps text)
    property string state: "idle"
    property string resultFiledLine: ""
    property string resultReferLine: ""
    property var resultFacts: []
    property string resultWarning: ""
    property string errorSnippet: ""

    IpcHandler {
        target: "note"
        function toggle(): void { rootScope.panelVisible = !rootScope.panelVisible; }
        function open(): void { rootScope.panelVisible = true; }
        function close(): void { rootScope.panelVisible = false; }
    }

    GlobalShortcut {
        name: "noteToggle"
        description: "Toggles the note panel on press"
        onPressed: rootScope.panelVisible = !rootScope.panelVisible
    }
    GlobalShortcut {
        name: "noteOpen"
        description: "Opens the note panel on press"
        onPressed: rootScope.panelVisible = true
    }
    GlobalShortcut {
        name: "noteClose"
        description: "Closes the note panel on press"
        onPressed: rootScope.panelVisible = false
    }

    onPanelVisibleChanged: {
        if (panelVisible) {
            rootScope.handleOpened();
        } else {
            rootScope.handleClosed();
        }
    }

    // A stale finished draft (success already shown, or a past failure)
    // must not greet the next open — but a submission still in flight
    // keeps showing "classifying…" across a close/reopen since it's
    // still true.
    function handleOpened() {
        if (rootScope.state === "done" || rootScope.state === "error") {
            rootScope.resetDraft();
        }
    }

    // Esc (or any close) discards the draft only if nothing was ever sent
    // to `mach note` yet. A submission in flight, or a just-shown
    // done/error state, is left completely alone: the classify call keeps
    // running to completion in the background even after the window is
    // gone (it lives on this Scope, not inside the LazyLoader), and a
    // failed attempt's text is not wiped out from under the user.
    function handleClosed() {
        if (rootScope.state === "idle") {
            rootScope.cleanupTempImage();
            rootScope.resetDraft();
        }
    }

    function resetDraft() {
        rootScope.noteText = "";
        rootScope.hasImage = false;
        rootScope.state = "idle";
        rootScope.resultFiledLine = "";
        rootScope.resultReferLine = "";
        rootScope.resultFacts = [];
        rootScope.resultWarning = "";
        rootScope.errorSnippet = "";
    }

    function cleanupTempImage() {
        Quickshell.execDetached(["rm", "-f", rootScope.tempImagePath]);
    }

    // ---------- clipboard-image paste (Ctrl+V) ----------
    // wl-paste --list-types is checked first (cheap, text) before ever
    // spawning the actual image dump, so a plain text paste (the common
    // case) never touches disk.
    function checkClipboardForImage() {
        clipTypesProc.running = false;
        clipTypesProc.running = true;
    }

    function beginImageCapture() {
        if (rootScope.hasImage) rootScope.cleanupTempImage();
        rootScope.imageVersion += 1;
        captureProc.running = false;
        captureProc.command = ["bash", "-c", "wl-paste --type image/png > '" + rootScope.tempImagePath + "' 2>/dev/null"];
        captureProc.running = true;
    }

    Process {
        id: clipTypesProc
        command: ["wl-paste", "--list-types"]
        stdout: StdioCollector {
            id: clipTypesCollector
            onStreamFinished: {
                const types = clipTypesCollector.text.split("\n");
                if (types.some(t => t.startsWith("image/"))) {
                    rootScope.beginImageCapture();
                }
            }
        }
    }

    Process {
        id: captureProc
        onExited: (exitCode, exitStatus) => {
            if (exitCode === 0) {
                rootScope.hasImage = true;
            }
        }
    }

    // ---------- submission ----------
    function submitNote() {
        if (rootScope.state === "submitting") return;
        if (rootScope.noteText.trim().length === 0 && !rootScope.hasImage) return;

        rootScope.state = "submitting";
        const args = ["mach", "note"];
        if (rootScope.hasImage) {
            args.push("--image", rootScope.tempImagePath);
        }
        submissionProc.command = args;
        submissionProc.stdinEnabled = true;
        submissionProc.running = true;
    }

    function parseResult(text) {
        const lines = text.split("\n");
        let filedLine = "", referLine = "", warningLine = "";
        let facts = [];
        for (const raw of lines) {
            const line = raw.trim();
            if (line.length === 0) continue;
            if (line.startsWith("warning:")) warningLine = line;
            else if (line.startsWith("filed ")) filedLine = line;
            else if (line.startsWith("refer to it as:")) referLine = line;
            else if (line.startsWith("- ")) facts.push(line.substring(2));
        }
        return { filedLine: filedLine, referLine: referLine, facts: facts, warningLine: warningLine };
    }

    function applySuccess(stdoutText) {
        const parsed = rootScope.parseResult(stdoutText);
        rootScope.resultFiledLine = parsed.filedLine;
        rootScope.resultReferLine = parsed.referLine;
        rootScope.resultFacts = parsed.facts;
        rootScope.resultWarning = parsed.warningLine;
        rootScope.state = "done";
        autoCloseTimer.restart();
    }

    function applyFailure(stderrText, exitCode) {
        const trimmed = stderrText.trim();
        rootScope.errorSnippet = trimmed.length > 0
            ? trimmed.split("\n").slice(0, 6).join("\n")
            : ("mach note exited with code " + exitCode);
        rootScope.state = "error";
    }

    Process {
        id: submissionProc
        stdout: StdioCollector { id: submissionStdout }
        stderr: StdioCollector { id: submissionStderr }
        onRunningChanged: {
            if (submissionProc.running) {
                submissionProc.write(rootScope.noteText);
                submissionProc.stdinEnabled = false; // end input stream
            }
        }
        onExited: (exitCode, exitStatus) => {
            if (rootScope.hasImage) {
                rootScope.cleanupTempImage();
            }
            if (exitCode === 0) {
                rootScope.applySuccess(submissionStdout.text);
            } else {
                rootScope.applyFailure(submissionStderr.text, exitCode);
            }
        }
    }

    Timer {
        id: autoCloseTimer
        interval: 5000
        onTriggered: rootScope.panelVisible = false
    }

    LazyLoader {
        active: rootScope.panelVisible

        FloatingWindow {
            id: panel
            title: "note"
            implicitWidth: 560
            implicitHeight: 400
            color: Appearance.colors.colLayer0
            visible: true
            onVisibleChanged: if (!visible) rootScope.panelVisible = false

            Component.onCompleted: noteInput.forceActiveFocus()

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 18
                spacing: 10

                // header
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 10
                    Text {
                        text: "note"
                        color: Appearance.colors.colPrimary
                        font { pixelSize: Appearance.font.pixelSize.huge; family: Appearance.font.family.title; weight: Font.DemiBold }
                    }
                    Item { Layout.fillWidth: true }
                    Text {
                        visible: rootScope.state === "idle"
                        text: rootScope.noteText.length + " chars"
                        color: Appearance.colors.colSubtext
                        font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.monospace }
                    }
                }

                // attached-image chip
                RowLayout {
                    Layout.fillWidth: true
                    visible: rootScope.hasImage && (rootScope.state === "idle" || rootScope.state === "submitting")
                    spacing: 8

                    Rectangle {
                        Layout.preferredWidth: 40
                        Layout.preferredHeight: 40
                        radius: Appearance.rounding.verysmall
                        color: Appearance.colors.colLayer2
                        clip: true
                        Image {
                            anchors.fill: parent
                            source: rootScope.hasImage ? ("file://" + rootScope.tempImagePath) : ""
                            cache: false
                            fillMode: Image.PreserveAspectCrop
                            asynchronous: true
                        }
                    }
                    MaterialSymbol {
                        text: "image"
                        iconSize: 16
                        color: Appearance.colors.colSubtext
                    }
                    Text {
                        text: "image attached"
                        color: Appearance.colors.colOnLayer0
                        font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
                    }
                    Item { Layout.fillWidth: true }
                    NoteButton {
                        label: "remove"
                        enabled: rootScope.state === "idle"
                        onClicked: {
                            rootScope.hasImage = false;
                            rootScope.cleanupTempImage();
                        }
                    }
                }

                // input surface — present through idle/submitting/error so
                // the text is always there to read, copy, or keep editing;
                // hidden only once a success result is shown.
                Rectangle {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: rootScope.state !== "done"
                    radius: Appearance.rounding.small
                    color: Appearance.colors.colLayer1
                    border.width: noteInput.activeFocus ? 1 : 0
                    border.color: Appearance.colors.colPrimary

                    StyledTextArea {
                        id: noteInput
                        anchors.fill: parent
                        anchors.margins: 10
                        wrapMode: TextArea.Wrap
                        placeholderText: "Jot a note — Enter files it, Shift+Enter newline, Esc discards"
                        text: rootScope.noteText
                        readOnly: rootScope.state === "submitting"
                        opacity: rootScope.state === "submitting" ? 0.5 : 1
                        onTextChanged: rootScope.noteText = text

                        Keys.onPressed: (event) => {
                            if (event.key === Qt.Key_Escape) {
                                event.accepted = true;
                                rootScope.panelVisible = false;
                                return;
                            }
                            if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && !(event.modifiers & Qt.ShiftModifier)) {
                                event.accepted = true;
                                rootScope.submitNote();
                                return;
                            }
                            if (event.key === Qt.Key_V && (event.modifiers & Qt.ControlModifier)) {
                                rootScope.checkClipboardForImage();
                                // not accepted: default text paste (if any) still runs normally
                            }
                        }
                    }
                }

                // classifying indicator
                Text {
                    Layout.fillWidth: true
                    visible: rootScope.state === "submitting"
                    text: "classifying…"
                    color: Appearance.colors.colSubtext
                    font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
                }

                // error banner — text stays in the input box above
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: rootScope.state === "error"
                    spacing: 4
                    Text {
                        Layout.fillWidth: true
                        text: "mach note failed — your text is kept above, copy it or press Enter to retry"
                        color: Appearance.m3colors.m3error
                        wrapMode: Text.Wrap
                        font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main; weight: Font.DemiBold }
                    }
                    Text {
                        Layout.fillWidth: true
                        text: rootScope.errorSnippet
                        color: Appearance.colors.colSubtext
                        wrapMode: Text.Wrap
                        maximumLineCount: 6
                        elide: Text.ElideRight
                        font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.monospace }
                    }
                }

                // success result
                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    visible: rootScope.state === "done"
                    spacing: 6
                    clip: true

                    Text {
                        Layout.fillWidth: true
                        text: rootScope.resultFiledLine
                        color: Appearance.colors.colOnLayer0
                        wrapMode: Text.Wrap
                        font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.main }
                    }
                    Text {
                        Layout.fillWidth: true
                        text: rootScope.resultReferLine
                        color: Appearance.colors.colPrimary
                        wrapMode: Text.Wrap
                        font { pixelSize: Appearance.font.pixelSize.small; family: Appearance.font.family.main; weight: Font.DemiBold }
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 2
                        Repeater {
                            model: rootScope.resultFacts
                            delegate: Text {
                                required property string modelData
                                Layout.fillWidth: true
                                text: "• " + modelData
                                color: Appearance.colors.colOnLayer1
                                wrapMode: Text.Wrap
                                font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
                            }
                        }
                    }
                    Text {
                        Layout.fillWidth: true
                        visible: rootScope.resultWarning.length > 0
                        text: rootScope.resultWarning
                        color: Appearance.m3colors.m3error
                        wrapMode: Text.Wrap
                        font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.main }
                    }
                    Item { Layout.fillHeight: true }
                    Text {
                        Layout.fillWidth: true
                        text: "closing shortly…"
                        color: Appearance.colors.colSubtext
                        font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.main }
                    }
                }
            }
        }
    }

    component NoteButton: Rectangle {
        id: btn
        property string label: ""
        signal clicked()

        implicitWidth: btnText.implicitWidth + 20
        implicitHeight: 26
        radius: Appearance.rounding.full
        opacity: enabled ? 1 : 0.45
        color: btnArea.containsMouse && btn.enabled ? Appearance.colors.colLayer2Hover : Appearance.colors.colLayer2

        Text {
            id: btnText
            anchors.centerIn: parent
            text: btn.label
            color: Appearance.colors.colOnLayer2
            font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.main }
        }
        MouseArea {
            id: btnArea
            anchors.fill: parent
            hoverEnabled: true
            enabled: btn.enabled
            onClicked: btn.clicked()
        }
    }
}
