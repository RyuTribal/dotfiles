pragma ComponentBehavior: Bound
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import qs
import Quickshell
import QtQuick
import QtQuick.Layouts

MouseArea {
    id: root
    property bool hovered: false

    // Auto-refresh every N minutes (tweak as you like)
    property int refreshMinutes: 15
    readonly property int refreshMs: refreshMinutes * 60 * 1000

    implicitWidth: rowLayout.implicitWidth + 10 * 2
    implicitHeight: Appearance.sizes.barHeight
    hoverEnabled: true

    // Manual refresh (kept as-is)
    onClicked: {
        Weather.getData();
        Quickshell.execDetached(["notify-send", Translation.tr("Weather"), Translation.tr("Refreshing (manually triggered)"), "-a", "Shell"]);
    }

    // Initial fetch on mount
    Component.onCompleted: Weather.getData()

    // Poller
    Timer {
        id: weatherPoller
        interval: root.refreshMs
        running: true
        repeat: true
        onTriggered: Weather.getData()
    }

    // (Optional) save power: pause polling when window not active
    // If you have a suitable signal; otherwise remove this block.
    Connections {
        target: Quickshell.window // or your app’s window singleton
        function onActiveChanged() {
            weatherPoller.running = Quickshell.window.active;
        }
    }

    RowLayout {
        id: rowLayout
        anchors.centerIn: parent

        MaterialSymbol {
            fill: 0
            text: WeatherIcons.codeToName[Weather.data.wCode] ?? "cloud"
            iconSize: Appearance.font.pixelSize.large
            color: Appearance.colors.colOnLayer1
            Layout.alignment: Qt.AlignVCenter
        }

        StyledText {
            visible: true
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colOnLayer1
            text: Weather.data?.temp ?? "--°"
            Layout.alignment: Qt.AlignVCenter
        }
    }

    WeatherPopup {
        id: weatherPopup
        hoverTarget: root
    }
}
