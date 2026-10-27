import QtQuick
import QtTest
import ShellUnderTest

// SystemInfo.qml and DateTime.qml against the stand-in Backend of fakes/Backend.qml.
// Run it through backend_tests.sh, which builds the ShellUnderTest module.
TestCase {
    id: tc
    name: "SystemClients"

    property double rev: 0

    function frame(data) {
        tc.rev += 1;
        return {
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            data: data
        };
    }

    function archOs() {
        return {
            id: "arch",
            name: "Arch Linux",
            pretty_name: "Arch Linux",
            home_url: "https://archlinux.org/",
            documentation_url: "https://wiki.archlinux.org/",
            support_url: "https://bbs.archlinux.org/",
            bug_report_url: "https://gitlab.archlinux.org/groups/archlinux/-/issues",
            privacy_policy_url: "https://terms.archlinux.org/docs/privacy-policy/",
            logo: "archlinux-logo"
        };
    }

    function sendOs(os) {
        SystemInfo.topic._apply(frame({
            user: "ryutribal",
            os: os,
            boot_at: null,
            resumed_at: null,
            clock_changed_at: null
        }));
    }

    function sendTimes(times) {
        DateTime.topic._apply(frame(times));
    }

    function init() {
        Backend.instance = "one";
        tc.rev = 0;
        SystemInfo.topic._rev = 0;
        DateTime.topic._rev = 0;
    }

    function test_systemInfoReadsTheTopic() {
        sendOs(archOs());
        compare(SystemInfo.distroName, "Arch Linux");
        compare(SystemInfo.distroIcon, "arch-symbolic");
        compare(SystemInfo.logo, "archlinux-logo");
        compare(SystemInfo.homeUrl, "https://archlinux.org/");
        compare(SystemInfo.documentationUrl, "https://wiki.archlinux.org/");
        compare(SystemInfo.supportUrl, "https://bbs.archlinux.org/");
        compare(SystemInfo.bugReportUrl, "https://gitlab.archlinux.org/groups/archlinux/-/issues");
        compare(SystemInfo.privacyPolicyUrl, "https://terms.archlinux.org/docs/privacy-policy/");
        compare(SystemInfo.username, "ryutribal");
    }

    function test_systemInfoFallsBackWhenFieldsAreNull() {
        sendOs({
            id: "gentoo",
            name: "Gentoo Linux",
            pretty_name: null,
            home_url: null,
            documentation_url: null,
            support_url: null,
            bug_report_url: null,
            privacy_policy_url: null,
            logo: null
        });
        compare(SystemInfo.distroName, "Gentoo");
        compare(SystemInfo.distroIcon, "linux-symbolic");
        compare(SystemInfo.logo, "linux-symbolic");
        compare(SystemInfo.homeUrl, "");
        compare(SystemInfo.privacyPolicyUrl, "");
    }

    function test_systemInfoBlankLogoFallsBackToTheIcon() {
        const os = archOs();
        os.logo = "  \n ";
        sendOs(os);
        compare(SystemInfo.logo, "arch-symbolic");
        os.logo = " archlinux-logo ";
        sendOs(os);
        compare(SystemInfo.logo, "archlinux-logo");
    }

    function test_systemInfoIconByDistroFamily() {
        const families = {
            ubuntu: "ubuntu-symbolic",
            popos: "ubuntu-symbolic",
            kali: "debian-symbolic",
            nixos: "nixos-symbolic"
        };
        for (const id in families) {
            sendOs({
                id: id,
                name: id,
                pretty_name: id
            });
            compare(SystemInfo.distroIcon, families[id], id);
        }
    }

    function test_systemInfoNyarchWinsOverTheId() {
        sendOs({
            id: "arch",
            name: "Arch Linux",
            pretty_name: "NyArch Linux"
        });
        compare(SystemInfo.distroIcon, "nyarch-symbolic");
        compare(SystemInfo.logo, "nyarch-symbolic");
    }

    function test_uptimeIsDashesUntilBootAtIsKnown() {
        sendTimes({
            boot_at: null,
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "--");
    }

    function test_uptimeFollowsBootAt() {
        const minuteMs = 60000;
        const offsetMs = ((24 + 1) * 60 + 1) * minuteMs + 30000;
        sendTimes({
            boot_at: Date.now() - offsetMs,
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "1d, 1h, 1m");
        sendTimes({
            boot_at: null,
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "--");
    }

    // Review F6: the uptime rolls over at boot_at + k minutes, not at the next wall-clock minute.
    function test_uptimeRollsOnBootAtMinutesNotWallMinutes() {
        const minuteMs = 60000;
        sendTimes({
            boot_at: Date.now() - (2 * minuteMs - 300),
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "1m");
        tryCompare(DateTime, "uptime", "2m", 2000);
        // and it keeps rolling from there: the next whole minute is a minute after the first roll
        sendTimes({
            boot_at: null,
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "--");
        wait(400);
        compare(DateTime.uptime, "--", "no boot_at, no timer");
    }

    function test_uptimeNeverNegativeWhenBootAtIsInTheFuture() {
        sendTimes({
            boot_at: Date.now() + 3600000,
            resumed_at: null,
            clock_changed_at: null
        });
        compare(DateTime.uptime, "0m");
    }

    function test_clockIsTheCurrentMinute() {
        const now = Date.now();
        const minuteStart = now - now % 60000;
        const shown = DateTime.clock.date.getTime();
        verify(shown === minuteStart || shown === minuteStart - 60000, "clock.date " + shown);
        compare(DateTime.clock.hours, DateTime.clock.date.getHours());
        compare(DateTime.clock.minutes, DateTime.clock.date.getMinutes());
    }

    function test_resumeAndClockStepReArmTheMinuteTimer() {
        const times = {
            boot_at: null,
            resumed_at: 5,
            clock_changed_at: null
        };
        sendTimes(times);
        const afterResume = DateTime._ticks;
        sendTimes(times);
        compare(DateTime._ticks, afterResume, "an unchanged snapshot must not re-arm");
        times.clock_changed_at = 6;
        sendTimes(times);
        compare(DateTime._ticks, afterResume + 1, "a clock step must re-arm");
        times.boot_at = 1000;
        sendTimes(times);
        compare(DateTime._ticks, afterResume + 1, "a boot_at change alone must not re-arm");
        times.resumed_at = 7;
        sendTimes(times);
        compare(DateTime._ticks, afterResume + 2, "a second resume must re-arm");
        compare(DateTime.time, Qt.locale().toString(DateTime.clock.date, "hh:mm"));
    }
}
