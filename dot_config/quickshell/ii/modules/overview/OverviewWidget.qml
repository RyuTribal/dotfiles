import "../../services/hypr.js" as H
import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Wayland
import Quickshell.Hyprland

Item {
    id: root
    required property var panelWindow
    readonly property var monitor: Hypr.monitorFor(panelWindow.screen)
    readonly property var toplevels: ToplevelManager.toplevels
    readonly property int workspacesShown: Config.options.overview.rows * Config.options.overview.columns
    // A named workspace has no number (active_workspace null): the overview shows group 0 then.
    readonly property int workspaceGroup: Math.floor(((monitor?.active_workspace ?? 1) - 1) / workspacesShown)
    property bool monitorIsFocused: Hypr.focusedMonitorName === monitor?.id
    // The previews' toplevels by Hyprland address. HyprlandToplevel.address lacks the "0x" that a
    // window row's `address` carries.
    readonly property var toplevelByAddress: {
        const byAddress = {};
        for (const toplevel of ToplevelManager.toplevels.values)
            byAddress[`0x${toplevel.HyprlandToplevel.address}`] = toplevel;
        return byAddress;
    }
    property real scale: Config.options.overview.scale
    property color activeBorderColor: Appearance.colors.colSecondary

    // The monitor row's scale and reserved edges (left, top, right, bottom), neutral until the first
    // hypr snapshot gives the row.
    readonly property real monitorScale: monitor?.scale ?? 1
    readonly property var monitorReserved: monitor?.reserved ?? [0, 0, 0, 0]
    property real workspaceImplicitWidth: (monitor?.transform % 2 === 1) ? 
        (((monitor?.height ?? 0) - monitorReserved[0] - monitorReserved[2]) * root.scale / monitorScale) :
        (((monitor?.width ?? 0) - monitorReserved[0] - monitorReserved[2]) * root.scale / monitorScale)
    property real workspaceImplicitHeight: (monitor?.transform % 2 === 1) ? 
        (((monitor?.width ?? 0) - monitorReserved[1] - monitorReserved[3]) * root.scale / monitorScale) :
        (((monitor?.height ?? 0) - monitorReserved[1] - monitorReserved[3]) * root.scale / monitorScale)

    property real workspaceNumberMargin: 80
    property real workspaceNumberSize: Math.min(workspaceImplicitHeight, workspaceImplicitWidth) * monitorScale
    property int workspaceZ: 0
    property int windowZ: 1
    property int windowDraggingZ: 99999
    property real workspaceSpacing: 5

    property int draggingFromWorkspace: -1
    property int draggingTargetWorkspace: -1

    implicitWidth: overviewBackground.implicitWidth + Appearance.sizes.elevationMargin * 2
    implicitHeight: overviewBackground.implicitHeight + Appearance.sizes.elevationMargin * 2

    property Component windowComponent: OverviewWindow {}
    property list<OverviewWindow> windowWidgets: []

    StyledRectangularShadow {
        target: overviewBackground
    }
    Rectangle { // Background
        id: overviewBackground
        property real padding: 10
        anchors.fill: parent
        anchors.margins: Appearance.sizes.elevationMargin

        implicitWidth: workspaceColumnLayout.implicitWidth + padding * 2
        implicitHeight: workspaceColumnLayout.implicitHeight + padding * 2
        radius: Appearance.rounding.screenRounding * root.scale + padding
        color: Appearance.colors.colLayer0
        border.width: 1
        border.color: Appearance.colors.colLayer0Border

        ColumnLayout { // Workspaces
            id: workspaceColumnLayout

            z: root.workspaceZ
            anchors.centerIn: parent
            spacing: workspaceSpacing
            Repeater {
                model: Config.options.overview.rows
                delegate: RowLayout {
                    id: row
                    property int rowIndex: index
                    spacing: workspaceSpacing

                    Repeater { // Workspace repeater
                        model: Config.options.overview.columns
                        Rectangle { // Workspace
                            id: workspace
                            property int colIndex: index
                            property int workspaceValue: root.workspaceGroup * workspacesShown + rowIndex * Config.options.overview.columns + colIndex + 1
                            property color defaultWorkspaceColor: Appearance.colors.colLayer1 // TODO: reconsider this color for a cleaner look
                            property color hoveredWorkspaceColor: ColorUtils.mix(defaultWorkspaceColor, Appearance.colors.colLayer1Hover, 0.1)
                            property color hoveredBorderColor: Appearance.colors.colLayer2Hover
                            property bool hoveredWhileDragging: false

                            implicitWidth: root.workspaceImplicitWidth
                            implicitHeight: root.workspaceImplicitHeight
                            color: hoveredWhileDragging ? hoveredWorkspaceColor : defaultWorkspaceColor
                            radius: Appearance.rounding.screenRounding * root.scale
                            border.width: 2
                            border.color: hoveredWhileDragging ? hoveredBorderColor : "transparent"

                            StyledText {
                                anchors.centerIn: parent
                                text: workspaceValue
                                font.pixelSize: root.workspaceNumberSize * root.scale
                                font.weight: Font.DemiBold
                                color: ColorUtils.transparentize(Appearance.colors.colOnLayer1, 0.8)
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                            }

                            MouseArea {
                                id: workspaceArea
                                anchors.fill: parent
                                acceptedButtons: Qt.LeftButton
                                onClicked: {
                                    if (root.draggingTargetWorkspace === -1) {
                                        GlobalStates.overviewOpen = false
                                        Hypr.focusWorkspace(workspaceValue)
                                    }
                                }
                            }

                            DropArea {
                                anchors.fill: parent
                                onEntered: {
                                    root.draggingTargetWorkspace = workspaceValue
                                    if (root.draggingFromWorkspace == root.draggingTargetWorkspace) return;
                                    hoveredWhileDragging = true
                                }
                                onExited: {
                                    hoveredWhileDragging = false
                                    if (root.draggingTargetWorkspace == workspaceValue) root.draggingTargetWorkspace = -1
                                }
                            }

                        }
                    }
                }
            }
        }

        Item { // Windows & focused workspace indicator
            id: windowSpace
            anchors.centerIn: parent
            implicitWidth: workspaceColumnLayout.implicitWidth
            implicitHeight: workspaceColumnLayout.implicitHeight

            Repeater { // Window repeater
                // Windows on the shown group's workspaces; a scratchpad window has no workspace number
                // and is in no group (E5). Rows are matched by id, so a changed row keeps its preview.
                model: ScriptModel {
                    values: Hypr.windows.filter(w => H.inGroup(w.workspace, root.workspaceGroup, root.workspacesShown))
                    objectProp: "id"
                }
                delegate: OverviewWindow {
                    id: window
                    required property var modelData
                    windowData: modelData
                    toplevel: root.toplevelByAddress[modelData.address] ?? null // null: the preview stays empty
                    monitorData: Hypr.monitors.find(m => m.id === modelData.monitor) ?? null
                    scale: root.scale
                    availableWorkspaceWidth: root.workspaceImplicitWidth
                    availableWorkspaceHeight: root.workspaceImplicitHeight
                    widgetMonitorName: root.monitor?.id ?? ""

                    property bool atInitPosition: (initX == x && initY == y)

                    property int workspaceColIndex: (windowData?.workspace - 1) % Config.options.overview.columns
                    property int workspaceRowIndex: Math.floor((windowData?.workspace - 1) % root.workspacesShown / Config.options.overview.columns)
                    xOffset: (root.workspaceImplicitWidth + workspaceSpacing) * workspaceColIndex
                    yOffset: (root.workspaceImplicitHeight + workspaceSpacing) * workspaceRowIndex

                    z: atInitPosition ? root.windowZ : root.windowDraggingZ
                    Drag.hotSpot.x: targetWindowWidth / 2
                    Drag.hotSpot.y: targetWindowHeight / 2
                    MouseArea {
                        id: dragArea
                        anchors.fill: parent
                        hoverEnabled: true
                        onEntered: hovered = true // For hover color change
                        onExited: hovered = false // For hover color change
                        acceptedButtons: Qt.LeftButton | Qt.MiddleButton
                        drag.target: parent
                        onPressed: (mouse) => {
                            root.draggingFromWorkspace = windowData?.workspace
                            window.pressed = true
                            window.Drag.active = true
                            window.Drag.source = window
                            window.Drag.hotSpot.x = mouse.x
                            window.Drag.hotSpot.y = mouse.y
                            // console.log(`[OverviewWindow] Dragging window ${windowData?.address} from position (${window.x}, ${window.y})`)
                        }
                        onReleased: {
                            const targetWorkspace = root.draggingTargetWorkspace
                            window.pressed = false
                            window.Drag.active = false
                            root.draggingFromWorkspace = -1
                            if (targetWorkspace !== -1 && targetWorkspace !== windowData?.workspace) {
                                Hypr.moveWindow(windowData.id, targetWorkspace)
                            }
                            else {
                                window.x = window.initX
                                window.y = window.initY
                            }
                        }
                        onClicked: (event) => {
                            if (!windowData) return;

                            if (event.button === Qt.LeftButton) {
                                GlobalStates.overviewOpen = false
                                Hypr.focusWindow(windowData.id)
                                event.accepted = true
                            } else if (event.button === Qt.MiddleButton) {
                                Hypr.closeWindow(windowData.id)
                                event.accepted = true
                            }
                        }

                        StyledToolTip {
                            extraVisibleCondition: false
                            alternativeVisibleCondition: dragArea.containsMouse && !window.Drag.active
                            content: `${windowData.title}\n[${windowData.class}] ${windowData.xwayland ? "[XWayland] " : ""}\n`
                        }
                    }
                }
            }

            Rectangle { // Focused workspace indicator
                id: focusedWorkspaceIndicator
                property int activeWorkspaceInGroup: (monitor?.active_workspace ?? 1) - (root.workspaceGroup * root.workspacesShown)
                property int activeWorkspaceRowIndex: Math.floor((activeWorkspaceInGroup - 1) / Config.options.overview.columns)
                property int activeWorkspaceColIndex: (activeWorkspaceInGroup - 1) % Config.options.overview.columns
                x: (root.workspaceImplicitWidth + workspaceSpacing) * activeWorkspaceColIndex
                y: (root.workspaceImplicitHeight + workspaceSpacing) * activeWorkspaceRowIndex
                z: root.windowZ
                width: root.workspaceImplicitWidth
                height: root.workspaceImplicitHeight
                color: "transparent"
                radius: Appearance.rounding.screenRounding * root.scale
                border.width: 2
                border.color: root.activeBorderColor
                Behavior on x {
                    animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                }
                Behavior on y {
                    animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                }
            }
        }
    }
}
