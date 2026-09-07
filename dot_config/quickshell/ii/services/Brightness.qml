pragma Singleton
pragma ComponentBehavior: Bound

// From https://github.com/caelestia-dots/shell with modifications.
// License: GPLv3

import Quickshell
import Quickshell.Io
import Quickshell.Hyprland
import QtQuick

/**
 * For managing brightness of monitors. Supports both brightnessctl and ddcutil.
 */
Singleton {
    id: root

    signal brightnessChanged()

    property var ddcMonitors: []
    readonly property list<BrightnessMonitor> monitors: Quickshell.screens.map(screen => monitorComp.createObject(root, {
        screen
    }))

    function getMonitorForScreen(screen: ShellScreen): var {
        return monitors.find(m => m.screen === screen);
    }

    function increaseBrightness(): void {
        const focusedName = Hyprland.focusedMonitor.name;
        const monitor = monitors.find(m => focusedName === m.screen.name);
        if (monitor)
            monitor.setBrightness(monitor.brightness + 0.05);
    }

    function decreaseBrightness(): void {
        const focusedName = Hyprland.focusedMonitor.name;
        const monitor = monitors.find(m => focusedName === m.screen.name);
        if (monitor)
            monitor.setBrightness(monitor.brightness - 0.05);
    }

    reloadableId: "brightness"

    onMonitorsChanged: {
        ddcMonitors = [];
        ddcProc.running = true;
    }

    Process {
        id: ddcProc

        command: ["ddcutil", "detect", "--brief"]
        stdout: SplitParser {
            splitMarker: "\n\n"
            onRead: data => {
                if (data.startsWith("Display ")) {
                    const lines = data.split("\n").map(l => l.trim());
                    root.ddcMonitors.push({
                        model: lines.find(l => l.startsWith("Monitor:")).split(":")[2],
                        busNum: lines.find(l => l.startsWith("I2C bus:")).split("/dev/i2c-")[1]
                    });
                }
            }
        }
        onExited: root.ddcMonitorsChanged()
    }

    component BrightnessMonitor: QtObject {
        id: monitor

        required property ShellScreen screen
        readonly property bool isDdc: root.ddcMonitors.some(m => m.model === screen.model)
        readonly property string busNum: root.ddcMonitors.find(m => m.model === screen.model)?.busNum ?? ""
        property int rawMaxBrightness: 100
        property real brightness
        property bool ready: false

        // Coalescing state for hardware writes: pendingTarget/pendingValue is the
        // latest value anyone has asked for (updated on every scroll tick);
        // lastSentTarget/lastSentValue is what the in-flight/just-finished setProc
        // call was asked to reach. When the pending and last-sent targets differ
        // after a write completes, one more write is fired with the latest target,
        // skipping every intermediate step from a fast scroll burst.
        property int pendingTarget: -1
        property real pendingValue: 0
        property int lastSentTarget: -1
        property real lastSentValue: 0

        onBrightnessChanged: {
            if (monitor.ready) {
                root.brightnessChanged();
            }
        }

        function initialize() {
            monitor.ready = false;
            initProc.command = isDdc ? ["ddcutil", "-b", busNum, "getvcp", "10", "--brief"] : ["sh", "-c", `echo "a b c $(brightnessctl g) $(brightnessctl m)"`];
            initProc.running = true;
        }

        readonly property Process initProc: Process {
            stdout: SplitParser {
                onRead: data => {
                    const [, , , current, max] = data.split(" ");
                    monitor.rawMaxBrightness = parseInt(max);
                    monitor.brightness = parseInt(current) / monitor.rawMaxBrightness;
                    monitor.ready = true;
                }
            }
        }

        function writeToHardware(rounded: int, value: real): void {
            monitor.lastSentTarget = rounded;
            monitor.lastSentValue = value;
            setProc.command = isDdc ? ["ddcutil", "-b", busNum, "setvcp", "10", rounded] : ["brightnessctl", "s", rounded, "--quiet"];
            setProc.running = true;
        }

        function setBrightness(value: real): void {
            value = Math.max(0.01, Math.min(1, value));
            const rounded = Math.round(value * monitor.rawMaxBrightness);
            if (Math.round(brightness * monitor.rawMaxBrightness) === rounded)
                return;

            monitor.pendingTarget = rounded;
            monitor.pendingValue = value;

            // DDC writes are slow (100-300ms of serialized I2C over ddcutil), so
            // update the property optimistically: it (and anything bound to it,
            // e.g. the OSD) moves instantly instead of crawling behind the writes.
            // brightnessctl is effectively instant already, so that path is left
            // to update brightness from the write itself, in onExited below.
            if (monitor.isDdc)
                brightness = value;

            // Only one hardware write in flight at a time. If one is already
            // running, its onExited handler will pick up the latest pendingTarget
            // once it finishes instead of another process being spawned now.
            if (setProc.running)
                return;

            monitor.writeToHardware(rounded, value);
        }

        readonly property Process setProc: Process {
            onExited: (exitCode, exitStatus) => {
                if (exitCode !== 0) {
                    // Write failed, so the property may have diverged from
                    // reality (optimistic update on the ddc path, or simply a
                    // failed brightnessctl call). Re-read the actual hardware
                    // state and correct it.
                    monitor.initialize();
                    return;
                }

                if (!monitor.isDdc) {
                    // Non-ddc path: the property follows the write that just
                    // actually completed, rather than being set ahead of it.
                    monitor.brightness = monitor.lastSentValue;
                }

                if (monitor.pendingTarget !== -1 && monitor.pendingTarget !== monitor.lastSentTarget)
                    monitor.writeToHardware(monitor.pendingTarget, monitor.pendingValue);
            }
        }

        Component.onCompleted: {
            initialize();
        }

        onBusNumChanged: {
            initialize();
        }
    }

    Component {
        id: monitorComp

        BrightnessMonitor {}
    }

    IpcHandler {
        target: "brightness"

        function increment() {
            onPressed: root.increaseBrightness()
        }

        function decrement() {
            onPressed: root.decreaseBrightness()
        }
    }

    GlobalShortcut {
        name: "brightnessIncrease"
        description: "Increase brightness"
        onPressed: root.increaseBrightness()
    }

    GlobalShortcut {
        name: "brightnessDecrease"
        description: "Decrease brightness"
        onPressed: root.decreaseBrightness()
    }
}
