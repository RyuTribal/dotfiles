// SweepPanel.qml — disk usage inspector for Quickshell (illogical-impulse)
// Toggle from anywhere:  qs -c ii ipc call sweep toggle
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.modules.common
import qs.services

Scope {
    id: rootScope

    property bool panelVisible: false
    property bool confirming: false

    IpcHandler {
        target: "sweep"
        function toggle(): void {
            rootScope.panelVisible = !rootScope.panelVisible;
        }
        function show(): void { rootScope.panelVisible = true; }
        function hide(): void { rootScope.panelVisible = false; }
    }

    // Sweep subscribes to axon's sweep topic only while the panel is open.
    Binding {
        target: Sweep
        property: "panelOpen"
        value: rootScope.panelVisible
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
                        text: Sweep.curPath || Sweep.scanPath
                        color: Appearance.colors.colSubtext
                        font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.monospace }
                        elide: Text.ElideMiddle
                    }
                    Text {
                        text: Sweep.totalHuman
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
                        enabled: Sweep.parentId !== null
                        onClicked: Sweep.up()
                    }
                    SweepButton {
                        label: "rescan"
                        onClicked: Sweep.scan(Sweep.scanPath)
                    }
                    SweepButton {
                        label: "restore all"
                        enabled: Sweep.stagedCount > 0
                        onClicked: Sweep.restoreAll()
                    }
                    Item { Layout.fillWidth: true }
                    SweepButton {
                        label: Sweep.stagedCount > 0
                            ? "delete " + Sweep.stagedCount + " (" + Sweep.stagedHuman + ")"
                            : "nothing staged"
                        danger: true
                        enabled: Sweep.stagedCount > 0 && Sweep.canStage
                        onClicked: rootScope.confirming = true
                    }
                }

                // scanning indicator / status
                Text {
                    Layout.fillWidth: true
                    visible: Sweep.scanning || !Sweep.ready
                    text: Sweep.status
                    color: Appearance.colors.colSubtext
                    font { pixelSize: Appearance.font.pixelSize.smaller; family: Appearance.font.family.main }
                }

                // entry list
                ListView {
                    id: listView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true
                    model: ScriptModel {
                        values: Sweep.entries
                        objectProp: "id"
                    }
                    spacing: 3
                    // A directory with more than 100 children arrives in pages.
                    onAtYEndChanged: if (atYEnd) Sweep.fetchMore()

                    property real maxSize: {
                        let m = 1;
                        for (const e of Sweep.entries) if (e.size > m) m = e.size;
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
                                enabled: Sweep.canStage
                                opacity: enabled ? 1 : 0.45
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
                                    onClicked: {
                                        if (row.modelData.marked)
                                            Sweep.unmark(row.modelData.id);
                                        else
                                            Sweep.mark(row.modelData.id);
                                    }
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
                                    Sweep.open(row.modelData.id);
                            }
                        }
                    }
                }

                // footer: axon down, the engine's own status, this view's own failure, the staged
                // line (in the error colour) or the status, the first that applies
                Text {
                    Layout.fillWidth: true
                    text: Sweep.footer
                    color: Sweep.stagedLine !== "" && Sweep.footer === Sweep.stagedLine
                        ? Appearance.m3colors.m3error
                        : Appearance.colors.colSubtext
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
                            text: "Permanently delete " + Sweep.stagedCount
                                + " item(s), " + Sweep.stagedHuman + "?"
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
                                onClicked: {
                                    Sweep.commit();
                                    rootScope.confirming = false;
                                }
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
