import qs.modules.common
import qs.modules.common.widgets
import qs.modules.topMenu.todo
import QtQuick

Rectangle {
    // Root item of a SwipeView page: sized directly by SwipeView, same as
    // the other top-menu tab roots, so no Layout parent is needed here
    // (unlike CalendarTab's panes, which sit inside its RowLayout).
    color: Appearance.colors.colLayer1
    radius: Appearance.rounding.normal
    clip: true

    TodoWidget {
        anchors.fill: parent
        anchors.margins: 5
    }
}
