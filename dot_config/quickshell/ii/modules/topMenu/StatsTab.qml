import qs
import qs.modules.common
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Stats tab: Claude usage panel on the left, system stats panel on the right.
RowLayout {
    id: root
    spacing: 10

    // Gate ccusage/status polling to when this tab is actually the selected
    // SwipeView page AND the top menu itself is open. `root.visible` is NOT
    // enough on its own: SwipeView keeps every page instantiated and does
    // not clear a non-current page's `visible` (its pages just sit off to
    // the side), so that alone would leave polling running whenever the top
    // menu is open regardless of which tab is selected. `SwipeView.isCurrentItem`
    // is the attached property SwipeView actually maintains per page for
    // this; it defaults to false for an item with no enclosing SwipeView
    // (e.g. the standalone verification harness), which is the correct
    // fail-safe default.
    Binding {
        target: ClaudeUsage
        property: "pollingEnabled"
        value: root.SwipeView.isCurrentItem && GlobalStates.topMenuOpen
        restoreMode: Binding.RestoreBindingOrValue
    }

    // ResourceUsage's temp/disk polling used to be gated the same way, but
    // batch 2 made the bar's stat circles need those readings live with the
    // top menu closed, so ResourceUsage now polls both unconditionally (see
    // services/ResourceUsage.qml) and no longer has a pollingEnabled binding
    // to set here.

    // Claude panel (left half)
    Rectangle {
        Layout.fillWidth: true
        Layout.fillHeight: true
        color: Appearance.colors.colLayer1
        radius: Appearance.rounding.normal
        clip: true

        ClaudePanel {
            anchors.fill: parent
            anchors.margins: 10
        }
    }

    // System stats pane (right half)
    Rectangle {
        Layout.fillWidth: true
        Layout.fillHeight: true
        color: Appearance.colors.colLayer1
        radius: Appearance.rounding.normal
        clip: true

        SystemPanel {
            anchors.fill: parent
            anchors.margins: 10
        }
    }
}
