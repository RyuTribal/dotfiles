import qs
import qs.modules.common
import qs.modules.common.functions
import qs.modules.common.widgets
import qs.services
import QtQuick
import QtQuick.Layouts
import Quickshell

// Batch 5 note: the horizontal bar (modules/bar/Resources.qml) no longer
// uses this — it now gives each stat its own small per-metric StyledPopup
// instead of one big popup covering every metric. This component stays
// alive only because modules/verticalBar/Resources.qml still hovers its
// whole RAM/Swap/CPU column as one unit and shares this popup for that.
StyledPopup {
    id: root

    // The "/" mount, if df reported one.
    readonly property var rootMount: ResourceUsage.mounts.find(m => m.target === "/")

    // Helper function to format KB to GB
    function formatKB(kb) {
        return (kb / (1024 * 1024)).toFixed(1) + " GB";
    }

    component ResourceItem: RowLayout {
        id: resourceItem
        required property string icon
        required property string label
        required property string value
        spacing: 4
        Layout.fillWidth: true

        MaterialSymbol {
            text: resourceItem.icon
            color: Appearance.colors.colOnSurfaceVariant
            iconSize: Appearance.font.pixelSize.large
        }
        StyledText {
            text: resourceItem.label
            color: Appearance.colors.colOnSurfaceVariant
        }
        StyledText {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignRight
            visible: resourceItem.value !== ""
            color: Appearance.colors.colOnSurfaceVariant
            text: resourceItem.value
        }
    }

    component ResourceHeaderItem: RowLayout {
        id: headerItem
        required property var icon
        required property var label
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

    RowLayout {
        anchors.centerIn: parent
        spacing: 12

        ColumnLayout {
            Layout.alignment: Qt.AlignTop
            spacing: 8

            ResourceHeaderItem {
                icon: "memory"
                label: "RAM"
            }
            ColumnLayout {
                ResourceItem {
                    icon: "clock_loader_60"
                    label: Translation.tr("Used:")
                    value: formatKB(ResourceUsage.memoryUsed)
                }
                ResourceItem {
                    icon: "check_circle"
                    label: Translation.tr("Free:")
                    value: formatKB(ResourceUsage.memoryFree)
                }
                ResourceItem {
                    icon: "empty_dashboard"
                    label: Translation.tr("Total:")
                    value: formatKB(ResourceUsage.memoryTotal)
                }
            }
        }

        ColumnLayout {
            visible: ResourceUsage.swapTotal > 0
            Layout.alignment: Qt.AlignTop
            spacing: 8

            ResourceHeaderItem {
                icon: "swap_horiz"
                label: "Swap"
            }
            ColumnLayout {
                ResourceItem {
                    icon: "clock_loader_60"
                    label: Translation.tr("Used:")
                    value: formatKB(ResourceUsage.swapUsed)
                }
                ResourceItem {
                    icon: "check_circle"
                    label: Translation.tr("Free:")
                    value: formatKB(ResourceUsage.swapFree)
                }
                ResourceItem {
                    icon: "empty_dashboard"
                    label: Translation.tr("Total:")
                    value: formatKB(ResourceUsage.swapTotal)
                }
            }
        }

        ColumnLayout {
            Layout.alignment: Qt.AlignTop
            spacing: 8

            ResourceHeaderItem {
                icon: "planner_review"
                label: "CPU"
            }
            ColumnLayout {
                ResourceItem {
                    icon: "bolt"
                    label: Translation.tr("Load:")
                    value: (ResourceUsage.cpuUsage > 0.8 ? Translation.tr("High") : ResourceUsage.cpuUsage > 0.4 ? Translation.tr("Medium") : Translation.tr("Low")) + ` (${Math.round(ResourceUsage.cpuUsage * 100)}%)`
                }
            }
        }

        ColumnLayout {
            Layout.alignment: Qt.AlignTop
            spacing: 8

            ResourceHeaderItem {
                icon: "device_thermostat"
                label: ResourceUsage.cpuTempNamed ? Translation.tr("CPU temp") : Translation.tr("Peak temp")
            }
            ColumnLayout {
                ResourceItem {
                    icon: "bolt"
                    label: Translation.tr("Now:")
                    value: ResourceUsage.cpuTempC > 0 ? ResourceUsage.cpuTempC.toFixed(0) + "°C" : Translation.tr("n/a")
                }
            }
        }

        ColumnLayout {
            visible: root.rootMount !== undefined
            Layout.alignment: Qt.AlignTop
            spacing: 8

            ResourceHeaderItem {
                icon: "storage"
                label: Translation.tr("Disk /")
            }
            ColumnLayout {
                ResourceItem {
                    icon: "clock_loader_60"
                    label: Translation.tr("Used:")
                    value: `${root.rootMount?.pcent ?? 0}%`
                }
                ResourceItem {
                    icon: "check_circle"
                    label: Translation.tr("Free:")
                    value: StringUtils.formatAvail(root.rootMount?.avail ?? 0)
                }
            }
        }
    }
}
