import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts
import Quickshell

Item {
    id: root
    property real dialogPadding: 15
    property real dialogMargin: 30
    property string titleText: "Selection Dialog"
    property alias items: choiceModel.values
    property int selectedId: -1

    function shownValues() {
        return (root.enableSearch && root.searchText.length > 0) ? filteredModel.values : choiceModel.values;
    }
    property var defaultChoice

    property bool enableSearch: false
    property string searchText: ""

    signal canceled
    signal selected(var result)

    Rectangle { // Scrim
        id: scrimOverlay
        anchors.fill: parent
        radius: Appearance.rounding.small
        color: Appearance.colors.colScrim
        MouseArea {
            hoverEnabled: true
            anchors.fill: parent
            preventStealing: true
            propagateComposedEvents: false
        }
    }

    Rectangle { // The dialog
        id: dialog
        color: Appearance.colors.colSurfaceContainerHigh
        radius: Appearance.rounding.normal
        anchors.fill: parent
        anchors.margins: dialogMargin
        implicitHeight: dialogColumnLayout.implicitHeight

        ColumnLayout {
            id: dialogColumnLayout
            anchors.fill: parent
            spacing: 16

            StyledText {
                id: dialogTitle
                Layout.topMargin: dialogPadding
                Layout.leftMargin: dialogPadding
                Layout.rightMargin: dialogPadding
                Layout.alignment: Qt.AlignLeft
                color: Appearance.m3colors.m3onSurface
                font.pixelSize: Appearance.font.pixelSize.larger
                text: root.titleText
            }

            Rectangle {
                color: Appearance.m3colors.m3outline
                implicitHeight: 1
                Layout.fillWidth: true
                Layout.leftMargin: dialogPadding
                Layout.rightMargin: dialogPadding
            }

            MaterialTextField {
                id: searchField
                visible: root.enableSearch
                placeholderText: "Search…"
                Layout.fillWidth: true
                Layout.leftMargin: dialogPadding
                Layout.rightMargin: dialogPadding
                text: root.searchText
                onTextChanged: {
                    root.searchText = text;
                    filteredModel.filterText = root.searchText;
                    filteredModel.update();
                    // clear any previous selection during search edits
                    root.selectedId = -1;
                    choiceListView.currentIndex = -1;
                }
            }

            StyledListView {
                id: choiceListView
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                spacing: 6

                // Use array models (unchanged from your last fix)
                model: (root.enableSearch && root.searchText.length > 0) ? filteredModel.values : choiceModel.values

                // Do NOT bind currentIndex; keep it inert so it won’t auto-select
                currentIndex: -1

                ScriptModel {
                    id: choiceModel
                }

                ScriptModel {
                    id: filteredModel
                    property string filterText: ""
                    property var values: []

                    function fuzzyScore(pattern, str) {
                        if (!pattern)
                            return 0;
                        const p = pattern.toLowerCase();
                        const s = (str || "").toString().toLowerCase();
                        let score = 0, pi = 0, last = -2;
                        for (let i = 0; i < s.length && pi < p.length; i++) {
                            if (s[i] === p[pi]) {
                                score += (i === last + 1) ? 2 : 1;
                                last = i;
                                pi++;
                            }
                        }
                        return (pi === p.length) ? score : -1;
                    }

                    function update() {
                        const all = choiceModel.values || [];
                        const ft = (filterText || "").trim();
                        if (!ft) {
                            values = all;
                            return;
                        }
                        const scored = [];
                        for (let i = 0; i < all.length; ++i) {
                            const v = all[i];
                            const sc = fuzzyScore(ft, v);
                            if (sc >= 0)
                                scored.push({
                                    v,
                                    sc
                                });
                        }
                        scored.sort((a, b) => b.sc - a.sc);
                        values = scored.map(x => x.v);
                    }

                    Component.onCompleted: update()
                    onFilterTextChanged: update()
                    onValuesChanged: {
                        // list content changed (due to filter) → clear selection
                        root.selectedId = -1;
                        choiceListView.currentIndex = -1;
                    }
                }

                delegate: StyledRadioButton {
                    id: radioButton
                    required property var modelData
                    required property int index
                    anchors {
                        left: parent?.left
                        right: parent?.right
                        leftMargin: root.dialogPadding
                        rightMargin: root.dialogPadding
                    }
                    description: modelData.toString()

                    // bind to our explicit selection, not ListView.currentIndex
                    checked: index === root.selectedId

                    // set selection ONLY on user action
                    onClicked: root.selectedId = index
                    // If StyledRadioButton lacks clicked, use:
                    // onCheckedChanged: if (checked) root.selectedId = index
                }
            }
            Rectangle {
                color: Appearance.m3colors.m3outline
                implicitHeight: 1
                Layout.fillWidth: true
                Layout.leftMargin: dialogPadding
                Layout.rightMargin: dialogPadding
            }

            RowLayout {
                id: dialogButtonsRowLayout
                Layout.bottomMargin: dialogPadding
                Layout.leftMargin: dialogPadding
                Layout.rightMargin: dialogPadding
                Layout.alignment: Qt.AlignRight

                DialogButton {
                    buttonText: Translation.tr("Cancel")
                    onClicked: root.canceled()
                }
                DialogButton {
                    buttonText: Translation.tr("OK")
                    onClicked: {
                        const vals = root.shownValues();
                        root.selected(root.selectedId === -1 ? null : vals[root.selectedId]);
                    }
                }
            }
        }
    }
}
