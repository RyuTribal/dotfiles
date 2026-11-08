import QtQuick
import QtTest
import Quickshell.Io
import qs.services
import qs.modules.common
import qs.modules.common.functions
import AppearanceUnderTest as Real

// The real services/Theme.qml over the stand-in Backend and FileView: the colours it writes into
// Appearance from iris.json, its reload when the topic's generation moves, its iris report, the
// wallpaper URL and the knob write. The hover test reads the real modules/common/Appearance.qml.
// Run it through backend_tests.sh, which builds the modules.
TestCase {
    id: tc
    name: "Theme"

    property var view: null // Theme's stand-in FileView
    property double rev: 0

    // The 52 roles of cozy's dark iris.json (axon's golden render).
    readonly property var cozyDarkColors: ({
            accent: "#ffb4ab",
            background: "#091519",
            error: "#ffb4ab",
            error_container: "#93000a",
            inverse_on_surface: "#273236",
            inverse_primary: "#006c50",
            inverse_surface: "#d8e5e9",
            on_accent: "#690005",
            on_background: "#d8e5e9",
            on_error: "#690005",
            on_error_container: "#ffdad6",
            on_primary: "#003828",
            on_primary_container: "#85f8cc",
            on_primary_fixed: "#002116",
            on_primary_fixed_variant: "#00513b",
            on_secondary: "#003828",
            on_secondary_container: "#a6f2d1",
            on_secondary_fixed: "#002116",
            on_secondary_fixed_variant: "#00513b",
            on_surface: "#d8e5e9",
            on_surface_variant: "#b2cad3",
            on_tertiary: "#003641",
            on_tertiary_container: "#b1ecff",
            on_tertiary_fixed: "#001f27",
            on_tertiary_fixed_variant: "#004e5e",
            outline: "#7d949d",
            outline_variant: "#344a51",
            primary: "#67dbb0",
            primary_container: "#00513b",
            primary_fixed: "#85f8cc",
            primary_fixed_dim: "#67dbb0",
            screen_corner: "#000000",
            scrim: "#000000",
            secondary: "#8bd6b6",
            secondary_container: "#00513b",
            secondary_fixed: "#a6f2d1",
            secondary_fixed_dim: "#8bd6b6",
            shadow: "#000000",
            surface: "#091519",
            surface_bright: "#2f3b3f",
            surface_container: "#162125",
            surface_container_high: "#202c30",
            surface_container_highest: "#2b373a",
            surface_container_low: "#121d21",
            surface_container_lowest: "#051013",
            surface_dim: "#091519",
            surface_tint: "#67dbb0",
            surface_variant: "#344a51",
            tertiary: "#86d1e9",
            tertiary_container: "#004e5e",
            tertiary_fixed: "#b1ecff",
            tertiary_fixed_dim: "#86d1e9"
        })

    function init() {
        Backend.up = true;
        Backend.instance = "one";
        reportAs(true); // sends what an earlier test left unreported
        reportAs(false);
        view = FileViewProbe.view;
        view.reloads = 0;
        Backend.calls = [];
    }

    // Whether Theme reports like the main shell, which alone sets the flag.
    function reportAs(mainShell) {
        Theme.reportsIris = mainShell;
    }

    function tokens(fields) {
        return Object.assign({
            schema: 1,
            generation: "1@2026-11-08T10:00:00Z",
            theme: {
                id: "cozy",
                name: "Cozy"
            },
            mode: "dark",
            wallpaper: {
                name: "Pastel-Window.png",
                path: "/p/Pastel-Window.png",
                width: 2880,
                height: 1620
            },
            colors: tc.cozyDarkColors
        }, fields ?? {});
    }

    function loadText(text) {
        view.content = text;
        view.loaded();
    }

    function load(fields) {
        loadText(JSON.stringify(tokens(fields)));
    }

    function snapshot(snapshotData) {
        tc.rev += 1;
        Theme.topic._apply({
            t: "snapshot",
            topic: "theme",
            rev: tc.rev,
            status: "ok",
            source: "live",
            observed_at: 1,
            ops: [],
            data: snapshotData
        });
    }

    function topicData(generation, themeRow) {
        return {
            active: "cozy",
            mode: "dark",
            wallpaper: null,
            generation: generation,
            themes: themeRow ? [themeRow] : []
        };
    }

    function reports() {
        return Backend.calls.filter(call => call.method === "theme.report_target");
    }

    function test_colorsLoadFromIrisJson() {
        compare(view.path, "/nonexistent/axon/theme/current/iris.json");
        load();
        verify(Qt.colorEqual(Appearance.m3colors.m3background, "#091519"));
        verify(Qt.colorEqual(Appearance.m3colors.m3accent, "#ffb4ab"));
        verify(Qt.colorEqual(Appearance.m3colors.m3onAccent, "#690005"));
        verify(Qt.colorEqual(Appearance.m3colors.m3screenCorner, "#000000"));
        compare(Appearance.m3colors.darkmode, true);
        compare(Theme.mode, "dark");
        load({
            mode: "light"
        });
        compare(Appearance.m3colors.darkmode, false);
        compare(Theme.mode, "light");
        // Every key Theme writes is one the real Appearance declares.
        for (const key of Object.keys(Appearance.m3colors).filter(name => name.startsWith("m3")))
            verify(key in Real.Appearance.m3colors, key);
    }

    function test_alphaColoursReachQtAsArgb() {
        load({
            colors: Object.assign({}, tc.cozyDarkColors, {
                scrim: "#00000080"
            })
        });
        compare(Appearance.m3colors.m3scrim, "#80000000");
    }

    function test_missingFileKeepsBuiltIns() {
        const before = JSON.stringify(Appearance.m3colors);
        view.loadFailed(FileViewError.FileNotFound);
        compare(JSON.stringify(Appearance.m3colors), before);
        reportAs(true);
        compare(reports().length, 0);
    }

    function test_reloadsWhenTheGenerationMoves() {
        load({
            generation: "4@2026-11-08T10:00:00Z"
        });
        snapshot(topicData("4@2026-11-08T10:00:00Z"));
        compare(view.reloads, 0, "the loaded generation");
        snapshot(topicData("5@2026-11-08T10:01:00Z"));
        compare(view.reloads, 1);
        snapshot(topicData("5@2026-11-08T10:01:00Z"));
        compare(view.reloads, 1, "the same generation in a later frame");
    }

    function test_reportsAfterLoad() {
        reportAs(true);
        load({
            generation: "6@2026-11-08T10:02:00Z"
        });
        compare(reports().length, 1);
        compare(reports()[0].args, {
            generation: "6@2026-11-08T10:02:00Z",
            target: "iris",
            ok: true
        });
        snapshot(topicData("7@2026-11-08T10:03:00Z"));
        const before = JSON.stringify(Appearance.m3colors);
        loadText('{"schema": 1, "colors": ');
        compare(reports().length, 2);
        const failed = reports()[1].args;
        compare(failed.generation, "7@2026-11-08T10:03:00Z");
        compare(failed.target, "iris");
        compare(failed.ok, false);
        verify(typeof failed.error === "string" && failed.error.length > 0, failed.error);
        compare(JSON.stringify(Appearance.m3colors), before, "a broken file keeps the colours");
    }

    function test_onlyTheMainShellReports() {
        load({
            generation: "8@2026-11-08T10:04:00Z"
        });
        compare(reports().length, 0);
        reportAs(true);
        compare(reports().length, 1);
        compare(reports()[0].args.generation, "8@2026-11-08T10:04:00Z");
        reportAs(false);
        reportAs(true);
        compare(reports().length, 1, "a load is reported once");
        load({
            generation: "8@2026-11-08T10:04:00Z"
        });
        compare(reports().length, 2);
    }

    function test_reportWaitsForTheConnection() {
        Backend.up = false;
        reportAs(true);
        load({
            generation: "9@2026-11-08T10:05:00Z"
        });
        compare(reports().length, 0);
        Backend.up = true;
        compare(reports().length, 1);
        compare(reports()[0].args.generation, "9@2026-11-08T10:05:00Z");
    }

    function test_wallpaperUrlEncodesSpacesAndHash() {
        load({
            wallpaper: {
                name: "Video Game - #1's.jpeg",
                path: "/p/Video Game - #1's.jpeg",
                width: 1920,
                height: 1080
            }
        });
        compare(Theme.wallpaperUrl, "file:///p/Video%20Game%20-%20%231%27s.jpeg");
        compare(Theme.wallpaper.width, 1920);
        // A literal % stays encoded, never read back as a slash; non-ASCII goes as UTF-8 bytes.
        load({
            wallpaper: {
                name: "a%2Fb (é)!.png",
                path: "/p/a%2Fb (é)!.png",
                width: 1920,
                height: 1080
            }
        });
        compare(Theme.wallpaperUrl, "file:///p/a%252Fb%20%28%C3%A9%29%21.png");
    }

    // T24R-2: a file that parses but holds a colour that is not one is refused whole.
    function test_badColourKeepsTheColoursAndReportsIt() {
        load();
        const before = JSON.stringify(Appearance.m3colors);
        reportAs(true);
        Backend.calls = [];
        load({
            generation: "12@2026-11-08T10:08:00Z",
            colors: Object.assign({}, tc.cozyDarkColors, {
                background: "#123456",
                primary: 42
            })
        });
        compare(JSON.stringify(Appearance.m3colors), before, "nothing of the file is applied");
        compare(reports().length, 1);
        compare(reports()[0].args.ok, false);
        verify(reports()[0].args.error.indexOf("primary") !== -1, reports()[0].args.error);
        load({
            colors: Object.assign({}, tc.cozyDarkColors, {
                not_a_role: "#000000"
            })
        });
        compare(JSON.stringify(Appearance.m3colors), before, "a role Appearance does not declare");
        compare(reports().length, 2);
        compare(reports()[1].args.ok, false);
        load({
            colors: ["#000000"]
        });
        compare(reports().length, 3);
        compare(reports()[2].args.ok, false, "colors that is no object");
    }

    function test_layer0HoverIsOpaqueWithTransparencyOff() {
        const colors = Real.Appearance.colors;
        compare(colors.colLayer0Hover.a, 1.0);
        compare(colors.colLayer0Active.a, 1.0);
        Real.Config.options = {
            appearance: {
                transparency: {
                    enable: true,
                    automatic: false,
                    backgroundTransparency: 0.1,
                    contentTransparency: 0.3
                },
                extraBackgroundTint: false
            },
            bar: {
                cornerStyle: 0,
                verbose: false
            }
        };
        const mixAlpha = ColorUtils.mix(colors.colLayer0, colors.colOnLayer0, 0.9).a;
        verify(mixAlpha < 1, "the background transparency reaches the mix");
        fuzzyCompare(colors.colLayer0Hover.a, (1 - 0.3) * mixAlpha, 0.005);
    }

    function test_activeRowFeedsTheKnobs() {
        snapshot(topicData("10@2026-11-08T10:06:00Z", {
            id: "cozy",
            rev: 1,
            modes: ["dark", "light"],
            wallpapers: {
                rev: 9007199254740991,
                count: 16,
                skipped: 1
            },
            knobs: [
                {
                    id: "scheme",
                    rev: 2,
                    type: "enum",
                    value: "fruit-salad"
                },
                {
                    id: "seed",
                    rev: 3,
                    type: "color",
                    value: ""
                }
            ],
            warnings: [
                {
                    id: "11",
                    rev: 11,
                    code: "knob_value_invalid",
                    key: "theme.cozy.scheme",
                    message: "not a scheme"
                }
            ]
        }));
        compare(Theme.modes, ["dark", "light"]);
        compare(Theme.wallpapersRev, 9007199254740991);
        compare(Theme.knobs.length, 2);
        compare(Theme.hasSeedKnob, true);
        compare(Theme.knobWarnings["theme.cozy.scheme"], "not a scheme");
    }

    function test_setKnobWritesTheActiveTheme() {
        load();
        compare(Theme.themeId, "cozy");
        snapshot(topicData("1@2026-11-08T10:00:00Z", {
            id: "cozy",
            rev: 1,
            modes: ["dark", "light"],
            wallpapers: {
                rev: 1,
                count: 16,
                skipped: 0
            },
            knobs: [],
            warnings: []
        }));
        Backend.calls = [];
        Theme.setKnob("scheme", "tonal-spot");
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].method, "config.set");
        compare(Backend.calls[0].args, {
            key: "theme.cozy.scheme",
            value: "tonal-spot"
        });
    }

    // T24R-1: between a switch and its reload, iris.json still names the old theme; the knobs and
    // their writes follow the topic's active theme, which axon renders.
    function test_knobsFollowTheTopicsActiveTheme() {
        load();
        const data = topicData("13@2026-11-08T10:09:00Z", {
            id: "tiny",
            rev: 1,
            modes: ["dark"],
            wallpapers: {
                rev: 1,
                count: 0,
                skipped: 0
            },
            knobs: [
                {
                    id: "scheme",
                    rev: 1,
                    type: "enum",
                    value: "content"
                }
            ],
            warnings: [
                {
                    id: "12",
                    rev: 12,
                    code: "knob_value_invalid",
                    key: "theme.tiny.scheme",
                    message: "not a scheme"
                }
            ]
        });
        data.active = "tiny";
        snapshot(data);
        compare(Theme.themeId, "cozy", "the colours on screen are still cozy's");
        Backend.calls = [];
        Theme.setKnob("scheme", "neutral");
        compare(Backend.calls.length, 1);
        compare(Backend.calls[0].args, {
            key: "theme.tiny.scheme",
            value: "neutral"
        });
        compare(Theme.knobWarning("scheme"), "not a scheme");
        compare(Theme.knobWarning("seed"), "");
    }
}
