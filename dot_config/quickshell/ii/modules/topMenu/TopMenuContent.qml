import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt5Compat.GraphicalEffects

Item {
    id: root
    required property var scopeRoot
    anchors.fill: parent
    // Replaces the old file's sidebarPadding, which was scoped to the removed
    // left sidebar and doesn't apply here.
    property int topMenuPadding: 15
    property var tabButtonList: [
        {"icon": "music_note", "name": Translation.tr("Media")},
        {"icon": "calendar_month", "name": Translation.tr("Calendar")},
        {"icon": "done_outline", "name": Translation.tr("Todo")},
        {"icon": "schedule", "name": Translation.tr("Timer")},
        {"icon": "monitoring", "name": Translation.tr("Stats")}
    ]
    // Name->index map for the tab-jump API (GlobalStates.topMenuTab), kept in
    // the same order as tabButtonList above.
    readonly property var tabNameList: ["media", "calendar", "todo", "timer", "stats"]
    property int selectedTab: 0

    // Tab-jump API: `qs ipc call topMenu openTab <name>` (see TopMenu.qml)
    // sets GlobalStates.topMenuTab, which we consume here and reset right
    // away. One-shot signal semantics: a later manual tab switch doesn't get
    // fought by a stale non-empty value re-triggering this handler.
    Connections {
        target: GlobalStates
        function onTopMenuTabChanged() {
            if (GlobalStates.topMenuTab === "") return;
            const index = root.tabNameList.indexOf(GlobalStates.topMenuTab);
            if (index !== -1) {
                root.selectedTab = index;
            } else {
                console.warn("topMenu: unknown tab name:", GlobalStates.topMenuTab);
            }
            GlobalStates.topMenuTab = "";
        }
    }

    function focusActiveItem() {
        swipeView.currentItem.forceActiveFocus();
    }

    Keys.onPressed: event => {
        if (event.modifiers === Qt.ControlModifier) {
            if (event.key === Qt.Key_PageDown) {
                root.selectedTab = Math.min(root.selectedTab + 1, root.tabButtonList.length - 1);
                event.accepted = true;
            } else if (event.key === Qt.Key_PageUp) {
                root.selectedTab = Math.max(root.selectedTab - 1, 0);
                event.accepted = true;
            } else if (event.key === Qt.Key_Tab) {
                root.selectedTab = (root.selectedTab + 1) % root.tabButtonList.length;
                event.accepted = true;
            } else if (event.key === Qt.Key_Backtab) {
                root.selectedTab = (root.selectedTab - 1 + root.tabButtonList.length) % root.tabButtonList.length;
                event.accepted = true;
            }
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: root.topMenuPadding

        spacing: root.topMenuPadding

        PrimaryTabBar { // Tab strip
            id: tabBar
            tabButtonList: root.tabButtonList
            externalTrackedTab: root.selectedTab
            function onCurrentIndexChanged(currentIndex) {
                root.selectedTab = currentIndex;
            }
        }

        SwipeView { // Content pages
            id: swipeView
            Layout.topMargin: 5
            Layout.fillWidth: true
            Layout.fillHeight: true
            spacing: 10

            currentIndex: tabBar.externalTrackedTab
            onCurrentIndexChanged: {
                tabBar.enableIndicatorAnimation = true;
                root.selectedTab = currentIndex;
            }

            clip: true
            layer.enabled: true
            layer.effect: OpacityMask {
                maskSource: Rectangle {
                    width: swipeView.width
                    height: swipeView.height
                    radius: Appearance.rounding.small
                }
            }

            contentChildren: [mediaTab.createObject(), calendarTab.createObject(), todoTab.createObject(), timerTab.createObject(), statsTab.createObject()]
        }

        Component {
            id: mediaTab
            MediaTab {}
        }
        Component {
            id: calendarTab
            CalendarTab {}
        }
        Component {
            id: todoTab
            TodoTab {}
        }
        Component {
            id: timerTab
            TimerTab {}
        }
        Component {
            id: statsTab
            StatsTab {}
        }
    }
}
