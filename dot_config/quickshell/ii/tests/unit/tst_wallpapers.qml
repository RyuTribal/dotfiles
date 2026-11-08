import QtQuick
import QtTest
import Quickshell.Io
import qs
import qs.services
import qs.modules.common
import qs.modules.settings
import qs.modules.wallpaperSelector
import "../../services/theme.js" as ThemeLogic

// The wallpaper selector and the theme's settings controls (40.13) over the real services/Theme.qml,
// services/Wallpapers.qml and services/AxonConfig.qml and the stand-in Backend: the selector's lazy
// list of `theme.list` rows, a tile and its click, one settings control per knob row, and the pure
// functions of services/theme.js behind the overview's accentcolor action and the clock format.
// Run it through backend_tests.sh, which builds the modules.
TestCase {
    id: tc
    name: "Wallpapers"
    when: windowShown
    // A TestCase is invisible by default, and under it every item's `visible` reads false.
    visible: true

    property double topicRev: 0
    property double configRev: 0

    readonly property var schemeValues: ["auto", "content", "expressive", "fidelity", "fruit-salad", "monochrome", "neutral", "rainbow", "tonal-spot"]

    Component {
        id: tileComponent
        WallpaperTile {
            width: 200
            height: 150
        }
    }

    Component {
        id: knobComponent
        ThemeKnobControl {
            width: 400
        }
    }

    function knob(id, type, value, extra) {
        return Object.assign({
            id: id,
            rev: 1,
            type: type,
            label: id,
            value: value,
            default: value
        }, extra ?? {});
    }

    function schemeKnob(value) {
        return knob("scheme", "enum", value, {
            values: tc.schemeValues
        });
    }

    function seedKnob() {
        return knob("seed", "color", "");
    }

    // A theme topic snapshot with cozy active, its wallpaper collection at `wallpapersRev` and these
    // knob rows and warnings.
    function themeSnapshot(wallpapersRev, knobs, warnings) {
        tc.topicRev += 1;
        Theme.topic._apply({
            t: "snapshot",
            topic: "theme",
            rev: tc.topicRev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                active: "cozy",
                mode: "dark",
                wallpaper: null,
                generation: "1@2026-11-08T10:00:00Z",
                themes: [
                    {
                        id: "cozy",
                        rev: tc.topicRev,
                        modes: ["dark", "light"],
                        wallpapers: {
                            rev: wallpapersRev,
                            count: 2,
                            skipped: 0
                        },
                        knobs: knobs ?? [schemeKnob("auto"), seedKnob()],
                        warnings: warnings ?? []
                    }
                ]
            }
        });
    }

    // A config snapshot with no issues, so the knob controls are writable.
    function configSnapshot() {
        tc.configRev += 1;
        AxonConfig.topic._apply({
            rev: tc.configRev,
            status: "ok",
            source: "live",
            observed_at: 1,
            data: {
                values: {},
                issues: []
            }
        });
    }

    function listCalls() {
        return Backend.calls.filter(call => call.method === "theme.list");
    }

    function lastCall() {
        return Backend.calls[Backend.calls.length - 1];
    }

    function wallpaperRow(name, fields) {
        return Object.assign({
            id: name,
            rev: 1,
            name: name,
            width: null,
            height: null,
            modified_at: 1,
            thumbnail: null,
            active: false
        }, fields ?? {});
    }

    // Answers the latest theme.list call with page 0 of the collection at `rev`.
    function answerPage(rev) {
        listCalls()[listCalls().length - 1].done({
            ok: true,
            result: {
                rev: rev,
                total: 1,
                offset: 0,
                next: null,
                items: [wallpaperRow("Pastel-Window.png")]
            }
        });
    }

    function answer(ok) {
        lastCall().done(ok ? {
            ok: true,
            result: null
        } : {
            ok: false,
            error: {
                code: "invalid_args",
                message: "refused"
            }
        });
    }

    function initTestCase() {
        configSnapshot();
    }

    // Each test starts with the selector closed, page 0 of the list answered and no calls recorded.
    function init() {
        if (GlobalStates.wallpaperSelectorOpen) {
            GlobalStates.wallpaperSelectorOpen = false;
            wait(20);
        }
        if (Wallpapers.list.loading)
            answerPage(Theme.wallpapersRev);
        Wallpapers.searchQuery = "";
        themeSnapshot(1);
        wait(20);
        Backend.calls = [];
    }

    function test_listArgsFollowTheThemeAndQuery() {
        Wallpapers.searchQuery = "pastel window";
        GlobalStates.wallpaperSelectorOpen = true;
        wait(20);
        compare(listCalls().length, 1);
        compare(listCalls()[0].args, {
            collection: "wallpapers",
            theme: "cozy",
            query: "pastel window",
            offset: 0,
            limit: 100
        });
        answerPage(1);
        compare(Wallpapers.list.items.length, 1);
    }

    // T25R-2: between a switch and its reload, iris.json still names the old theme; the list follows
    // the topic's active theme, whose wallpaper rev it watches.
    function test_listFollowsTheTopicsActiveTheme() {
        const view = FileViewProbe.view;
        view.content = JSON.stringify({
            schema: 1,
            generation: "1@2026-11-08T10:00:00Z",
            theme: {
                id: "cozy",
                name: "Cozy"
            },
            mode: "dark",
            wallpaper: null,
            colors: {}
        });
        view.loaded();
        tc.topicRev += 1;
        Theme.topic._apply({
            t: "snapshot",
            topic: "theme",
            rev: tc.topicRev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                active: "tiny",
                mode: "dark",
                wallpaper: null,
                generation: "2@2026-11-08T10:01:00Z",
                themes: [
                    {
                        id: "tiny",
                        rev: 1,
                        modes: ["dark"],
                        wallpapers: {
                            rev: 7,
                            count: 0,
                            skipped: 0
                        },
                        knobs: [],
                        warnings: []
                    }
                ]
            }
        });
        compare(Theme.themeId, "cozy", "the colours on screen are still cozy's");
        Backend.calls = [];
        GlobalStates.wallpaperSelectorOpen = true;
        wait(20);
        compare(listCalls().length, 1);
        compare(listCalls()[0].args.theme, "tiny");
        answerPage(7);
    }

    // Task 26: a selector open at shell start, before the first theme snapshot, asks for nothing until
    // the topic names the active theme; theme "" would be answered not_found.
    function test_listWaitsForTheActiveTheme() {
        tc.topicRev += 1;
        Theme.topic._apply({
            t: "snapshot",
            topic: "theme",
            rev: tc.topicRev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: {
                active: null,
                mode: null,
                wallpaper: null,
                generation: null,
                themes: []
            }
        });
        compare(Theme.activeThemeId, "");
        Backend.calls = [];
        GlobalStates.wallpaperSelectorOpen = true;
        wait(20);
        compare(listCalls().length, 0, "no list before the active theme is known");
        themeSnapshot(1);
        wait(20);
        compare(listCalls().length, 1);
        compare(listCalls()[0].args.theme, "cozy");
        compare(listCalls()[0].args.offset, 0);
        answerPage(1);
    }

    function test_revMoveRefetches() {
        GlobalStates.wallpaperSelectorOpen = true;
        wait(20);
        answerPage(1);
        Backend.calls = [];
        themeSnapshot(1, [schemeKnob("content"), seedKnob()]);
        wait(20);
        compare(listCalls().length, 0, "a snapshot that leaves the rev sends nothing");
        themeSnapshot(9007199254740991);
        wait(20);
        compare(listCalls().length, 1, "a rev move refetches page 0 once");
        compare(listCalls()[0].args.offset, 0);
        answerPage(9007199254740991);
        wait(20);
        compare(listCalls().length, 1);

        GlobalStates.wallpaperSelectorOpen = false;
        wait(20);
        Backend.calls = [];
        themeSnapshot(3);
        wait(20);
        compare(listCalls().length, 0, "the closed selector's list sends nothing");
    }

    function test_clickSetsTheWallpaperAndCloses() {
        GlobalStates.wallpaperSelectorOpen = true;
        wait(20);
        answerPage(1);
        Backend.calls = [];
        const tile = createTemporaryObject(tileComponent, tc, {
            row: wallpaperRow("Video Game - The Blackstreets.jpeg")
        });
        mouseClick(tile);
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "theme.set_wallpaper");
        compare(Backend.calls[0].args, {
            name: "Video Game - The Blackstreets.jpeg"
        });
        compare(GlobalStates.wallpaperSelectorOpen, false);
    }

    function test_tileShowsThumbnailAndActive() {
        const thumbnailFile = Qt.resolvedUrl("fakes/wallpapers/thumbnail.ppm").toString().replace("file://", "");
        const tile = createTemporaryObject(tileComponent, tc, {
            row: wallpaperRow("Pastel-Window.png", {
                thumbnail: thumbnailFile,
                active: true
            })
        });
        const thumbnail = findChild(tile, "thumbnail");
        const placeholder = findChild(tile, "placeholder");
        compare(thumbnail.source.toString(), "file://" + thumbnailFile);
        compare(thumbnail.asynchronous, true);
        tryCompare(thumbnail, "status", Image.Ready);
        compare(thumbnail.visible, true);
        compare(placeholder.visible, false);
        compare(findChild(tile, "name").text, "Pastel-Window.png");
        const background = findChild(tile, "background");
        verify(Qt.colorEqual(background.color, Appearance.colors.colSecondaryContainer), "the active wallpaper");

        tile.row = wallpaperRow("new.png");
        compare(thumbnail.visible, false);
        compare(placeholder.visible, true, "a placeholder until axon made the thumbnail");
        verify(!Qt.colorEqual(background.color, Appearance.colors.colSecondaryContainer));
        tile.current = true;
        verify(Qt.colorEqual(background.color, Appearance.colors.colPrimary), "the keyboard selection");
    }

    function test_knobControlsRenderFromDeclarations() {
        themeSnapshot(1, [schemeKnob("auto"), seedKnob()], [
            {
                id: "w1",
                rev: 1,
                code: "knob_value_invalid",
                key: "theme.cozy.scheme",
                message: "not a scheme"
            }
        ]);
        const control = createTemporaryObject(knobComponent, tc, {
            knobId: "scheme"
        });
        const selector = findChild(control, "enumSelector");
        verify(selector !== null);
        compare(findChild(control, "switch"), null);
        compare(findChild(control, "spinBox"), null);
        compare(selector.options.length, 9);
        compare(selector.options.map(option => option.value), tc.schemeValues);
        compare(selector.currentValue, "auto");
        compare(findChild(control, "warningRow").text, "not a scheme");

        themeSnapshot(1, [schemeKnob("neutral"), seedKnob()]);
        compare(selector.currentValue, "neutral");
        compare(Backend.calls.length, 0, "a snapshot writes nothing back");
        compare(findChild(control, "warningRow").visible, false);

        selector.selected("tonal-spot");
        compare(Backend.calls.length, 1);
        compare(lastCall().method, "config.set");
        compare(lastCall().args, {
            key: "theme.cozy.scheme",
            value: "tonal-spot"
        });
        compare(selector.currentValue, "tonal-spot", "the write shows at once");
        answer(true);
        themeSnapshot(1, [schemeKnob("neutral"), seedKnob()]);
        compare(selector.currentValue, "tonal-spot", "a snapshot from before the write");
        themeSnapshot(1, [schemeKnob("tonal-spot"), seedKnob()]);
        themeSnapshot(1, [schemeKnob("rainbow"), seedKnob()]);
        compare(selector.currentValue, "rainbow", "it follows the topic again");
    }

    function test_refusedWriteSnapsBackWithTheError() {
        const control = createTemporaryObject(knobComponent, tc, {
            knobId: "seed"
        });
        const field = findChild(control, "textField");
        compare(field.text, "");
        field.forceActiveFocus();
        field.selectAll();
        keyClick(Qt.Key_NumberSign);
        for (const character of "ff00")
            keyClick(character);
        keyClick(Qt.Key_Return);
        compare(Backend.calls.length, 0, "#ff00 is no colour");
        for (const character of "00")
            keyClick(character);
        keyClick(Qt.Key_Return);
        compare(Backend.calls.length, 1);
        compare(lastCall().args, {
            key: "theme.cozy.seed",
            value: "#ff0000"
        });
        answer(false);
        compare(field.text, "");
        compare(findChild(control, "errorRow").text, "refused");
    }

    function test_switchWritesOnClickOnly() {
        themeSnapshot(1, [knob("blur", "bool", false)]);
        const control = createTemporaryObject(knobComponent, tc, {
            knobId: "blur"
        });
        const toggle = findChild(control, "switch");
        compare(toggle.checked, false);
        themeSnapshot(1, [knob("blur", "bool", true)]);
        compare(toggle.checked, true);
        compare(Backend.calls.length, 0);
        mouseClick(toggle);
        compare(Backend.calls.length, 1);
        compare(lastCall().args.value, false);
        compare(toggle.checked, false);
        answer(true);
        themeSnapshot(1, [knob("blur", "bool", false)]);
        themeSnapshot(1, [knob("blur", "bool", true)]);
        compare(toggle.checked, true, "bound to the topic again");
    }

    // Cozy declares no int knob, so the row is a fixture.
    function test_unchangedTypedEntryFollowsTheTopic() {
        themeSnapshot(1, [knob("gaps", "int", 4, {
                min: 0,
                max: 20
            })]);
        const control = createTemporaryObject(knobComponent, tc, {
            knobId: "gaps"
        });
        const field = findChild(control, "valueField");
        compare(field.text, "4");
        field.forceActiveFocus();
        field.selectAll();
        keyClick("4");
        keyClick(Qt.Key_Return);
        compare(Backend.calls.length, 0, "an entry that ends as its start value writes nothing");
        themeSnapshot(1, [knob("gaps", "int", 7, {
                min: 0,
                max: 20
            })]);
        compare(field.text, "7");

        field.selectAll();
        keyClick("9");
        keyClick(Qt.Key_Return);
        compare(Backend.calls.length, 1);
        compare(lastCall().args, {
            key: "theme.cozy.gaps",
            value: 9
        });
    }

    // The argument parsing behind the overview's accentcolor action, and the seed flag its listing
    // reads; SearchWidget itself is not loaded.
    function test_accentColourArgumentAndSeedFlag() {
        themeSnapshot(1, [schemeKnob("auto")]);
        compare(Theme.hasSeedKnob, false);
        themeSnapshot(1, [schemeKnob("auto"), seedKnob()]);
        compare(Theme.hasSeedKnob, true);
        compare(ThemeLogic.accentColour("#ff0000"), "#ff0000");
        compare(ThemeLogic.accentColour("ff0000"), "#ff0000");
        compare(ThemeLogic.accentColour("#ff00"), null);
        compare(ThemeLogic.accentColour(""), null);
        Theme.setKnob("seed", ThemeLogic.accentColour("ff0000"));
        compare(lastCall().method, "config.set");
        compare(lastCall().args, {
            key: "theme.cozy.seed",
            value: "#ff0000"
        });
    }

    // The clock format to lock_clock mapping and its config.set; GeneralConfig itself is not loaded.
    function test_lockClockMappingAndConfigSet() {
        compare(ThemeLogic.lockClock("hh:mm"), "24h");
        compare(ThemeLogic.lockClock("h:mm ap"), "12h");
        compare(ThemeLogic.lockClock("h:mm AP"), "12h");
        AxonConfig.set("theme.lock_clock", ThemeLogic.lockClock("h:mm AP"));
        compare(lastCall().method, "config.set");
        compare(lastCall().args, {
            key: "theme.lock_clock",
            value: "12h"
        });
    }
}
