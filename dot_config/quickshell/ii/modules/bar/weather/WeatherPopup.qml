import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets

import QtQuick
import QtQuick.Layouts
import qs.modules.bar

StyledPopup {
    id: root

    WeatherContent {
        id: weatherContent
        anchors.centerIn: parent
        textColor: Appearance.colors.colOnSurfaceVariant

        // Reproduces the pre-extraction sizing exactly: width fits the wider
        // of header/grid, height tracks the grid only (the header's height
        // was never counted here, before or after this extraction).
        implicitWidth: Math.max(weatherContent.headerItem.implicitWidth, weatherContent.gridItem.implicitWidth)
        implicitHeight: weatherContent.gridItem.implicitHeight
    }
}
