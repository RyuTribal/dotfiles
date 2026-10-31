pragma Singleton
pragma ComponentBehavior: Bound

import "backend.js" as B
import QtQuick
import Quickshell

/**
 * CPU, memory, swap and disk usage, a thin client of axon's `resources` topic
 * (section 32, "resources"). axon samples /proc and hwmon only while the topic
 * has a subscriber, and the bar always is one. The properties keep the names
 * and units the widgets bound before axon: memory and swap in kB, shares as
 * fractions, temperatures in °C. Until the first sample arrives they hold the
 * old service's initial values.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "resources"
        onSnapshot: data => root._apply(data)
    }

    readonly property double memoryTotal: _memoryTotal // kB
    readonly property double memoryFree: _memoryFree // kB, MemAvailable
    readonly property double memoryUsed: _memoryUsed // kB
    readonly property double memoryUsedPercentage: _memoryUsedPercentage // fraction
    readonly property double swapTotal: _swapTotal // kB
    readonly property double swapFree: _swapFree // kB
    readonly property double swapUsed: _swapUsed // kB
    readonly property double swapUsedPercentage: _swapUsedPercentage // fraction
    readonly property double cpuUsage: _cpuUsage // fraction
    readonly property real cpuTempC: _cpuTempC // °C, 0 when there is no sensor
    // True when cpuTempC comes from the CPU's own sensor (coretemp or k10temp).
    // False means it is the hottest of all hwmon sensors, which may be the nvme,
    // a RAM DIMM or the wifi card, or that there is no sensor at all; the
    // widgets label it "Peak temp" instead of "CPU temp".
    readonly property bool cpuTempNamed: _cpuTempNamed
    readonly property var mounts: _mounts // [{ id, rev, target, pcent, avail (kB) }]

    property double _memoryTotal: 1
    property double _memoryFree: 1
    property double _memoryUsed: 0
    property double _memoryUsedPercentage: 0
    property double _swapTotal: 1
    property double _swapFree: 1
    property double _swapUsed: 0
    property double _swapUsedPercentage: 0
    property double _cpuUsage: 0
    property real _cpuTempC: 0
    property bool _cpuTempNamed: true
    property var _mounts: []
    property var _mountIndex: ({})

    function _apply(data) {
        _memoryTotal = data.memory.total_bytes / 1024;
        _memoryFree = data.memory.available_bytes / 1024;
        _memoryUsed = data.memory.used_bytes / 1024;
        _memoryUsedPercentage = data.memory.used_pct / 100;
        _swapTotal = data.swap.total_bytes / 1024;
        _swapFree = data.swap.free_bytes / 1024;
        _swapUsed = data.swap.used_bytes / 1024;
        _swapUsedPercentage = data.swap.used_pct / 100;
        _cpuUsage = data.cpu.usage_pct / 100;
        _cpuTempC = data.cpu.temp_c ?? 0;
        _cpuTempNamed = data.cpu.temp_sensor === "coretemp" || data.cpu.temp_sensor === "k10temp";
        _applyMounts(data.mounts);
    }

    // The mount rows change only when axon reads the mounts again (every 60 s
    // and on a mount table change), not on every sample, so the array is kept
    // while every row is the same object. SystemPanel's Repeater takes `mounts`
    // as a plain array and would otherwise rebuild its rows on each sample.
    function _applyMounts(wireRows) {
        const rows = wireRows.map(row => ({
                    id: row.id,
                    rev: row.rev,
                    target: row.target,
                    pcent: row.used_pct,
                    avail: row.avail_bytes / 1024
                }));
        const reused = B.reuseRows(_mountIndex, rows);
        _mountIndex = reused.index;
        const unchanged = reused.rows.length === _mounts.length && reused.rows.every((row, index) => row === _mounts[index]);
        if (!unchanged)
            _mounts = reused.rows;
    }
}
