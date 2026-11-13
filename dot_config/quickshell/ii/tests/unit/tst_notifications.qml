import QtQuick
import QtTest
import "../../services/notifications.js" as NotificationsJs

// services/notifications.js: the mapping of a `notify` topic row, the popup rules and the grouping the
// Notifications facade builds from it. The first example row is section 31's, with the popup_at the
// engine adds (popups are staggered 300 ms apart). Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "Notifications"

    readonly property double timeOfFirst: 1792922457000

    readonly property var discordRow: ({
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
        })

    function iconPathStub(name) {
        return "icon:" + name;
    }

    // A mapped row with only the fields the popup rules read.
    function popupRow(popup, popupAt, popupUntil) {
        return {
            id: "row",
            popup: popup,
            popupAt: popupAt,
            popupUntil: popupUntil
        };
    }

    function test_mapRowParity() {
        const mapped = NotificationsJs.mapRow(discordRow, iconPathStub);
        compare(mapped.id, "2701");
        compare(mapped.rev, 1);
        compare(mapped.notificationId, "2701");
        compare(mapped.appName, "discord");
        compare(mapped.appIcon, "discord");
        compare(mapped.desktopEntry, "discord");
        compare(mapped.summary, "Anna");
        compare(mapped.body, "are you coming at 12?");
        compare(mapped.image, discordRow.image);
        compare(mapped.urgency, "normal");
        compare(mapped.time, 1792922457000);
        compare(mapped.actions, [
            {
                identifier: "default",
                text: "Open"
            }
        ]);
        compare(mapped.popup, true);
        compare(mapped.popupAt, 1792922457000);
        compare(mapped.popupUntil, 1792922462000);
    }

    function test_mapRowNullDeadlines() {
        const row = Object.assign({}, discordRow, {
            popup: false,
            popup_at: null,
            popup_until: null
        });
        const mapped = NotificationsJs.mapRow(row, iconPathStub);
        compare(mapped.popupAt, null);
        compare(mapped.popupUntil, null);
    }

    function test_mapRowIconNameImage() {
        const iconName = Object.assign({}, discordRow, {
            image: "dialog-information"
        });
        compare(NotificationsJs.mapRow(iconName, iconPathStub).image, "icon:dialog-information");
        compare(NotificationsJs.mapRow(discordRow, iconPathStub).image, discordRow.image);
        const noImage = Object.assign({}, discordRow, {
            image: ""
        });
        compare(NotificationsJs.mapRow(noImage, iconPathStub).image, "");
    }

    function test_popupRowsRules() {
        const now = 1000;
        const sticky = popupRow(true, 500, null);
        const running = popupRow(true, 500, 1500);
        const pastDeadline = popupRow(true, 500, 1000);
        const notPopup = popupRow(false, null, null);
        const queued = popupRow(true, 1200, 1700);
        const rows = [sticky, running, pastDeadline, notPopup, queued];
        compare(NotificationsJs.popupRows(rows, now, false), [sticky, running]);
        compare(NotificationsJs.popupRows(rows, now, true), []);
        // A row whose popupAt is in the future is hidden, then shown once now reaches it.
        compare(NotificationsJs.popupRows([queued], 1199, false), []);
        compare(NotificationsJs.popupRows([queued], 1200, false), [queued]);
        compare(NotificationsJs.popupRows([queued], 1700, false), []);
    }

    function test_nextDeadline() {
        const now = 1000;
        compare(NotificationsJs.nextDeadline([], now), -1);
        compare(NotificationsJs.nextDeadline([popupRow(true, 500, null)], now), -1);
        // The earliest future popupUntil; past ones and rows that do not popup are ignored.
        const rows = [popupRow(true, 500, 3000), popupRow(true, 500, 2000), popupRow(true, 500, 900), popupRow(false, null, 1100)];
        compare(NotificationsJs.nextDeadline(rows, now), 2000);
        // A future popupAt earlier than every popupUntil is the deadline.
        compare(NotificationsJs.nextDeadline([popupRow(true, 1300, 4300), popupRow(true, 500, 2000)], now), 1300);
        // A deadline equal to now has passed.
        compare(NotificationsJs.nextDeadline([popupRow(true, 500, 1000)], now), -1);
    }

    function test_latestTimes() {
        const rows = [
            {
                appName: "discord",
                time: 100
            },
            {
                appName: "kitty",
                time: 200
            },
            {
                appName: "discord",
                time: 300
            }
        ];
        compare(NotificationsJs.latestTimes(rows), {
            discord: 300,
            kitty: 200
        });
    }

    function test_heldIds() {
        const rows = [
            {
                id: "3",
                held: true
            },
            {
                id: "2",
                held: false
            },
            {
                id: "1",
                held: true
            }
        ];
        compare(NotificationsJs.heldIds(rows), ["3", "1"]);
    }

    // The old service's groupsForList and appNameListForGroups gave these results on its list, which
    // was oldest first (new notifications were appended): [third, second, first] below. The engine's
    // rows are newest first, so the same call here gets [first, second, third] and must give the
    // same groups (Notifications.qml:93-136 at HEAD): each group holds its rows oldest first, which
    // NotificationGroup's reverse turns into newest first.
    function test_groupsMatchTheOldService() {
        const first = {
            id: "3",
            appName: "discord",
            appIcon: "discord",
            time: 300
        };
        const second = {
            id: "2",
            appName: "kitty",
            appIcon: "kitty",
            time: 200
        };
        const third = {
            id: "1",
            appName: "discord",
            appIcon: "other-icon",
            time: 100
        };
        const rows = [first, second, third];
        const latest = NotificationsJs.latestTimes(rows);
        const groups = NotificationsJs.groupsForList(rows, latest);
        compare(Object.keys(groups).sort(), ["discord", "kitty"]);
        compare(groups.discord.appName, "discord");
        compare(groups.discord.appIcon, "other-icon"); // the oldest row's icon, the first the old service saw
        compare(groups.discord.time, 300); // the latest time of the app
        compare(groups.discord.notifications.length, 2);
        verify(groups.discord.notifications[0] === third);
        verify(groups.discord.notifications[1] === first);
        compare(groups.kitty.time, 200);
        compare(NotificationsJs.appNameListForGroups(groups), ["discord", "kitty"]);
        // A group whose app has no latest time takes the time of its newest row, the last the old
        // service saw.
        const unlatest = NotificationsJs.groupsForList([first, third], {});
        compare(unlatest.discord.time, 300);
    }

    // A collapsed group shows the first two rows of its reversed array: the newest two.
    function test_collapsedGroupFirstTwoAreTheNewest() {
        const rows = [5, 4, 3, 2, 1].map(number => ({
                    id: String(number),
                    appName: "discord",
                    appIcon: "discord",
                    time: number * 100
                }));
        const groups = NotificationsJs.groupsForList(rows, NotificationsJs.latestTimes(rows));
        const shown = groups.discord.notifications.slice().reverse().slice(0, 2);
        compare(shown.map(row => row.id), ["5", "4"]);
    }

    // The engine shows a popup whose popup_at is null (model.rs popup_at.is_none_or).
    function test_popupRowsNullPopupAtShows() {
        const unstamped = popupRow(true, null, 1500);
        compare(NotificationsJs.popupRows([unstamped], 1000, false), [unstamped]);
        compare(NotificationsJs.popupRows([unstamped], 1500, false), []);
        compare(NotificationsJs.nextDeadline([unstamped], 1000), 1500);
    }
}
