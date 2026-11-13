import QtQuick
import QtTest
import ShellUnderTest
import qs

// The real services/Notifications.qml over the stand-in Backend of fakes/Backend.qml and the stand-in
// GlobalStates of fakes/qs: the Parity names built from a `notify` snapshot, the calls the functions
// send, the sidebar rule and the one-shot popup timer. backend_tests.sh copies Notifications.qml
// without its `pragma Singleton` line into ShellUnderTest as NotificationsService, so each test makes
// its own instance. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "NotificationsService"

    Component {
        id: serviceComponent
        NotificationsService {}
    }

    property double rev: 0
    readonly property double discordTime: 1792922457000
    // The wall clock the services under test read: real time plus a shift a test sets, so the fixed
    // example times of section 31 can be judged and the clock still runs. The shift sits in a plain
    // object, so moving it notifies no binding, as a real wall-clock jump does not.
    readonly property var wallState: ({
            shift: 0
        })

    function wallNow() {
        return Date.now() + tc.wallState.shift;
    }

    function setWall(milliseconds) {
        tc.wallState.shift = milliseconds - Date.now();
    }

    function makeService() {
        return createTemporaryObject(serviceComponent, tc, {
            wallClock: () => tc.wallNow()
        });
    }

    property int clockReads: 0

    // A service whose clock gives `readings` first, one per read, then the running test clock.
    function makeScriptedService(readings) {
        return createTemporaryObject(serviceComponent, tc, {
            wallClock: () => {
                tc.clockReads += 1;
                return readings.length > 0 ? readings.shift() : tc.wallNow();
            }
        });
    }

    function init() {
        tc.clockReads = 0;
        tc.setWall(tc.discordTime);
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = tc.discordTime;
        Backend.downSince = 0;
        GlobalStates.sidebarRightOpen = false;
        tc.rev = 0;
    }

    // Section 31's example rows, with popup_at.
    function discordRow(overrides) {
        return Object.assign({
            id: "2701",
            rev: 1,
            app_name: "discord",
            app_icon: "discord",
            desktop_entry: "discord",
            summary: "Anna",
            body: "are you coming at 12?",
            image: "/home/ryutribal/.local/state/axon/notify/images/9c41d2e0a7b35f18e62c04b9d7a13f85.png",
            urgency: "normal",
            time: 1792922457000,
            actions: [
                {
                    id: "default",
                    label: "Open"
                }
            ],
            popup: true,
            popup_at: 1792922457000,
            popup_until: 1792922462000,
            held: false,
            transient: false
        }, overrides ?? {});
    }

    function kittyRow(overrides) {
        return Object.assign({
            id: "2699",
            rev: 2,
            app_name: "kitty",
            app_icon: "kitty",
            desktop_entry: "kitty",
            summary: "cargo test",
            body: "Command finished in 2m 13s",
            image: "",
            urgency: "low",
            time: 1792922220000,
            actions: [],
            popup: false,
            popup_at: null,
            popup_until: null,
            held: false,
            transient: false
        }, overrides ?? {});
    }

    function frame(service, items, dnd) {
        tc.rev += 1;
        service.topic._apply({
            t: "snapshot",
            topic: "notify",
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                server: {
                    name: "org.freedesktop.Notifications",
                    owned: true
                },
                dnd: dnd ?? false,
                count: items.length,
                items: items
            }
        });
    }

    function test_parityNamesFromSnapshot() {
        const service = makeService();
        frame(service, [discordRow(), kittyRow()]);
        compare(service.list.length, 2);
        compare(service.list[0].notificationId, "2701");
        compare(service.appNameList, ["discord", "kitty"]);
        compare(service.silent, false);
        compare(service.serverOwned, true);
        compare(service.latestTimeForApp.discord, 1792922457000);
        compare(service.groupsByAppName.kitty.notifications.length, 1);
        compare(service.popupList.length, 1);
        compare(service.popupAppNameList, ["discord"]);
        compare(service.popupGroupsByAppName.discord.appName, "discord");
        frame(service, [kittyRow()], true);
        compare(service.silent, true);
        compare(service.latestTimeForApp.discord, undefined);
    }

    function test_callsCarryTheWireArgs() {
        const service = makeService();
        const cases = [
            {
                run: () => service.discardNotification("2701"),
                method: "notify.dismiss",
                args: {
                    id: "2701"
                }
            },
            {
                run: () => service.discardAllNotifications(),
                method: "notify.dismiss_all",
                args: {}
            },
            {
                run: () => service.timeoutAll(),
                method: "notify.expire_popups",
                args: {}
            },
            {
                run: () => service.attemptInvokeAction("2701", "default"),
                method: "notify.invoke_action",
                args: {
                    id: "2701",
                    action: "default"
                }
            },
            {
                run: () => service.holdPopups(["2701", "2699"], true),
                method: "notify.hold_popups",
                args: {
                    ids: ["2701", "2699"],
                    held: true
                }
            },
            {
                run: () => service.setSilent(true),
                method: "notify.set_dnd",
                args: {
                    on: true
                }
            }
        ];
        for (const testCase of cases) {
            Backend.calls = [];
            testCase.run();
            compare(Backend.calls.length, 1, testCase.method);
            compare(Backend.calls[0].method, testCase.method);
            compare(Backend.calls[0].args, testCase.args);
        }
    }

    function test_sidebarOpenAndCloseEachExpire() {
        const service = makeService();
        GlobalStates.sidebarRightOpen = true;
        GlobalStates.sidebarRightOpen = false;
        const methods = Backend.calls.map(call => call.method);
        compare(methods, ["notify.expire_popups", "notify.expire_popups"]);
    }

    function test_popupListHidesWhileSidebarOpen() {
        const service = makeService();
        frame(service, [discordRow(), kittyRow()]);
        compare(service.popupList.length, 1);
        GlobalStates.sidebarRightOpen = true;
        compare(service.popupList.length, 0);
        compare(service.list.length, 2);
        GlobalStates.sidebarRightOpen = false;
        compare(service.popupList.length, 1);
    }

    // The one-shot timer is armed for 50 ms from the deadline: no new snapshot arrives.
    function test_popupTimerDropsAnExpiredRow() {
        const service = makeService();
        tc.setWall(1792922462000 - 50);
        frame(service, [discordRow()]);
        compare(service.popupList.length, 1);
        // popupList is an array value: tryCompare on it would poll the array read now, so poll the property.
        tryVerify(() => service.popupList.length === 0, 1000);
        compare(service.list.length, 1);
    }

    // A queued popup's popup_at is ahead of the frame: the engine does not republish when it arrives.
    function test_queuedPopupAppearsWithoutANewSnapshot() {
        const service = makeService();
        const wall = tc.wallNow();
        frame(service, [discordRow({
                time: wall,
                popup_at: wall + 50,
                popup_until: wall + 5050
            })]);
        compare(service.list.length, 1);
        compare(service.popupList.length, 0);
        tryVerify(() => service.popupList.length === 1, 1000);
    }

    // A safety net: the engine sends no frame at popup_at, so after a wall-clock jump or a missed
    // fire each Backend.now tick re-reads the wall clock. The timer was armed for 5 s; the clock
    // jumps past popup_at and one tick shows the popup without waiting for that interval.
    function test_backendTickRereadsTheWallClock() {
        const service = makeService();
        const wall = tc.wallNow();
        frame(service, [discordRow({
                time: wall,
                popup_at: wall + 5000,
                popup_until: wall + 10000
            })]);
        compare(service.popupList.length, 0);
        tc.setWall(wall + 6000);
        wait(100);
        compare(service.popupList.length, 0); // nothing moves until the tick
        Backend.now += 1000;
        tryVerify(() => service.popupList.length === 1, 200);
    }

    // A fire can read the wall clock a little before the deadline (a timer that fires early, a clock
    // that was stepped back): the popup must still end, with no tick and no new frame.
    function test_earlyFireStillEndsThePopup() {
        const deadline = 1792922462000;
        tc.setWall(deadline - 50);
        const service = makeScriptedService([deadline - 50, deadline - 30]); // the frame, then the fire
        frame(service, [discordRow()]);
        compare(service.popupList.length, 1);
        tryVerify(() => service.popupList.length === 0, 300);
    }

    // The same for a popup that starts: the fire reads 30 ms before popup_at, and it still shows.
    function test_earlyFireStillShowsTheQueuedPopup() {
        const start = 1792922457500;
        tc.setWall(start - 50);
        const service = makeScriptedService([start - 50, start - 30]);
        frame(service, [discordRow({
                popup_at: start,
                popup_until: start + 5000
            })]);
        compare(service.popupList.length, 0);
        tryVerify(() => service.popupList.length === 1, 300);
    }

    // A frozen clock never reaches the deadline, but each fire arms the timer again, so the popup
    // ends within about 50 ms of the clock moving on, without a tick.
    function test_frozenClockFireArmsAgain() {
        const deadline = 1792922462000;
        let frozen = true;
        const service = createTemporaryObject(serviceComponent, tc, {
            wallClock: () => {
                tc.clockReads += 1;
                return frozen ? deadline - 50 : deadline + 10;
            }
        });
        frame(service, [discordRow()]);
        wait(300);
        verify(tc.clockReads >= 4, "reads " + tc.clockReads);
        compare(service.popupList.length, 1);
        frozen = false;
        tryVerify(() => service.popupList.length === 0, 200);
    }

    // A backward wall-clock jump (NTP, date -s) must not keep earlier readings: the engine stamps new
    // rows with the new time, and they show.
    function test_backwardClockJumpDoesNotHideNewPopups() {
        const service = makeService();
        frame(service, [discordRow()]);
        compare(service.popupList.length, 1);
        tc.setWall(tc.discordTime - 3600 * 1000);
        const wall = tc.wallNow();
        frame(service, [discordRow({
                id: "2800",
                rev: 9,
                time: wall,
                popup_at: wall,
                popup_until: wall + 5000
            })]);
        compare(service.popupList.map(row => row.id), ["2800"]);
    }

    // The default clock is the wall clock, not Backend.now, which ticks once a second: a popup that
    // starts 30 ms after its frame shows within 200 ms although Backend.now never changes.
    function test_popupShowsOnTheWallClockNotTheBackendTick() {
        const service = createTemporaryObject(serviceComponent, tc);
        const wall = Date.now();
        Backend.now = wall - 900; // as stale as a frame can find it
        frame(service, [discordRow({
                time: wall,
                popup_at: wall + 30,
                popup_until: wall + 5030
            })]);
        compare(service.popupList.length, 0);
        tryVerify(() => service.popupList.length === 1, 200);
    }

    // The engine shows a popup whose popup_at is null (model.rs: popup_at.is_none_or).
    function test_popupWithoutPopupAtShows() {
        const service = makeService();
        frame(service, [discordRow({
                popup_at: null
            })]);
        compare(service.popupList.length, 1);
    }

    // The groups hold their rows oldest first, as the old service's list did, so NotificationGroup's
    // reverse shows the newest rows first and a collapsed group's first two are the newest.
    function test_collapsedGroupShowsTheNewestRows() {
        const service = makeService();
        const rows = [5, 4, 3, 2, 1].map(number => discordRow({
                id: String(2700 + number),
                rev: number,
                time: tc.discordTime + number * 1000
            }));
        frame(service, rows);
        const shown = service.groupsByAppName.discord.notifications.slice().reverse().slice(0, 2);
        compare(shown.map(row => row.notificationId), ["2705", "2704"]);
    }

    function test_heldRowsReleasedOnFirstSnapshot() {
        const service = makeService();
        frame(service, [discordRow({
                held: true,
                popup_until: null
            }), kittyRow()]);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "notify.hold_popups");
        compare(Backend.calls[0].args, {
            ids: ["2701"],
            held: false
        });
        frame(service, [discordRow({
                rev: 3,
                held: true,
                popup_until: null
            }), kittyRow()]);
        compare(Backend.calls.length, 1);
    }

    function test_unchangedRowKeepsItsObject() {
        const service = makeService();
        frame(service, [discordRow(), kittyRow()]);
        const firstDiscord = service.list[0];
        const firstKitty = service.list[1];
        frame(service, [discordRow(), kittyRow({
                rev: 3
            })]);
        verify(service.list[0] === firstDiscord);
        verify(service.list[1] !== firstKitty);
    }
}
