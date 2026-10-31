import QtQuick
import QtTest
import ShellUnderTest

// ResourceUsage.qml against the stand-in Backend of fakes/Backend.qml: a `resources` snapshot comes
// out in the names and units the widgets bound before axon (section 32, the Parity table). Run it
// through backend_tests.sh, which builds the ShellUnderTest module.
TestCase {
    id: tc
    name: "ResourceUsage"

    property double rev: 0

    SignalSpy {
        id: mountsChanged
        target: ResourceUsage
        signalName: "mountsChanged"
    }

    function send(data) {
        tc.rev += 1;
        ResourceUsage.topic._apply({
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            data: data
        });
    }

    function rootMount() {
        return {
            id: "/",
            rev: 5502173894017366,
            target: "/",
            device: "/dev/nvme0n1p2",
            fstype: "ext4",
            size_bytes: 1006000000000,
            used_bytes: 578676350000,
            avail_bytes: 377023650000,
            used_pct: 61
        };
    }

    function bootMount() {
        return {
            id: "/boot",
            rev: 1180442793356021,
            target: "/boot",
            device: "/dev/nvme0n1p1",
            fstype: "vfat",
            size_bytes: 1071640576,
            used_bytes: 289342955,
            avail_bytes: 782297621,
            used_pct: 27
        };
    }

    // The example snapshot of section 32, "resources", with the given cpu and mounts.
    function sample(cpu, mounts) {
        return {
            interval_ms: 3000,
            cpu: cpu,
            memory: {
                total_bytes: 16535624089,
                available_bytes: 9985798963,
                used_bytes: 6549825126,
                used_pct: 39.6
            },
            swap: {
                total_bytes: 4294963200,
                free_bytes: 2899098829,
                used_bytes: 1395864371,
                used_pct: 32.5
            },
            mounts: mounts,
            mounts_observed_at: 1792922400000
        };
    }

    function cpu(usagePct, tempC, sensor) {
        return {
            usage_pct: usagePct,
            temp_c: tempC,
            temp_sensor: sensor
        };
    }

    // Runs once, before the first test function. ResourceUsage is a singleton, so this is the only
    // place where no snapshot has arrived yet.
    function initTestCase() {
        compare(ResourceUsage.topic.name, "resources");
        compare(ResourceUsage.memoryTotal, 1);
        compare(ResourceUsage.memoryFree, 1);
        compare(ResourceUsage.memoryUsed, 0);
        compare(ResourceUsage.memoryUsedPercentage, 0);
        compare(ResourceUsage.swapTotal, 1);
        compare(ResourceUsage.swapFree, 1);
        compare(ResourceUsage.swapUsed, 0);
        compare(ResourceUsage.swapUsedPercentage, 0);
        compare(ResourceUsage.cpuUsage, 0);
        compare(ResourceUsage.cpuTempC, 0);
        compare(ResourceUsage.cpuTempNamed, true);
        compare(ResourceUsage.mounts.length, 0);
    }

    // Memory and swap in kB, shares as fractions, the mount rows as SystemPanel reads them.
    function test_aSnapshotMapsToTheParityTable() {
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount(), bootMount()]));
        compare(ResourceUsage.memoryTotal, 16535624089 / 1024);
        compare(ResourceUsage.memoryFree, 9985798963 / 1024);
        compare(ResourceUsage.memoryUsed, 6549825126 / 1024);
        compare(ResourceUsage.memoryUsedPercentage, 0.396);
        compare(ResourceUsage.swapTotal, 4294963200 / 1024);
        compare(ResourceUsage.swapFree, 2899098829 / 1024);
        compare(ResourceUsage.swapUsed, 1395864371 / 1024);
        compare(ResourceUsage.swapUsedPercentage, 0.325);
        compare(ResourceUsage.cpuUsage, 0.12);
        compare(ResourceUsage.cpuTempC, 54);
        compare(ResourceUsage.cpuTempNamed, true);
        compare(ResourceUsage.mounts.length, 2);
        const rootRow = ResourceUsage.mounts[0];
        compare(rootRow.id, "/");
        compare(rootRow.rev, 5502173894017366);
        compare(rootRow.target, "/");
        compare(rootRow.pcent, 61);
        compare(rootRow.avail, 377023650000 / 1024);
        compare(ResourceUsage.mounts[1].target, "/boot");
        compare(ResourceUsage.mounts[1].pcent, 27);
    }

    function test_k10tempIsTheCpusOwnSensor() {
        send(sample(cpu(1.0, 41.5, "k10temp"), [rootMount()]));
        compare(ResourceUsage.cpuTempC, 41.5);
        compare(ResourceUsage.cpuTempNamed, true);
    }

    // The hottest of all sensors may be the nvme or a RAM DIMM, so the widgets say "Peak temp".
    function test_theFallbackSensorIsNotNamed() {
        send(sample(cpu(1.0, 70.0, "fallback"), [rootMount()]));
        compare(ResourceUsage.cpuTempC, 70);
        compare(ResourceUsage.cpuTempNamed, false);
    }

    function test_noSensorReadsAsZeroDegrees() {
        send(sample(cpu(1.0, 70.0, "coretemp"), [rootMount()]));
        send(sample(cpu(1.0, null, "none"), [rootMount()]));
        compare(ResourceUsage.cpuTempC, 0);
        compare(ResourceUsage.cpuTempNamed, false);
    }

    // The mount rows change only when axon reads the mounts again, so a sample that leaves them as
    // they were keeps the array, and SystemPanel's Repeater keeps its rows.
    function test_unchangedMountRowsKeepTheArray() {
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount(), bootMount()]));
        const before = ResourceUsage.mounts;
        mountsChanged.clear();
        send(sample(cpu(50.0, 60.0, "coretemp"), [rootMount(), bootMount()]));
        compare(ResourceUsage.cpuUsage, 0.5);
        verify(ResourceUsage.mounts === before, "the same array while no mount row changed");
        compare(mountsChanged.count, 0);
    }

    function test_aChangedMountRowReplacesOnlyThatRow() {
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount(), bootMount()]));
        const before = ResourceUsage.mounts;
        mountsChanged.clear();
        const boot = bootMount();
        boot.rev = 7;
        boot.used_pct = 28;
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount(), boot]));
        compare(mountsChanged.count, 1);
        verify(ResourceUsage.mounts[0] === before[0], "the unchanged row keeps its object");
        verify(ResourceUsage.mounts[1] !== before[1], "the changed row is a new object");
        compare(ResourceUsage.mounts[1].pcent, 28);
    }

    function test_aMountThatGoesAwayLeavesTheList() {
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount(), bootMount()]));
        mountsChanged.clear();
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount()]));
        compare(mountsChanged.count, 1);
        compare(ResourceUsage.mounts.length, 1);
        compare(ResourceUsage.mounts[0].target, "/");
    }

    // An error frame carries no data, so the values stay as the last snapshot left them.
    function test_aDataNullFrameKeepsTheValues() {
        send(sample(cpu(12.0, 54.0, "coretemp"), [rootMount()]));
        tc.rev += 1;
        ResourceUsage.topic._apply({
            rev: tc.rev,
            status: "error",
            source: "live",
            observed_at: 2,
            data: null,
            error: {
                code: "internal",
                message: "the resources engine stopped"
            }
        });
        compare(ResourceUsage.topic.status, "error");
        compare(ResourceUsage.cpuUsage, 0.12);
        compare(ResourceUsage.memoryTotal, 16535624089 / 1024);
        compare(ResourceUsage.mounts.length, 1);
    }
}
