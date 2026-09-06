// SweepPanel.qml — disk usage inspector popup for Quickshell (Hyprland)
// Toggle from anywhere:  qs ipc call sweep toggle
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import Quickshell.Wayland

Scope {
    id: rootScope

    property bool panelVisible: false
    property string scanPath: Quickshell.env("HOME")

    // ---------- state mirrored from sweepd ----------
    property bool scanning: false
    property bool ready: false
    property string curPath: ""
    property int curId: 0
    property var parentId: null
    property string totalHuman: "0 B"
    property int stagedCount: 0
    property string stagedHuman: "0 B"
    property string status: "idle"
    property var entries: []
    property bool confirming: false

    IpcHandler {
        target: "sweep"
        function toggle(): void {
            rootScope.panelVisible = !rootScope.panelVisible;
            if (rootScope.panelVisible) rootScope.connectAndScan();
        }
        function show(): void { rootScope.panelVisible = true; rootScope.connectAndScan(); }
        function hide(): void { rootScope.panelVisible = false; }
    }

    // Start daemon if it is not running yet.
    Process {
        id: daemonProc
        command: ["sweepd"]
        running: false
    }

    Socket {
        id: sock
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/sweep.sock"
        connected: false
        parser: SplitParser {
            onRead: msg => rootScope.handleMessage(msg)
        }
        onConnectionStateChanged: {
            if (connected) {
                rootScope.status = "connected";
                rootScope.send({ op: "status" });
            }
        }
        onError: {
            // daemon probably not running — spawn it and retry
            daemonProc.running = true;
            retryTimer.start();
        }
    }

    Timer {
        id: retryTimer
        interval: 400
        onTriggered: sock.connected = true
    }

    Timer {
        id: pollTimer
        interval: 250
        repeat: true
        running: rootScope.scanning && sock.connected
        onTriggered: rootScope.send({ op: "status" })
    }

    function connectAndScan() {
        if (!sock.connected) {
            sock.connected = true;
        } else if (!ready && !scanning) {
            startScan(scanPath);
        }
    }

    function send(obj) {
        if (sock.connected) sock.write(JSON.stringify(obj) + "\n");
    }

    function startScan(path) {
        scanPath = path;
        ready = false;
        entries = [];
        send({ op: "scan", path: path });
    }

    function handleMessage(msg) {
        let r;
        try { r = JSON.parse(msg); } catch (e) { return; }

        if (r.error && !r.ok) { status = r.error; }

        // status response
        if (r.scanning !== undefined && r.ready !== undefined) {
            scanning = r.scanning;
            if (r.scanning) {
                status = "scanning… " + r.files + " files, " + r.bytes_human;
            } else if (r.ready) {
                if (!ready) { ready = true; send({ op: "ls" }); }
            } else if (panelVisible) {
                startScan(scanPath);
            }
            return;
        }
        // scan accepted
        if (r.ok && r.scanning === true) { scanning = true; return; }
        // ls response
        if (r.entries !== undefined) {
            curId = r.id;
            parentId = r.parent;
            curPath = r.path;
            totalHuman = r.total_human;
            stagedCount = r.staged_count;
            stagedHuman = r.staged_human;
            entries = r.entries;
            return;
        }
        // mark/unmark response
        if (r.marked !== undefined) { send({ op: "ls", id: curId }); return; }
        // commit / restore response
        if (r.deleted !== undefined) {
            status = "freed " + r.freed_human;
            confirming = false;
            send({ op: "ls", id: curId });
            return;
        }
        if (r.restored !== undefined) {
            status = "restored " + r.restored + " item(s)";
            send({ op: "ls", id: curId });
            return;
        }
    }

    LazyLoader {
        active: rootScope.panelVisible

        PanelWindow {
            id: panel
            anchors { top: true; right: true }
            margins { top: 8; right: 8 }
            implicitWidth: 460
            implicitHeight: 560
            color: "transparent"
            exclusionMode: ExclusionMode.Ignore
            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.OnDemand

            Rectangle {
                anchors.fill: parent
                radius: 14
                color: "#e61e1e2e"
                border.color: "#89b4fa"
                border.width: 1

                ColumnLayout {
                    anchors.fill: parent
                    anchors.margins: 14
                    spacing: 8

                    // header
                    RowLayout {
                        Layout.fillWidth: true
                        Text {
                            text: "sweep"
                            color: "#89b4fa"
                            font { pixelSize: 16; bold: true }
                        }
                        Text {
                            Layout.fillWidth: true
                            text: rootScope.curPath || rootScope.scanPath
                            color: "#cdd6f4"
                            font.pixelSize: 11
                            elide: Text.ElideMiddle
                        }
                        Text {
                            text: rootScope.totalHuman
                            color: "#a6e3a1"
                            font { pixelSize: 13; bold: true }
                        }
                    }

                    // toolbar
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: 6

                        SweepButton {
                            label: "↑ up"
                            enabled: rootScope.parentId !== null && rootScope.parentId !== undefined
                            onClicked: rootScope.send({ op: "ls", id: rootScope.parentId })
                        }
                        SweepButton {
                            label: "⟳ rescan"
                            onClicked: rootScope.startScan(rootScope.scanPath)
                        }
                        SweepButton {
                            label: "restore all"
                            enabled: rootScope.stagedCount > 0
                            onClicked: rootScope.send({ op: "restore_all" })
                        }
                        Item { Layout.fillWidth: true }
                        SweepButton {
                            label: rootScope.stagedCount > 0
                                ? "delete " + rootScope.stagedCount + " (" + rootScope.stagedHuman + ")"
                                : "nothing staged"
                            danger: true
                            enabled: rootScope.stagedCount > 0
                            onClicked: rootScope.confirming = true
                        }
                    }

                    // scanning indicator / status
                    Text {
                        Layout.fillWidth: true
                        visible: rootScope.scanning || !rootScope.ready
                        text: rootScope.status
                        color: "#f9e2af"
                        font.pixelSize: 11
                    }

                    // entry list
                    ListView {
                        id: listView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        model: rootScope.entries
                        spacing: 2

                        property real maxSize: {
                            let m = 1;
                            for (const e of rootScope.entries) if (e.size > m) m = e.size;
                            return m;
                        }

                        delegate: Rectangle {
                            id: row
                            required property var modelData
                            width: listView.width
                            height: 30
                            radius: 6
                            color: rowArea.containsMouse ? "#313244" : "transparent"

                            // usage bar behind the row
                            Rectangle {
                                anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
                                width: parent.width * (row.modelData.size / listView.maxSize)
                                radius: 6
                                color: row.modelData.marked ? "#40f38ba8" : "#3045475a"
                            }

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 4
                                spacing: 8

                                Text {
                                    text: row.modelData.human
                                    color: "#a6e3a1"
                                    font { pixelSize: 12; family: "monospace" }
                                    Layout.preferredWidth: 64
                                    horizontalAlignment: Text.AlignRight
                                }
                                Text {
                                    Layout.fillWidth: true
                                    text: (row.modelData.dir ? " " : " ") + row.modelData.name + (row.modelData.dir ? "/" : "")
                                    color: row.modelData.marked ? "#f38ba8"
                                         : row.modelData.dir ? "#89b4fa" : "#cdd6f4"
                                    font.pixelSize: 12
                                    font.strikeout: row.modelData.marked
                                    elide: Text.ElideMiddle
                                }
                                // mark / unmark
                                Rectangle {
                                    width: 26; height: 22; radius: 5
                                    color: markArea.containsMouse ? "#f38ba8" : "#45475a"
                                    Text {
                                        anchors.centerIn: parent
                                        text: row.modelData.marked ? "↩" : "✕"
                                        color: "#1e1e2e"
                                        font.pixelSize: 12
                                    }
                                    MouseArea {
                                        id: markArea
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        onClicked: rootScope.send({
                                            op: row.modelData.marked ? "unmark" : "mark",
                                            id: row.modelData.id
                                        })
                                    }
                                }
                            }

                            MouseArea {
                                id: rowArea
                                anchors.fill: parent
                                anchors.rightMargin: 34
                                hoverEnabled: true
                                onClicked: {
                                    if (row.modelData.dir && !row.modelData.marked)
                                        rootScope.send({ op: "ls", id: row.modelData.id });
                                }
                            }
                        }
                    }

                    // footer
                    Text {
                        Layout.fillWidth: true
                        text: rootScope.stagedCount > 0
                            ? rootScope.stagedCount + " staged (" + rootScope.stagedHuman + ") — in trash until you delete"
                            : rootScope.status
                        color: rootScope.stagedCount > 0 ? "#f38ba8" : "#6c7086"
                        font.pixelSize: 10
                    }
                }

                // confirm overlay
                Rectangle {
                    anchors.fill: parent
                    radius: 14
                    color: "#cc11111b"
                    visible: rootScope.confirming

                    MouseArea { anchors.fill: parent } // swallow clicks

                    ColumnLayout {
                        anchors.centerIn: parent
                        spacing: 14
                        Text {
                            Layout.alignment: Qt.AlignHCenter
                            text: "Permanently delete " + rootScope.stagedCount
                                + " item(s), " + rootScope.stagedHuman + "?"
                            color: "#f38ba8"
                            font { pixelSize: 14; bold: true }
                        }
                        RowLayout {
                            Layout.alignment: Qt.AlignHCenter
                            spacing: 10
                            SweepButton {
                                label: "cancel"
                                onClicked: rootScope.confirming = false
                            }
                            SweepButton {
                                label: "delete forever"
                                danger: true
                                onClicked: rootScope.send({ op: "commit" })
                            }
                        }
                    }
                }
            }
        }
    }

    component SweepButton: Rectangle {
        id: btn
        property string label: ""
        property bool danger: false
        signal clicked()

        implicitWidth: btnText.implicitWidth + 18
        implicitHeight: 26
        radius: 6
        opacity: enabled ? 1 : 0.4
        color: btnArea.containsMouse && btn.enabled
            ? (danger ? "#f38ba8" : "#89b4fa")
            : "#313244"

        Text {
            id: btnText
            anchors.centerIn: parent
            text: btn.label
            color: btnArea.containsMouse && btn.enabled ? "#1e1e2e" : (btn.danger ? "#f38ba8" : "#cdd6f4")
            font.pixelSize: 11
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
