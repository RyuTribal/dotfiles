import qs
import qs.modules.common
import qs.modules.common.functions
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts

// System stats panel: CPU/RAM usage bars, CPU temperature, and per-mount
// disk usage bars, all sourced from the ResourceUsage service. Lives inside
// StatsTab's right half, mirroring ClaudePanel's layout on the left.
//
// The whole column is wrapped in a StyledFlickable (the scrollable
// container `modules/topMenu/todo/TaskList.qml` already uses) rather than
// just the disk-mount Repeater: a machine with more mounts than fit, or
// just a narrower panel, would otherwise silently clip content inside
// StatsTab's fixed-size, clipped Rectangle.
Item {
    id: root

    function formatPct(fraction) {
        return Math.round(fraction * 100) + "%";
    }

    StyledFlickable {
        id: flickable
        anchors.fill: parent
        contentHeight: column.height
        clip: true

        ColumnLayout {
            id: column
            width: flickable.width
            spacing: 12

            // Sections 1-3: CPU / RAM / temp as a single centered row of
            // hollow ring gauges (batch 4 — replaces the four StyledProgressBar
            // rows this panel used to have; batch 5 drops the disk-root ring
            // from this row since the per-mount list below already covers
            // every mount df reports, "/" included, as bars). Each ring's
            // warning threshold restores the coloring the bar's circle
            // rework (batch 2) had dropped: RAM/CPU compare against the
            // existing Config.options.bar.resources thresholds, temp
            // uses a fixed threshold since no config option exists for it.
            GridLayout {
                Layout.fillWidth: true
                Layout.alignment: Qt.AlignHCenter
                columns: 3
                rowSpacing: 16
                columnSpacing: 16

                RingGauge {
                    Layout.alignment: Qt.AlignHCenter
                    diameter: 100
                    value: Math.max(0, Math.min(1, ResourceUsage.memoryUsedPercentage))
                    label: Translation.tr("RAM")
                    valueText: root.formatPct(ResourceUsage.memoryUsedPercentage)
                    warning: ResourceUsage.memoryUsedPercentage * 100 >= Config.options.bar.resources.memoryWarningThreshold
                }

                RingGauge {
                    Layout.alignment: Qt.AlignHCenter
                    diameter: 100
                    value: Math.max(0, Math.min(1, ResourceUsage.cpuUsage))
                    label: Translation.tr("CPU")
                    valueText: root.formatPct(ResourceUsage.cpuUsage)
                    warning: ResourceUsage.cpuUsage * 100 >= Config.options.bar.resources.cpuWarningThreshold
                }

                // Label reflects whether the reading came from a named CPU
                // sensor (coretemp/k10temp) or the max-of-all-hwmon fallback
                // (which isn't guaranteed to be the CPU — see
                // ResourceUsage.cpuTempNamed).
                RingGauge {
                    Layout.alignment: Qt.AlignHCenter
                    diameter: 100
                    value: Math.max(0, Math.min(1, (ResourceUsage.cpuTempC - 30) / 65))
                    label: ResourceUsage.cpuTempNamed ? Translation.tr("CPU temp") : Translation.tr("Peak temp")
                    valueText: ResourceUsage.cpuTempC > 0 ? ResourceUsage.cpuTempC.toFixed(0) + "°C" : Translation.tr("n/a")
                    warning: ResourceUsage.cpuTempC >= 80
                }
            }

            // Divider
            Rectangle {
                Layout.fillWidth: true
                height: 1
                color: Appearance.colors.colOutlineVariant
                opacity: 0.3
            }

            // Section 4: per-mount disk usage
            ColumnLayout {
                Layout.fillWidth: true
                spacing: 10

                StyledText {
                    text: Translation.tr("Disk")
                    font.weight: Font.Medium
                }

                Repeater {
                    model: ResourceUsage.mounts
                    delegate: ColumnLayout {
                        required property var modelData
                        Layout.fillWidth: true
                        spacing: 2

                        RowLayout {
                            Layout.fillWidth: true
                            StyledText {
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                                text: modelData.target
                            }
                            StyledText {
                                color: Appearance.colors.colSubtext
                                text: modelData.pcent + "% " + Translation.tr("used") + " (" + StringUtils.formatAvail(modelData.avail) + " " + Translation.tr("free") + ")"
                            }
                        }
                        StyledProgressBar {
                            Layout.fillWidth: true
                            value: Math.max(0, Math.min(1, modelData.pcent / 100))
                        }
                    }
                }
            }

            // Bottom padding so the last row isn't flush against the
            // flickable's edge (matches TaskList.qml's listBottomPadding
            // idiom).
            Item {
                Layout.fillWidth: true
                implicitHeight: 10
            }
        }
    }
}
