import qs.modules.bar.weather
import QtQuick
import QtQuick.Layouts
import Quickshell
import Quickshell.Services.UPower
import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions

Item { // Bar content region
    id: root

    property var screen: root.QsWindow.window?.screen
    property var brightnessMonitor: Brightness.getMonitorForScreen(screen)
    property real useShortenedForm: (Appearance.sizes.barHellaShortenScreenWidthThreshold >= screen?.width) ? 2 : (Appearance.sizes.barShortenScreenWidthThreshold >= screen?.width) ? 1 : 0
    readonly property int centerSideModuleWidth: (useShortenedForm == 2) ? Appearance.sizes.barCenterSideModuleWidthHellaShortened : (useShortenedForm == 1) ? Appearance.sizes.barCenterSideModuleWidthShortened : Appearance.sizes.barCenterSideModuleWidth

    component VerticalBarSeparator: Rectangle {
        Layout.topMargin: Appearance.sizes.baseBarHeight / 3
        Layout.bottomMargin: Appearance.sizes.baseBarHeight / 3
        Layout.fillHeight: true
        implicitWidth: 1
        color: Appearance.colors.colOutlineVariant
    }

    // Background shadow
    Loader {
        active: Config.options.bar.showBackground && Config.options.bar.cornerStyle === 1
        anchors.fill: barBackground
        sourceComponent: StyledRectangularShadow {
            anchors.fill: undefined // The loader's anchors act on this, and this should not have any anchor
            target: barBackground
        }
    }
    // Background
    Rectangle {
        id: barBackground
        anchors {
            fill: parent
            margins: Config.options.bar.cornerStyle === 1 ? (Appearance.sizes.hyprlandGapsOut) : 0 // idk why but +1 is needed
        }
        color: Config.options.bar.showBackground ? Appearance.colors.colLayer0 : "transparent"
        radius: Config.options.bar.cornerStyle === 1 ? Appearance.rounding.windowRounding : 0
        border.width: Config.options.bar.cornerStyle === 1 ? 1 : 0
        border.color: Appearance.colors.colLayer0Border
    }

    FocusedScrollMouseArea { // Left side | scroll to change brightness
        id: barLeftSideMouseArea

        anchors {
            top: parent.top
            bottom: parent.bottom
            left: parent.left
            right: middleSection.left
        }
        implicitWidth: leftSectionRowLayout.implicitWidth
        implicitHeight: Appearance.sizes.baseBarHeight

        onScrollDown: root.brightnessMonitor.setBrightness(root.brightnessMonitor.brightness - 0.05)
        onScrollUp: root.brightnessMonitor.setBrightness(root.brightnessMonitor.brightness + 0.05)
        onMovedAway: GlobalStates.osdBrightnessOpen = false
        // Left-click anywhere on the left strip opens/closes the top menu
        // (no tab jump — batch 2's plain-toggle header click).
        onPressed: event => {
            if (event.button === Qt.LeftButton) {
                GlobalStates.topMenuOpen = !GlobalStates.topMenuOpen;
            }
        }

        // Visual content
        ScrollHint {
            reveal: barLeftSideMouseArea.hovered
            icon: "light_mode"
            tooltipText: Translation.tr("Scroll to change brightness")
            side: "left"
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
        }

        RowLayout {
            id: leftSectionRowLayout
            anchors.fill: parent
            // spacing dropped: ActiveWindow is the only child now that the
            // sidebarLeft removal took the other entries with it.

            ActiveWindow {
                visible: root.useShortenedForm === 0
                Layout.leftMargin: Appearance.rounding.screenRounding
                Layout.rightMargin: Appearance.rounding.screenRounding
                Layout.fillWidth: true
                Layout.fillHeight: true
            }
        }
    }

    RowLayout { // Middle section
        id: middleSection
        anchors {
            top: parent.top
            bottom: parent.bottom
            horizontalCenter: parent.horizontalCenter
        }
        spacing: 4

        BarGroup {
            id: leftCenterGroup
            Layout.preferredWidth: root.centerSideModuleWidth
            Layout.fillHeight: false

            Resources {
                alwaysShowAllResources: root.useShortenedForm === 2
                Layout.fillWidth: root.useShortenedForm === 2
            }

            ClaudeIndicator {
                visible: root.useShortenedForm < 2
            }

            Media {
                visible: root.useShortenedForm < 2
                Layout.fillWidth: true
                // Extra breathing room beyond BarGroup's default 4px
                // columnSpacing: without it, "No media" sits flush against
                // the Claude indicator with no visible gap.
                Layout.leftMargin: 12
            }
        }

        VerticalBarSeparator {
            visible: Config.options?.bar.borderless
        }

        BarGroup {
            id: middleCenterGroup
            padding: workspacesWidget.widgetPadding

            Workspaces {
                id: workspacesWidget
                Layout.fillHeight: true
                MouseArea {
                    // Right-click to toggle overview
                    anchors.fill: parent
                    acceptedButtons: Qt.RightButton

                    onPressed: event => {
                        if (event.button === Qt.RightButton) {
                            GlobalStates.overviewOpen = !GlobalStates.overviewOpen;
                        }
                    }
                }
            }
        }

        VerticalBarSeparator {
            visible: Config.options?.bar.borderless
        }

        MouseArea {
            id: rightCenterGroup
            implicitWidth: rightCenterGroupContent.implicitWidth
            implicitHeight: rightCenterGroupContent.implicitHeight
            Layout.preferredWidth: (root.useShortenedForm == 0) ? Appearance.sizes.barCenterSideModuleWidthClock : root.centerSideModuleWidth

            // Clicking the clock jumps straight to the top menu's Calendar
            // tab (batch 2), closing the menu again on a second click rather
            // than re-jumping tabs while it's already open. The right
            // sidebar keeps its own dedicated affordances elsewhere in this
            // bar (barRightSideMouseArea's left-click over the whole right
            // section, plus the explicit rightSidebarButton toggle button),
            // so this area doesn't need to double as a sidebarRight trigger
            // as well.
            onPressed: {
                if (GlobalStates.topMenuOpen) {
                    GlobalStates.topMenuOpen = false;
                } else {
                    GlobalStates.topMenuTab = "calendar";
                    GlobalStates.topMenuOpen = true;
                }
            }

            BarGroup {
                id: rightCenterGroupContent
                anchors.fill: parent

                // GridLayout has no built-in "center my content" option, so the
                // clock/buttons/battery group is centered by flanking it with
                // two equal fillWidth spacers instead of giving the clock
                // fillWidth itself (which used to stretch it across the whole
                // 480px pill instead of keeping it at its natural size).
                Item {
                    Layout.fillWidth: true
                }

                ClockWidget {
                    showDate: (Config.options.bar.verbose && root.useShortenedForm < 2)
                    Layout.alignment: Qt.AlignVCenter
                }

                UtilButtons {
                    visible: (Config.options.bar.verbose && root.useShortenedForm === 0)
                    Layout.alignment: Qt.AlignVCenter
                }

                BatteryIndicator {
                    visible: (root.useShortenedForm < 2 && UPower.displayDevice.isLaptopBattery)
                    Layout.alignment: Qt.AlignVCenter
                }

                Item {
                    Layout.fillWidth: true
                }
            }
        }
    }

    FocusedScrollMouseArea { // Right side | scroll to change volume
        id: barRightSideMouseArea

        // This area is anchored on both edges (left: middleSection.right,
        // right: parent.right), so its actual width is whatever's left over
        // after the two fixed-width 480px center pills, not its own
        // implicitWidth. When that leftover space is narrower than
        // rightSectionRowLayout's natural content width (sidebar button,
        // indicators, systray, weather), the row overflows — and because
        // layoutDirection is RightToLeft, the overflow pushes the leftmost
        // item (weather) out past this area's own left edge, over the
        // battery pill in rightCenterGroup. clip keeps that overflow
        // contained instead of letting it paint over adjacent content.
        clip: true

        anchors {
            top: parent.top
            bottom: parent.bottom
            left: middleSection.right
            right: parent.right
        }
        implicitWidth: rightSectionRowLayout.implicitWidth
        implicitHeight: Appearance.sizes.baseBarHeight

        onScrollDown: {
            const currentVolume = Audio.value;
            const step = currentVolume < 0.1 ? 0.01 : 0.02 || 0.2;
            Audio.sink.audio.volume -= step;
        }
        onScrollUp: {
            const currentVolume = Audio.value;
            const step = currentVolume < 0.1 ? 0.01 : 0.02 || 0.2;
            Audio.sink.audio.volume = Math.min(1, Audio.sink.audio.volume + step);
        }
        onMovedAway: GlobalStates.osdVolumeOpen = false
        onPressed: event => {
            if (event.button === Qt.LeftButton) {
                GlobalStates.sidebarRightOpen = !GlobalStates.sidebarRightOpen;
            }
        }

        // Visual content
        ScrollHint {
            reveal: barRightSideMouseArea.hovered
            icon: "volume_up"
            tooltipText: Translation.tr("Scroll to change volume")
            side: "right"
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
        }

        RowLayout {
            id: rightSectionRowLayout
            anchors.fill: parent
            spacing: 5
            layoutDirection: Qt.RightToLeft

            RippleButton { // Right sidebar button
                id: rightSidebarButton

                Layout.alignment: Qt.AlignRight | Qt.AlignVCenter
                Layout.rightMargin: Appearance.rounding.screenRounding
                Layout.fillWidth: false

                implicitWidth: indicatorsRowLayout.implicitWidth + 10 * 2
                implicitHeight: indicatorsRowLayout.implicitHeight + 5 * 2

                buttonRadius: Appearance.rounding.full
                colBackground: barRightSideMouseArea.hovered ? Appearance.colors.colLayer1Hover : ColorUtils.transparentize(Appearance.colors.colLayer1Hover, 1)
                colBackgroundHover: Appearance.colors.colLayer1Hover
                colRipple: Appearance.colors.colLayer1Active
                colBackgroundToggled: Appearance.colors.colSecondaryContainer
                colBackgroundToggledHover: Appearance.colors.colSecondaryContainerHover
                colRippleToggled: Appearance.colors.colSecondaryContainerActive
                toggled: GlobalStates.sidebarRightOpen
                property color colText: toggled ? Appearance.m3colors.m3onSecondaryContainer : Appearance.colors.colOnLayer0

                Behavior on colText {
                    animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
                }

                onPressed: {
                    GlobalStates.sidebarRightOpen = !GlobalStates.sidebarRightOpen;
                }

                RowLayout {
                    id: indicatorsRowLayout
                    anchors.centerIn: parent
                    property real realSpacing: 15
                    spacing: 0

                    Revealer {
                        reveal: Audio.source?.audio?.muted ?? false
                        Layout.fillHeight: true
                        Layout.rightMargin: reveal ? indicatorsRowLayout.realSpacing : 0
                        Behavior on Layout.rightMargin {
                            animation: Appearance.animation.elementMoveFast.numberAnimation.createObject(this)
                        }
                        MaterialSymbol {
                            text: "mic_off"
                            iconSize: Appearance.font.pixelSize.larger
                            color: rightSidebarButton.colText
                        }
                    }
                    Loader {
                        active: HyprlandXkb.layoutCodes.length > 1
                        visible: active
                        Layout.rightMargin: indicatorsRowLayout.realSpacing
                        sourceComponent: StyledText {
                            text: HyprlandXkb.currentLayoutCode
                            font.pixelSize: Appearance.font.pixelSize.small
                            color: rightSidebarButton.colText
                            animateChange: true
                        }
                    }
                    MaterialSymbol {
                        Layout.rightMargin: indicatorsRowLayout.realSpacing
                        text: Audio.materialSymbol
                        iconSize: Appearance.font.pixelSize.larger
                        color: rightSidebarButton.colText
                        property bool hovered: hhAudio.hovered
                        HoverHandler {
                            id: hhAudio
                        }

                        StyledToolTip {
                            // Your component reads parent.hovered via internalVisibleCondition
                            content: Audio.isMuted ? "Muted" : `Volume ${Audio.volumePercent}%`
                        }
                    }

                    MaterialSymbol {
                        Layout.rightMargin: indicatorsRowLayout.realSpacing
                        text: Network.materialSymbol
                        iconSize: Appearance.font.pixelSize.larger
                        color: rightSidebarButton.colText

                        property bool hovered: hhNet.hovered
                        HoverHandler {
                            id: hhNet
                        }

                        StyledToolTip {
                            content: Network.statusText ?? `Network: ${Network.networkName}`
                            // You can also add extra conditions:
                            // extraVisibleCondition: Network.connected
                        }
                    }
                    MaterialSymbol {
                        Layout.rightMargin: indicatorsRowLayout.realSpacing
                        text: BluetoothStatus.connected ? "bluetooth_connected" : BluetoothStatus.enabled ? "bluetooth" : "bluetooth_disabled"
                        iconSize: Appearance.font.pixelSize.larger
                        color: rightSidebarButton.colText
                        property bool hovered: hhBt.hovered
                        HoverHandler {
                            id: hhBt
                        }

                        StyledToolTip {
                            content: BluetoothStatus.connected ? `BT: ${BluetoothStatus.deviceName}` : BluetoothStatus.enabled ? "Bluetooth: On" : "Bluetooth: Off"
                        }
                    }
                    Item {
                        id: notifIcon
                        width: icon.implicitWidth
                        height: icon.implicitHeight

                        // Base icon (Material "notifications" / "notifications_none")
                        MaterialSymbol {
                            id: icon
                            anchors.centerIn: parent
                            text: Notifications.list.length > 0 ? "notifications" : "notifications_none"
                            iconSize: Appearance.font.pixelSize.larger
                            color: rightSidebarButton.colText

                            // optional hover tooltip
                            property bool hovered: hhNotif.hovered
                            HoverHandler {
                                id: hhNotif
                            }
                            StyledToolTip {
                                // e.g. “3 notifications” or “No notifications”
                                content: Notifications.list.length > 0 ? Translation.tr("%1 notifications").arg(Notifications.list.length) : Translation.tr("No notifications")
                            }
                        }

                        // Corner badge
                        Rectangle {
                            id: badge
                            visible: Notifications.list.length > 0

                            // Place at top-right of the icon's container, then push outward
                            anchors.top: parent.top
                            anchors.right: parent.right
                            // pull outward ~25% of badge size so it sits outside the bell corner
                            anchors.topMargin: -Math.round(width * 0.35)
                            anchors.rightMargin: -Math.round(width * 0.35)

                            // size grows with text; keep it circular
                            width: Math.max(badgeText.implicitWidth, badgeText.implicitHeight) + Math.round(icon.iconSize * 0.22)
                            height: width
                            radius: height / 2

                            color: Appearance.colors.accent ?? "#ff5555"
                            border.color: Qt.rgba(0, 0, 0, 0.15)
                            border.width: 1
                            layer.enabled: true

                            // draw above the icon (siblings only)
                            z: 10

                            Text {
                                id: badgeText
                                anchors.centerIn: parent
                                text: (Notifications.list.length > 99) ? "99+" : Notifications.list.length
                                // keep this big, but it will now sit outside the bell
                                font.pixelSize: Math.round(icon.iconSize * 0.62)
                                font.bold: false
                                color: "white"
                                horizontalAlignment: Text.AlignHCenter
                                verticalAlignment: Text.AlignVCenter
                                renderType: Text.NativeRendering
                            }
                        }
                        // Accessibility name (screen readers)
                        Accessible.name: (Notifications.list.length > 0) ? Translation.tr("%1 notifications").arg(Notifications.list.length) : Translation.tr("No notifications")
                        Accessible.role: Accessible.Button
                    }
                }
            }

            SysTray {
                visible: root.useShortenedForm === 0
                Layout.fillWidth: false
                Layout.fillHeight: true
                invertSide: Config?.options.bar.bottom
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true
            }

            // Weather
            Loader {
                Layout.leftMargin: 4
                active: Config.options.bar.weather.enable

                sourceComponent: BarGroup {
                    WeatherBar {}
                }
            }
        }
    }
}
