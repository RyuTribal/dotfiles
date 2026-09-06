import qs.modules.common
import qs.modules.common.functions
import qs.modules.common.widgets
import qs.services
import qs
import QtQuick
import QtQuick.Layouts

// Bar stat row: RAM / CPU / CPU temp / Disk, each shown as a small icon
// paired with a compact percent (or degree) label — batch 4 restores this
// from the pre-batch-2 circle rework (see modules/verticalBar/Resource.qml
// for the same icon+text idiom, still alive there). Clicking anywhere in
// this area jumps the top menu to the Stats tab.
//
// Batch 5 replaces the single row-wide ResourcesPopup with one small
// StyledPopup per stat, each anchored to just that stat's own icon+label
// item — hovering the RAM icon shows only RAM detail, hovering CPU shows
// only CPU detail, etc., instead of one big popup dumping every metric at
// once regardless of which icon triggered it. The old ResourcesPopup.qml
// stays alive (unused here) since modules/verticalBar/Resources.qml still
// shares it for the vertical bar's single-hover-for-the-whole-column
// layout. Swap had its own column in that popup but has no StatItem of
// its own in this row, so its row now lives inside the RAM popup instead.
MouseArea {
    id: root
    property bool borderless: Config.options.bar.borderless
    property bool alwaysShowAllResources: false
    implicitWidth: rowLayout.implicitWidth + rowLayout.anchors.leftMargin + rowLayout.anchors.rightMargin
    implicitHeight: Appearance.sizes.barHeight
    hoverEnabled: true

    // The "/" mount, if df reported one — rootMount.pcent already tracks the
    // Config.options.bar polling cadence set up in ResourceUsage.qml.
    readonly property var rootMount: ResourceUsage.mounts.find(m => m.target === "/")

    function formatPct(fraction) {
        return Math.round(fraction * 100) + "%";
    }

    // Helper function to format KB to GB, mirroring the old ResourcesPopup.
    function formatKB(kb) {
        return (kb / (1024 * 1024)).toFixed(1) + " GB";
    }

    onPressed: (event) => {
        if (event.button === Qt.LeftButton) {
            GlobalStates.topMenuTab = "stats";
            GlobalStates.topMenuOpen = true;
        }
    }

    // One icon + compact label pair, e.g. a "memory" icon next to "60%".
    // Carries its own hover tracking (a click-through MouseArea, since the
    // outer MouseArea already owns clicks for the tab-jump behavior above)
    // so each stat's popup only reacts to hovering that one stat.
    component StatItem: Item {
        id: statItem
        required property string icon
        required property string valueText
        property alias containsMouse: hoverArea.containsMouse
        Layout.alignment: Qt.AlignVCenter
        implicitWidth: innerRow.implicitWidth
        implicitHeight: innerRow.implicitHeight

        RowLayout {
            id: innerRow
            anchors.fill: parent
            spacing: 5

            MaterialSymbol {
                text: statItem.icon
                iconSize: Appearance.font.pixelSize.normal
                color: Appearance.colors.colOnLayer1
            }
            StyledText {
                text: statItem.valueText
                font.pixelSize: Appearance.font.pixelSize.small
                color: Appearance.colors.colOnLayer1
            }
        }

        MouseArea {
            id: hoverArea
            anchors.fill: parent
            hoverEnabled: true
            acceptedButtons: Qt.NoButton
        }
    }

    // Shared header row style for each popup below, mirroring
    // BatteryPopup's header markup.
    component StatPopupHeader: RowLayout {
        id: headerItem
        required property string icon
        required property string label
        spacing: 5

        MaterialSymbol {
            fill: 0
            font.weight: Font.Medium
            text: headerItem.icon
            iconSize: Appearance.font.pixelSize.large
            color: Appearance.colors.colOnSurfaceVariant
        }
        StyledText {
            text: headerItem.label
            font {
                weight: Font.Medium
                pixelSize: Appearance.font.pixelSize.normal
            }
            color: Appearance.colors.colOnSurfaceVariant
        }
    }

    // Shared label/value row style for each popup below, mirroring
    // BatteryPopup's detail rows.
    component StatPopupRow: RowLayout {
        id: popupRow
        required property string label
        required property string value
        Layout.fillWidth: true
        spacing: 8

        StyledText {
            Layout.fillWidth: true
            text: popupRow.label
            color: Appearance.colors.colOnSurfaceVariant
        }
        StyledText {
            horizontalAlignment: Text.AlignRight
            color: Appearance.colors.colOnSurfaceVariant
            text: popupRow.value
        }
    }

    RowLayout {
        id: rowLayout

        spacing: 12
        anchors.fill: parent
        anchors.leftMargin: 4
        anchors.rightMargin: 4

        StatItem {
            id: ramStat
            icon: "memory"
            valueText: `${Math.round(ResourceUsage.memoryUsedPercentage * 100)}%`
        }

        StatItem {
            id: cpuStat
            icon: "planner_review"
            valueText: `${Math.round(ResourceUsage.cpuUsage * 100)}%`
        }

        StatItem {
            id: tempStat
            icon: "device_thermostat"
            valueText: `${Math.round(ResourceUsage.cpuTempC)}°`
        }

        StatItem {
            id: diskStat
            icon: "storage"
            valueText: `${root.rootMount?.pcent ?? 0}%`
        }
    }

    // RAM popup — also carries Swap, since Swap has no StatItem of its own
    // in this row (see file header comment for the reasoning).
    StyledPopup {
        hoverTarget: ramStat

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            StatPopupHeader {
                icon: "memory"
                label: "RAM"
            }
            StatPopupRow {
                label: Translation.tr("Used:")
                value: root.formatKB(ResourceUsage.memoryUsed)
            }
            StatPopupRow {
                label: Translation.tr("Total:")
                value: root.formatKB(ResourceUsage.memoryTotal)
            }
            StatPopupRow {
                label: Translation.tr("Usage:")
                value: root.formatPct(ResourceUsage.memoryUsedPercentage)
            }

            ColumnLayout {
                visible: ResourceUsage.swapTotal > 0
                Layout.fillWidth: true
                Layout.topMargin: 4
                spacing: 4

                StatPopupHeader {
                    icon: "swap_horiz"
                    label: "Swap"
                }
                StatPopupRow {
                    label: Translation.tr("Used:")
                    value: root.formatKB(ResourceUsage.swapUsed)
                }
                StatPopupRow {
                    label: Translation.tr("Total:")
                    value: root.formatKB(ResourceUsage.swapTotal)
                }
            }
        }
    }

    // CPU popup — just load, since ResourceUsage doesn't track per-core
    // figures.
    StyledPopup {
        hoverTarget: cpuStat

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            StatPopupHeader {
                icon: "planner_review"
                label: "CPU"
            }
            StatPopupRow {
                label: Translation.tr("Load:")
                value: root.formatPct(ResourceUsage.cpuUsage)
            }
        }
    }

    // Temp popup — the sensor-source flag is already free (ResourceUsage
    // gathers it from the same reading), so it's shown here rather than
    // spawning a second process just for this popup.
    StyledPopup {
        hoverTarget: tempStat

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            StatPopupHeader {
                icon: "device_thermostat"
                label: ResourceUsage.cpuTempNamed ? Translation.tr("CPU temp") : Translation.tr("Peak temp")
            }
            StatPopupRow {
                label: Translation.tr("Now:")
                value: ResourceUsage.cpuTempC > 0 ? ResourceUsage.cpuTempC.toFixed(0) + "°C" : Translation.tr("n/a")
            }
            StatPopupRow {
                label: Translation.tr("Sensor:")
                value: ResourceUsage.cpuTempNamed ? Translation.tr("CPU package") : Translation.tr("Fallback (max)")
            }
        }
    }

    // Disk popup — the "/" mount only; the full per-mount breakdown lives
    // in the Stats tab's SystemPanel.
    StyledPopup {
        hoverTarget: diskStat

        ColumnLayout {
            anchors.centerIn: parent
            spacing: 4

            StatPopupHeader {
                icon: "storage"
                label: Translation.tr("Disk /")
            }
            StatPopupRow {
                label: Translation.tr("Used:")
                value: `${root.rootMount?.pcent ?? 0}%`
            }
            StatPopupRow {
                label: Translation.tr("Free:")
                value: StringUtils.formatAvail(root.rootMount?.avail ?? 0)
            }
        }
    }
}
