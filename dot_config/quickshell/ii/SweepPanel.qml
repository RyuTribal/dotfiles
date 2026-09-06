// SweepPanel.qml — disk usage inspector for Quickshell (illogical-impulse)
// Toggle from anywhere:  qs -c ii ipc call sweep toggle
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.modules.common

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
    property string status: "connecting…"
    property var entries: []
    property bool confirming: false

    IpcHandler {
        target: "sweep"
        function toggle(): void {
            rootScope.panelVisible = !rootScope.panelVisible;
        }
        function show(): void { rootScope.panelVisible = true; }
        function hide(): void { rootScope.panelVisible = false; }
    }

    onPanelVisibleChanged: {
        if (panelVisible && !sock.connected) sock.connected = true;
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
            rootScope.status = "starting daemon…";
            daemonProc.running = true;
            retryTimer.start();
        }
    }

    Timer {
        id: retryTimer
        interval: 500
        onTriggered: if (rootScope.panelVisible) sock.connected = true
    }

    // Self-healing driver: while the panel is open, keep polling until we
    // are connected, scanned, and have a listing.
    Timer {
        interval: 300
        repeat: true
        running: rootScope.panelVisible && (rootScope.scanning || !rootScope.ready)
        onTriggered: {
            if (!sock.connected) { sock.connected = true; return; }
            rootScope.send({ op: "status" });
        }
    }

    function send(obj) {
        if (!sock.connected) return;
        sock.write(JSON.stringify(obj) + "\n");
        sock.flush();
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
                if (!ready) { ready = true; status = "ready"; send({ op: "ls" }); }
            } else {
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

        FloatingWindow {
            id: panel
            title: "sweep"
            implicitWidth: 760
            implicitHeight: 540
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
                        text: "sweep"
                        color: Appearance.colors.colPrimary
                        font { pixelSize: Appearance.font.pixelSize.huge; family: Appearance.font.family.title; weight: Font.DemiBold }
                    }
                    Text {
                        Layout.fillWidth: true
                        text: rootScope.curPath || rootScope.scanPath
                        color: Appearance.colors.colSubtext
                        font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.monospace }
                        elide: Text.ElideMiddle
                    }
                    Text {
                        text: rootScope.totalHuman
                        color: Appearance.colors.colOnLayer0
                        font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.monospace; weight: Font.DemiBold }
                    }
                }

                // toolbar
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8

                    SweepButton {
                        label: "↑ up"
                        enabled: rootScope.parentId !== null && rootScope.parentId !== undefined
                        onClicked: rootScope.send({ op: "ls", id: rootScope.parentId })
                    }
                    SweepButton {
                        label: "rescan"
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
                    color: Appearance.colors.colSubtext
                    font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
                }

                // entry list
                ListView {
                    id: listView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: rootScope.entries
                    spacing: 3

                    property real maxSize: {
                        let m = 1;
                        for (const e of rootScope.entries) if (e.size > m) m = e.size;
                        return m;
                    }

                    delegate: Rectangle {
                        id: row
                        required property var modelData
                        width: listView.width
                        height: 34
                        radius: Appearance.rounding.verysmall
                        color: rowArea.containsMouse ? Appearance.colors.colLayer1Hover : Appearance.colors.colLayer1

                        // usage bar behind the row
                        Rectangle {
                            anchors { left: parent.left; top: parent.top; bottom: parent.bottom }
                            width: parent.width * (row.modelData.size / listView.maxSize)
                            radius: Appearance.rounding.verysmall
                            color: row.modelData.marked
                                ? Qt.alpha(Appearance.m3colors.m3error, 0.25)
                                : Qt.alpha(Appearance.m3colors.m3primary, 0.14)
                        }

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 10
                            anchors.rightMargin: 6
                            spacing: 10

                            Text {
                                text: row.modelData.human
                                color: Appearance.colors.colOnLayer1
                                font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.monospace }
                                Layout.preferredWidth: 76
                                horizontalAlignment: Text.AlignRight
                            }
                            Text {
                                Layout.fillWidth: true
                                text: row.modelData.name + (row.modelData.dir ? "/" : "")
                                color: row.modelData.marked ? Appearance.m3colors.m3error
                                     : row.modelData.dir ? Appearance.colors.colPrimary : Appearance.colors.colOnLayer1
                                font { pixelSize: Appearance.font.pixelSize.small; family: Appearance.font.family.main }
                                font.strikeout: row.modelData.marked
                                elide: Text.ElideMiddle
                            }
                            // mark / unmark
                            Rectangle {
                                width: 30; height: 24
                                radius: Appearance.rounding.unsharpenmore
                                color: markArea.containsMouse
                                    ? (row.modelData.marked ? Appearance.m3colors.m3secondaryContainer : Appearance.m3colors.m3errorContainer)
                                    : Appearance.colors.colLayer2
                                Text {
                                    anchors.centerIn: parent
                                    text: row.modelData.marked ? "↩" : "✕"
                                    color: markArea.containsMouse
                                        ? (row.modelData.marked ? Appearance.m3colors.m3onSecondaryContainer : Appearance.m3colors.m3onErrorContainer)
                                        : Appearance.colors.colOnLayer2
                                    font.pixelSize: Appearance.font.pixelSize.smaller
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
                            anchors.rightMargin: 40
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
                    color: rootScope.stagedCount > 0 ? Appearance.m3colors.m3error : Appearance.colors.colSubtext
                    font { pixelSize: Appearance.font.pixelSize.smallest; family: Appearance.font.family.main }
                }
            }

            // confirm overlay
            Rectangle {
                anchors.fill: parent
                color: Qt.alpha(Appearance.m3colors.m3scrim, 0.6)
                visible: rootScope.confirming

                MouseArea { anchors.fill: parent } // swallow clicks

                Rectangle {
                    anchors.centerIn: parent
                    width: confirmCol.implicitWidth + 48
                    height: confirmCol.implicitHeight + 40
                    radius: Appearance.rounding.normal
                    color: Appearance.colors.colLayer1

                    ColumnLayout {
                        id: confirmCol
                        anchors.centerIn: parent
                        spacing: 16
                        Text {
                            Layout.alignment: Qt.AlignHCenter
                            text: "Permanently delete " + rootScope.stagedCount
                                + " item(s), " + rootScope.stagedHuman + "?"
                            color: Appearance.colors.colOnLayer1
                            font { pixelSize: Appearance.font.pixelSize.normal; family: Appearance.font.family.main; weight: Font.DemiBold }
                        }
                        RowLayout {
                            Layout.alignment: Qt.AlignHCenter
                            spacing: 12
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

        implicitWidth: btnText.implicitWidth + 24
        implicitHeight: 30
        radius: Appearance.rounding.full
        opacity: enabled ? 1 : 0.45
        color: btnArea.containsMouse && btn.enabled
            ? (btn.danger ? Appearance.m3colors.m3errorContainer : Appearance.m3colors.m3primaryContainer)
            : Appearance.colors.colLayer2

        Text {
            id: btnText
            anchors.centerIn: parent
            text: btn.label
            color: btnArea.containsMouse && btn.enabled
                ? (btn.danger ? Appearance.m3colors.m3onErrorContainer : Appearance.m3colors.m3onPrimaryContainer)
                : (btn.danger ? Appearance.m3colors.m3error : Appearance.colors.colOnLayer2)
            font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
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
