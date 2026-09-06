import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts

// Shared header + 8-card metrics grid, used by both WeatherPopup.qml (the
// bar's hover popup) and CalendarTab.qml (the top menu's weather pane).
// Extracted so the two stop duplicating the same layout with only the text
// color differing between them.
ColumnLayout {
    id: root
    spacing: 5

    // colOnSurfaceVariant for the popup, colOnLayer1 for the top menu tab.
    property color textColor: Appearance.colors.colOnSurfaceVariant

    // Exposed so a caller can reproduce its own pre-extraction sizing quirks
    // against these sub-items (see WeatherPopup.qml, which sizes itself off
    // these directly rather than this root ColumnLayout's own implicit size).
    readonly property alias headerItem: header
    readonly property alias gridItem: gridLayout

    // Header
    ColumnLayout {
        id: header
        Layout.alignment: Qt.AlignHCenter
        spacing: 2

        RowLayout {
            Layout.alignment: Qt.AlignHCenter
            spacing: 6

            MaterialSymbol {
                fill: 0
                font.weight: Font.Medium
                text: "location_on"
                iconSize: Appearance.font.pixelSize.large
                color: root.textColor
            }

            StyledText {
                text: Weather.data.city
                font {
                    weight: Font.Medium
                    pixelSize: Appearance.font.pixelSize.normal
                }
                color: root.textColor
            }
        }
        StyledText {
            id: temp
            font.pixelSize: Appearance.font.pixelSize.smaller
            color: root.textColor
            text: Weather.data.temp + " • " + Translation.tr("Feels like %1").arg(Weather.data.tempFeelsLike)
        }
    }

    // Metrics grid
    GridLayout {
        id: gridLayout
        columns: 2
        rowSpacing: 5
        columnSpacing: 5
        uniformCellWidths: true

        WeatherCard {
            title: Translation.tr("UV Index")
            symbol: "wb_sunny"
            value: Weather.data.uv
        }
        WeatherCard {
            title: Translation.tr("Wind")
            symbol: "air"
            value: `(${Weather.data.windDir}) ${Weather.data.wind}`
        }
        WeatherCard {
            title: Translation.tr("Precipitation")
            symbol: "rainy_light"
            value: Weather.data.precip
        }
        WeatherCard {
            title: Translation.tr("Humidity")
            symbol: "humidity_low"
            value: Weather.data.humidity
        }
        WeatherCard {
            title: Translation.tr("Visibility")
            symbol: "visibility"
            value: Weather.data.visib
        }
        WeatherCard {
            title: Translation.tr("Pressure")
            symbol: "readiness_score"
            value: Weather.data.press
        }
        WeatherCard {
            title: Translation.tr("Sunrise")
            symbol: "wb_twilight"
            value: Weather.data.sunrise
        }
        WeatherCard {
            title: Translation.tr("Sunset")
            symbol: "bedtime"
            value: Weather.data.sunset
        }
    }
}
