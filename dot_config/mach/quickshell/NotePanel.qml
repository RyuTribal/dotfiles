// NotePanel.qml — quick note capture for Quickshell (illogical-impulse)
// Toggle from anywhere:  qs -c ii ipc call note toggle
//
// Notes must go away fast: Enter fires the classification job and closes
// the panel in the same tick — no "classifying…" state, no result
// display, nothing to wait on. Classification happens on a Process that
// lives on this Scope (never inside the LazyLoader below), so it keeps
// running to completion even after the FloatingWindow is gone — same
// principle SweepPanel.qml uses for its own long-lived state, just with
// nothing left for the UI to show once fired. Every submission spawns its
// own dynamically-created Process (see `classifyProcComponent`) rather
// than reusing one shared id, so rapid consecutive jots each get an
// independent classification job: nothing here can block, drop, or
// collide with another still in flight (the kb store's WAL journal mode
// plus a 3s busy_timeout on the SQLite side, set once in
// `store::open_with_path`, is what makes concurrent writers from several
// of these processes safe at the storage layer).
//
// All success/failure/fallback reporting has moved to `mach note`
// itself: run with `--quiet`, it prints nothing on a clean success and
// fires a desktop notification (via notify-send) only when something
// went wrong or had to fall back — see `note.rs`'s `Notifier`/
// `NoteOutcome`. This panel no longer parses stdout or shows an error
// banner at all.
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

    IpcHandler {
        target: "note"
        function toggle(): void { rootScope.panelVisible = !rootScope.panelVisible; }
        function open(): void { rootScope.panelVisible = true; }
        function close(): void { rootScope.panelVisible = false; }
        // Files `text` exactly as pressing Enter would, without ever
        // opening the panel — lets rapid-fire concurrency be exercised
        // from a script (`qs -c ii ipc call note submit "..."`) instead
        // of driving the GUI.
        function submit(text: string): void { rootScope.fireClassification(text, false, ""); }
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
        if (!rootScope.panelVisible) {
            rootScope.handleClosed();
        }
    }

    // Esc (or any other close) discards the draft. `submitNote()` already
    // clears `noteText`/`hasImage` and cleans up the temp image itself
    // before it sets `panelVisible = false`, so by the time this runs
    // after a real submission there is nothing left to discard — this
    // only ever does real work after an Esc-style close of an
    // un-submitted draft. Never-lose-text only applies to text that was
    // actually handed to `mach note`; an Esc'd draft was never sent
    // anywhere, so discarding it here is correct, not a loss.
    function handleClosed() {
        if (rootScope.hasImage) {
            rootScope.cleanupTempImage();
        }
        rootScope.noteText = "";
        rootScope.hasImage = false;
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

    // ---------- detached classification ----------
    // One dynamically-created Process per submission (via
    // `classifyProcComponent.createObject`), parented to this Scope —
    // never the LazyLoader'd FloatingWindow below — so it survives the
    // panel closing (which happens in the very same tick this fires) and
    // never collides with another still-running classification: each
    // submission gets its own independent object and its own independent
    // OS process, nothing shared or reused between them.
    function fireClassification(text, hasImage, imgPath) {
        if (text.trim().length === 0 && !hasImage) return;

        classifyProcComponent.createObject(rootScope, {
            "draftText": text,
            "imagePath": hasImage ? imgPath : "",
        });
    }

    Component {
        id: classifyProcComponent

        Process {
            id: classifyProc
            // Set once at creation, read by `command` below and by
            // `onRunningChanged`/`onExited` — never mutated afterward.
            property string draftText: ""
            property string imagePath: ""

            stdinEnabled: true
            command: classifyProc.imagePath.length > 0
                ? ["mach", "note", "--quiet", "--image", classifyProc.imagePath, "-"]
                : ["mach", "note", "--quiet", "-"]

            Component.onCompleted: classifyProc.running = true

            onRunningChanged: {
                if (classifyProc.running) {
                    classifyProc.write(classifyProc.draftText);
                    classifyProc.stdinEnabled = false; // end input stream
                }
            }

            // `mach note --quiet` never prints on success and reports any
            // failure or fallback itself via a desktop notification (see
            // note.rs) — nothing here needs to inspect the exit code.
            // Only cleanup remains: delete the copy-source clipboard
            // image (the permanent, content-addressed copy was already
            // made inside `mach note` before it could possibly have
            // exited), then drop this now-finished dynamic object.
            onExited: (exitCode, exitStatus) => {
                if (classifyProc.imagePath.length > 0) {
                    Quickshell.execDetached(["rm", "-f", classifyProc.imagePath]);
                }
                classifyProc.destroy();
            }
        }
    }

    // ---------- submission ----------
    function submitNote() {
        if (rootScope.noteText.trim().length === 0 && !rootScope.hasImage) return;

        const text = rootScope.noteText;
        const hasImage = rootScope.hasImage;
        const imgPath = rootScope.tempImagePath;

        rootScope.fireClassification(text, hasImage, imgPath);

        // Close immediately — no classifying state, no result display.
        // The dynamically-created Process above outlives this Scope's
        // draft state entirely; only a failure or fallback notification
        // (fired by `mach note --quiet` itself) will ever surface again.
        rootScope.noteText = "";
        rootScope.hasImage = false;
        rootScope.panelVisible = false;
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
                        text: rootScope.noteText.length + " chars"
                        color: Appearance.colors.colSubtext
                        font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.monospace }
                    }
                }

                // attached-image chip
                RowLayout {
                    Layout.fillWidth: true
                    visible: rootScope.hasImage
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
                        onClicked: {
                            rootScope.hasImage = false;
                            rootScope.cleanupTempImage();
                        }
                    }
                }

                // input surface
                Rectangle {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
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
        color: btnArea.containsMouse ? Appearance.colors.colLayer2Hover : Appearance.colors.colLayer2

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
            onClicked: btn.clicked()
        }
    }
}
