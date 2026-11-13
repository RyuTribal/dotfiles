import QtQuick
import QtTest
import qs
import qs.services
import qs.modules.common.widgets

// The group lists of the sidebar and the popups: the real NotificationListView, StyledListView and
// NotificationGroup over the real services/Notifications.qml, fed axon `notify` frames. The rule for
// every settled layout: the first group sits at the top of the list and each later group at the
// previous group's y + height + spacing. Under StyledListView's add and remove transitions a group that grew while one
// ran (a new app's second row within 500 ms) left the groups below in place, overlapping it.
TestCase {
    id: testCase

    property double rev: 0
    readonly property double base: Date.now() - 30000
    readonly property var ids1: row(101, "check-ids", 0, "ids one")
    readonly property var ids2: row(102, "check-ids", 10, "ids two")
    readonly property var dnd1: row(130, "check-dnd", 1000, "dnd one")
    readonly property var dnd2: row(131, "check-dnd", 1012, "dnd two")
    readonly property var new1: row(140, "check-new", 2000, "new one")

    function row(id, app, offset, summary) {
        return {
            id: String(id),
            rev: 1,
            app_name: app,
            app_icon: "",
            desktop_entry: app,
            summary: summary,
            body: "",
            image: "",
            urgency: "normal",
            time: testCase.base + offset,
            actions: [],
            popup: true,
            popup_at: testCase.base,
            popup_until: null,
            held: false,
            transient: false
        };
    }

    // One `notify` snapshot holding `items`, oldest first here and newest first on the wire, as
    // axond sends them.
    function frame(items) {
        testCase.rev += 1;
        Notifications.topic._apply({
            t: "snapshot",
            topic: "notify",
            rev: testCase.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                server: {
                    name: "org.freedesktop.Notifications",
                    owned: true
                },
                dnd: false,
                count: items.length,
                items: items.slice().reverse()
            }
        });
    }

    function groupItem(list, appName) {
        for (let index = 0; index < list.count; index++) {
            const group = list.itemAtIndex(index);
            if (group && group.modelData === appName)
                return group;
        }
        return null;
    }

    function geometry(list) {
        const groups = [];
        for (let index = 0; index < list.count; index++) {
            const group = list.itemAtIndex(index);
            groups.push(group ? `${group.modelData} y=${group.y} h=${group.height}` : "null");
        }
        return groups.join(" | ");
    }

    // A group removed at the top moves the list's origin instead of the groups below it, so the first
    // group sits at originY, and the list shows it at its top.
    function checkLayout(list, label) {
        compare(list.contentY, list.originY, `${label}: the list shows its first group at the top`);
        let expectedY = list.originY;
        for (let index = 0; index < list.count; index++) {
            const group = list.itemAtIndex(index);
            verify(group, `${label}: group ${index} has a delegate`);
            const where = `${group.modelData} at y=${group.y}, expected ${expectedY}`;
            verify(Math.abs(group.y - expectedY) < 0.5, `${label}: ${where} (${geometry(list)})`);
            expectedY = group.y + group.height + list.spacing;
        }
    }

    function checkAppearing(group, label) {
        const now = `opacity ${group.opacity}, scale ${group.scale}`;
        verify(group.opacity < 1 && group.scale < 1, `${label} starts small and clear (${now})`);
    }

    function makeList(popup) {
        return createTemporaryObject(popup ? popupListComponent : listComponent, testCase);
    }

    function init() {
        frame([]);
        wait(700); // the groups of the last test finish leaving
    }

    // Two settled groups, then the upper one gains a row: the lower one moves down by what it gained.
    function test_settled_growth() {
        const list = makeList(false);
        frame([ids1, ids2, dnd1]);
        wait(1000);
        const idsBefore = groupItem(list, "check-ids").y;
        const dndBefore = groupItem(list, "check-dnd").height;
        frame([ids1, ids2, dnd1, dnd2]);
        wait(1000);
        const added = groupItem(list, "check-dnd").height - dndBefore;
        verify(added > 0, "check-dnd grew");
        compare(groupItem(list, "check-ids").y, idsBefore + added, "check-ids moved down by what check-dnd gained");
        checkLayout(list, "settled growth");
    }

    // check-dnd's first row makes a new group above check-ids, and its second row lands `gap` ms later,
    // while the new group still appears.
    function test_insert_then_grow_data() {
        return [
            {
                tag: "same frame",
                gap: -1
            },
            {
                tag: "12ms",
                gap: 12
            },
            {
                tag: "100ms",
                gap: 100
            },
            {
                tag: "300ms",
                gap: 300
            },
            {
                tag: "700ms",
                gap: 700
            }
        ];
    }
    function test_insert_then_grow(data) {
        const list = makeList(false);
        frame([ids1, ids2]);
        wait(1000);
        if (data.gap < 0) {
            frame([ids1, ids2, dnd1, dnd2]);
        } else {
            frame([ids1, ids2, dnd1]);
            wait(data.gap);
            frame([ids1, ids2, dnd1, dnd2]);
        }
        wait(1000);
        compare(groupItem(list, "check-dnd").notificationCount, 2, "check-dnd holds both rows");
        checkLayout(list, "insert then grow " + data.tag);
    }

    // check-new (the newest group) is dismissed above check-dnd, which gains a row `gap` ms later,
    // while check-new still leaves.
    function test_remove_then_grow_data() {
        return [
            {
                tag: "12ms",
                gap: 12
            },
            {
                tag: "100ms",
                gap: 100
            },
            {
                tag: "300ms",
                gap: 300
            }
        ];
    }
    function test_remove_then_grow(data) {
        const list = makeList(false);
        frame([ids1, ids2, dnd1, new1]);
        wait(1000);
        frame([ids1, ids2, dnd1]);
        wait(data.gap);
        frame([ids1, ids2, dnd1, dnd2]);
        wait(1000);
        compare(list.count, 2, "check-new is gone");
        checkLayout(list, "remove then grow " + data.tag);
    }

    // A new group takes its space at once and fades and scales in from 0 to 1 in it.
    function test_new_group_appears_in_its_space() {
        const list = makeList(false);
        frame([ids1, ids2]);
        wait(1000);
        frame([ids1, ids2, dnd1]);
        wait(30);
        const dnd = groupItem(list, "check-dnd");
        checkAppearing(dnd, "check-dnd");
        checkLayout(list, "while check-dnd appears");
        tryVerify(() => dnd.opacity === 1 && dnd.scale === 1, 1000, "check-dnd ends opaque at full size");
        checkLayout(list, "after check-dnd appeared");
    }

    // The groups there when the list is made show as they are.
    function test_first_groups_do_not_animate() {
        frame([ids1, ids2, dnd1]);
        const list = makeList(false);
        wait(30);
        compare(list.count, 2);
        for (let index = 0; index < list.count; index++) {
            const group = list.itemAtIndex(index);
            compare(group.opacity, 1, group.modelData + " is opaque at once");
            compare(group.scale, 1, group.modelData + " is full size at once");
        }
        checkLayout(list, "first show");
    }

    // A dismissed group keeps its space while it fades out, then the groups below close the gap.
    function test_removed_group_keeps_its_space_until_it_has_left() {
        const list = makeList(false);
        frame([ids1, ids2, dnd1, new1]);
        wait(1000);
        const leaving = groupItem(list, "check-new");
        const dnd = groupItem(list, "check-dnd");
        const dndBefore = dnd.y;
        verify(dndBefore > 0, "check-dnd sits below check-new");
        let fadedWhileThere = false;
        leaving.opacityChanged.connect(() => {
            if (leaving.opacity > 0 && leaving.opacity < 1 && dnd.y === dndBefore)
                fadedWhileThere = true;
        });
        frame([ids1, ids2, dnd1]);
        wait(200);
        compare(dnd.y, dndBefore, "check-dnd stays while check-new leaves");
        verify(fadedWhileThere, "check-new fades while it holds its space");
        tryVerify(() => dnd.y - list.contentY === 0, 1000, "check-dnd moves up to the top once check-new has left");
        checkLayout(list, "after check-new left");
    }

    // Cutting a group's fade short still gives its space back.
    function test_stopped_fade_gives_the_space_back() {
        const list = makeList(false);
        frame([ids1, ids2, dnd1, new1]);
        wait(1000);
        const leaving = groupItem(list, "check-new");
        const dnd = groupItem(list, "check-dnd");
        const fade = leaving.data.find(child => child.property === "opacity" && child.to === 0);
        verify(fade, "check-new has its fade");
        frame([ids1, ids2, dnd1]);
        wait(100);
        verify(fade.running, "check-new is fading");
        fade.stop();
        tryVerify(() => dnd.y - list.contentY === 0, 200, "check-dnd moves up as soon as the fade stops");
        checkLayout(list, "after the stopped fade");
    }

    // A group dismissed while it still appears fades out from where it got to and gives its space back.
    function test_removed_while_appearing() {
        const list = makeList(false);
        frame([ids1, ids2]);
        wait(1000);
        frame([ids1, ids2, dnd1]);
        wait(100);
        frame([ids1, ids2]);
        wait(1000);
        compare(list.count, 1, "check-dnd is gone");
        checkLayout(list, "removed while appearing");
    }

    // Expanding the new group while it appears.
    function test_expand_while_appearing() {
        const list = makeList(false);
        frame([ids1, ids2]);
        wait(1000);
        frame([ids1, ids2, dnd1, dnd2]);
        wait(100);
        groupItem(list, "check-dnd").toggleExpanded();
        wait(1000);
        verify(groupItem(list, "check-dnd").expanded, "check-dnd is expanded");
        checkLayout(list, "expanded while appearing");
    }

    // The popup list is the same list over the popup groups.
    function test_popup_list_insert_then_grow() {
        const list = makeList(true);
        frame([ids1, ids2]);
        wait(1000);
        frame([ids1, ids2, dnd1]);
        wait(30);
        const dnd = groupItem(list, "check-dnd");
        checkAppearing(dnd, "the popup check-dnd");
        wait(70);
        frame([ids1, ids2, dnd1, dnd2]);
        wait(1000);
        compare(dnd.opacity, 1);
        compare(dnd.scale, 1);
        compare(dnd.notificationCount, 2, "the popup check-dnd holds both rows");
        checkLayout(list, "popup insert then grow");
    }

    name: "NotificationList"
    when: windowShown
    width: 420
    height: 800

    Component {
        id: listComponent

        NotificationListView {
            width: 400
            height: 760
        }
    }

    Component {
        id: popupListComponent

        NotificationListView {
            width: 400
            height: 760
            popup: true
        }
    }
}
