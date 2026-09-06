import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.topMenu.calendar
import qs.modules.bar.weather
import QtQuick
import QtQuick.Layouts

RowLayout {
    spacing: 10

    // Calendar pane (60%)
    Rectangle {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.horizontalStretchFactor: 3
        color: Appearance.colors.colLayer1
        radius: Appearance.rounding.normal
        clip: true

        CalendarWidget {
            anchors.centerIn: parent
        }
    }

    // Weather pane (40%). This is WeatherPopup.qml's content lifted out of
    // its StyledPopup/LazyLoader wrapper (that wrapper only handles the
    // bar's hover-positioned popup window) — same header + WeatherCard grid,
    // now shared via WeatherContent.qml, still reading straight from the
    // Weather singleton.
    Rectangle {
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.horizontalStretchFactor: 2
        color: Appearance.colors.colLayer1
        radius: Appearance.rounding.normal
        clip: true

        WeatherContent {
            anchors.centerIn: parent
            textColor: Appearance.colors.colOnLayer1
        }
    }
}
