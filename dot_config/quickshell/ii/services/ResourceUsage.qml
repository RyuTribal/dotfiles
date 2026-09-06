pragma Singleton
pragma ComponentBehavior: Bound

import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Io

/**
 * Simple polled resource usage service with RAM, Swap, and CPU usage.
 */
Singleton {
	property double memoryTotal: 1
	property double memoryFree: 1
	property double memoryUsed: memoryTotal - memoryFree
    property double memoryUsedPercentage: memoryUsed / memoryTotal
    property double swapTotal: 1
	property double swapFree: 1
	property double swapUsed: swapTotal - swapFree
    property double swapUsedPercentage: swapTotal > 0 ? (swapUsed / swapTotal) : 0
    property double cpuUsage: 0
    property var previousCpuStats

    // Temperature and disk usage. Both are heavier to gather (spawn a
    // shell/df process) than the /proc reads above, but the bar's stat
    // circles (batch 2) need them live even with the top menu closed, so
    // both now poll unconditionally instead of being gated to the Stats tab
    // being visible — temp every 5s (cheap sysfs reads), disk every 60s
    // (df is heavier and doesn't need to be nearly as fresh).
    property real cpuTempC: 0
    // True when cpuTempC came from a named CPU sensor (coretemp/k10temp);
    // false means no such sensor exists and cpuTempC is the fallback
    // max-of-all-hwmon reading, which may not actually be the CPU (could be
    // nvme, a RAM DIMM, wifi, etc.) — SystemPanel labels the row
    // accordingly ("CPU temp" vs "Peak temp").
    property bool cpuTempNamed: true
    property var mounts: [] // [{target, pcent, avail}]

	Timer {
		interval: 1
        running: true 
        repeat: true
		onTriggered: {
            // Reload files
            fileMeminfo.reload()
            fileStat.reload()

            // Parse memory and swap usage
            const textMeminfo = fileMeminfo.text()
            memoryTotal = Number(textMeminfo.match(/MemTotal: *(\d+)/)?.[1] ?? 1)
            memoryFree = Number(textMeminfo.match(/MemAvailable: *(\d+)/)?.[1] ?? 0)
            swapTotal = Number(textMeminfo.match(/SwapTotal: *(\d+)/)?.[1] ?? 1)
            swapFree = Number(textMeminfo.match(/SwapFree: *(\d+)/)?.[1] ?? 0)

            // Parse CPU usage
            const textStat = fileStat.text()
            const cpuLine = textStat.match(/^cpu\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)/)
            if (cpuLine) {
                const stats = cpuLine.slice(1).map(Number)
                const total = stats.reduce((a, b) => a + b, 0)
                const idle = stats[3]

                if (previousCpuStats) {
                    const totalDiff = total - previousCpuStats.total
                    const idleDiff = idle - previousCpuStats.idle
                    cpuUsage = totalDiff > 0 ? (1 - idleDiff / totalDiff) : 0
                }

                previousCpuStats = { total, idle }
            }
            interval = Config.options?.resources?.updateInterval ?? 3000
        }
	}

	FileView { id: fileMeminfo; path: "/proc/meminfo" }
    FileView { id: fileStat; path: "/proc/stat" }

    // Temp polling: always on, 5s cadence (matches the CPU/RAM timer's
    // ballpark) — the bar's CPU-temp circle needs this even when the top
    // menu is closed.
    Timer {
        interval: 5000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: tempProc.running = true
    }

    // Disk polling: always on, but on a slower 60s cadence. Sourced from
    // sweepd (the sweep disk-tool daemon, ~/.config/sweep) over its Unix
    // socket instead of spawning df — one shared backend for the bar's disk
    // stats and the SweepPanel inspector. Falls back to spawning the daemon
    // if it isn't running yet.
    Timer {
        interval: 60000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: {
            if (sweepSock.connected) {
                sweepSock.write('{"op":"mounts"}\n');
                sweepSock.flush();
            } else {
                sweepSock.connected = true;
            }
        }
    }

    Process {
        id: sweepdProc
        command: ["sweepd"]
        running: false
    }

    Timer {
        id: sweepRetryTimer
        interval: 700
        onTriggered: sweepSock.connected = true
    }

    Socket {
        id: sweepSock
        path: Quickshell.env("XDG_RUNTIME_DIR") + "/sweep.sock"
        connected: false
        parser: SplitParser {
            onRead: msg => {
                let r;
                try { r = JSON.parse(msg); } catch (e) { return; }
                if (r.mounts === undefined) return; // not for us
                // Map daemon bytes to the df-shaped {target, pcent, avail(KB)}
                // rows the bar/Stats consumers already expect.
                mounts = r.mounts.map(m => ({
                    target: m.target,
                    pcent: m.pcent,
                    avail: m.avail / 1024
                }));
            }
        }
        onConnectionStateChanged: {
            if (connected) {
                sweepSock.write('{"op":"mounts"}\n');
                sweepSock.flush();
            }
        }
        onError: {
            sweepdProc.running = true;
            sweepRetryTimer.start();
        }
    }

    // Prefers the hwmon device actually named "coretemp" (Intel) or
    // "k10temp" (AMD) — that device's temp1_input is the CPU package temp
    // (confirmed via temp1_label on this machine: "Package id 0"). Only
    // when neither exists does this fall back to the old max-of-all-hwmon
    // reading, which isn't guaranteed to be the CPU (nvme/RAM/wifi sensors
    // can read hotter). Output is "named:<millidegrees>" or
    // "fallback:<millidegrees>" so the QML side knows which case fired.
    Process {
        id: tempProc
        command: ["bash", "-c", "for d in /sys/class/hwmon/hwmon*; do n=$(cat \"$d/name\" 2>/dev/null); if [ \"$n\" = coretemp ] || [ \"$n\" = k10temp ]; then v=$(cat \"$d/temp1_input\" 2>/dev/null); if [ -n \"$v\" ]; then echo \"named:$v\"; exit 0; fi; fi; done; v=$(cat /sys/class/hwmon/hwmon*/temp1_input 2>/dev/null | sort -rn | head -1); echo \"fallback:$v\""]
        stdout: StdioCollector {
            id: tempCollector
            onStreamFinished: {
                const match = tempCollector.text.trim().match(/^(named|fallback):(\d+)$/)
                if (match) {
                    cpuTempNamed = match[1] === "named"
                    cpuTempC = Number(match[2]) / 1000
                }
            }
        }
    }

    // (df-based diskProc removed — mounts now come from sweepd above.)
}
