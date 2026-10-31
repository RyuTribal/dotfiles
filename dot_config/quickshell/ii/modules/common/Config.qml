pragma Singleton
pragma ComponentBehavior: Bound
import QtQuick
import Quickshell
import Quickshell.Io

Singleton {
    id: root
    property string filePath: Directories.shellConfigPath
    property alias options: configOptionsJsonAdapter
    property bool ready: false

    // False while config.json does not hold a JSON object: nothing is written then, so a hand edit with a
    // typo is not overwritten from memory. True before the first load so no error shows at shell start.
    readonly property bool fileValid: _fileValid
    readonly property string parseError: _parseError
    property bool _fileValid: true
    property string _parseError: ""

    // A write goes out only while the file is valid and no reload is pending, so a hand save made in the
    // ~100 ms before a shell-side change is read back instead of being overwritten. A shell-side change
    // made in that same window is lost; the hand save wins. Our own write is not a hand save and arms no
    // reload (see _ownWrite).
    readonly property bool _canWrite: _fileValid && !reloadTimer.running

    // True from our own writeAdapter() until FileView reports saved or saveFailed. The write replaces
    // config.json (QSaveFile renames over it), so FileView sends a fileChanged before saved; that one is
    // ignored. When the save completes FileView builds a new watcher, so any change after it is foreign.
    property bool _ownWrite: false

    function _write() {
        root._ownWrite = true;
        configFileView.writeAdapter();
    }

    // Writes a change that is still waiting in the debounce now. The settings and conflict windows call
    // it before Qt.quit(), which would otherwise drop the last 100 ms of edits.
    function flush() {
        if (writeTimer.running && root._canWrite) {
            writeTimer.stop();
            root._write();
        }
    }

    // The error text for a file that is not usable, or "" when it is. JsonAdapter reads only a JSON
    // object and keeps its old values for anything else, so [] or null count as errors too.
    function errorOf(text) {
        try {
            const parsed = JSON.parse(text);
            if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed))
                return "the top level must be a JSON object";
            return "";
        } catch (error) {
            return error.message || "the file is not valid JSON";
        }
    }

    // Same 100 ms debounce as the reload and write timers in Persistent.qml.
    Timer {
        id: reloadTimer
        interval: 100
        repeat: false
        onTriggered: configFileView.reload()
    }

    Timer {
        id: writeTimer
        interval: 100
        repeat: false
        onTriggered: {
            if (root._canWrite)
                root._write();
        }
    }

    FileView {
        id: configFileView
        path: root.filePath
        watchChanges: true
        onFileChanged: {
            if (!root._ownWrite)
                reloadTimer.restart();
        }
        onSaved: root._ownWrite = false
        onSaveFailed: root._ownWrite = false
        onAdapterUpdated: writeTimer.restart()
        onLoaded: {
            const message = root.errorOf(text());
            // The text goes in before the file turns invalid, so the toast is created with its message;
            // on a good load it is cleared after the file turns valid, so the toast never shows it empty.
            if (message !== "") {
                root._parseError = message;
                root._fileValid = false;
            } else {
                root._fileValid = true;
                root._parseError = "";
            }
            root.ready = true;
        }
        onLoadFailed: error => {
            if (error == FileViewError.FileNotFound) {
                root._fileValid = true;
                root._parseError = "";
                root._write();
            }
        }

        JsonAdapter {
            id: configOptionsJsonAdapter
            property JsonObject appearance: JsonObject {
                property bool extraBackgroundTint: true
                property int fakeScreenRounding: 2 // 0: None | 1: Always | 2: When not fullscreen
                property JsonObject transparency: JsonObject {
                    property bool enable: false
                    property bool automatic: true
                    property real backgroundTransparency: 0.11
                    property real contentTransparency: 0.57
                }
                property JsonObject wallpaperTheming: JsonObject {
                    property bool enableAppsAndShell: true
                    property bool enableQtApps: true
                    property bool enableTerminal: true
                    property JsonObject terminalGenerationProps: JsonObject {
                        property real harmony: 0.8
                        property real harmonizeThreshold: 100
                        property real termFgBoost: 0.35
                        property bool forceDarkMode: false
                    }
                }
                property JsonObject palette: JsonObject {
                    property string type: "auto" // Allowed: auto, scheme-content, scheme-expressive, scheme-fidelity, scheme-fruit-salad, scheme-monochrome, scheme-neutral, scheme-rainbow, scheme-tonal-spot
                }
            }

            property JsonObject audio: JsonObject {
                // Values in %
                property JsonObject protection: JsonObject {
                    // Prevent sudden bangs
                    property bool enable: false
                    property real maxAllowedIncrease: 10
                    property real maxAllowed: 99
                }
            }

            property JsonObject apps: JsonObject {
                property string bluetooth: "kcmshell6 kcm_bluetooth"
                property string network: "kitty -1 fish -c nmtui"
                property string networkEthernet: "kcmshell6 kcm_networkmanagement"
                property string taskManager: "kitty -1 btop"
                property string terminal: "kitty -1" // This is only for shell actions
            }

            property JsonObject background: JsonObject {
                property bool fixedClockPosition: false
                property real clockX: -500
                property real clockY: -500
                property bool showClock: true
                property string wallpaperPath: ""
                property string thumbnailPath: ""
                property JsonObject parallax: JsonObject {
                    property bool enableWorkspace: true
                    property real workspaceZoom: 1.07 // Relative to your screen, not wallpaper size
                    property bool enableSidebar: true
                }
                property string quote: ""
                property bool hideWhenFullscreen: true
            }

            property JsonObject bar: JsonObject {
                property JsonObject autoHide: JsonObject {
                    property bool enable: false
                    property bool pushWindows: false
                    property JsonObject showWhenPressingSuper: JsonObject {
                        property bool enable: true
                        property int delay: 140
                    }
                }
                property bool bottom: false // Instead of top
                property int cornerStyle: 0 // 0: Hug | 1: Float | 2: Plain rectangle
                property bool borderless: false // true for no grouping of items
                property bool showBackground: true
                property bool verbose: true
                property bool vertical: false
                property JsonObject resources: JsonObject {
                    property int memoryWarningThreshold: 95
                    property int swapWarningThreshold: 85
                    property int cpuWarningThreshold: 90
                }
                property list<string> screenList: [] // List of names, like "eDP-1", find out with 'hyprctl monitors' command
                property JsonObject utilButtons: JsonObject {
                    property bool showScreenSnip: true
                    property bool showColorPicker: false
                    property bool showMicToggle: false
                    property bool showDarkModeToggle: true
                    property bool showPerformanceProfileToggle: false
                }
                property JsonObject tray: JsonObject {
                    property bool monochromeIcons: true
                    property bool showItemId: false
                    property bool invertPinnedItems: true // Makes the below a whitelist for the tray and blacklist for the pinned area
                    property list<string> pinnedItems: []
                }
                property JsonObject workspaces: JsonObject {
                    property bool monochromeIcons: true
                    property int shown: 10
                    property bool showAppIcons: true
                    property bool alwaysShowNumbers: false
                    property list<string> numberMap: ["1", "2"] // Characters to show instead of numbers on workspace indicator
                    property bool useNerdFont: false
                }
                property JsonObject weather: JsonObject {
                    property bool enable: false
                    property bool enableGPS: true // gps based location
                    property string city: "" // When 'enableGPS' is false
                    property bool useUSCS: false // Instead of metric (SI) units
                    property int fetchInterval: 10 // minutes
                }
            }

            property JsonObject battery: JsonObject {
                property int low: 20
                property int critical: 5
                property bool automaticSuspend: true
                property int suspend: 3
            }

            property JsonObject conflictKiller: JsonObject {
                property bool autoKillNotificationDaemons: false
                property bool autoKillTrays: false
            }

            property JsonObject dock: JsonObject {
                property bool enable: false
                property bool monochromeIcons: true
                property real height: 60
                property real hoverRegionHeight: 2
                property bool pinnedOnStartup: false
                property bool hoverToReveal: true // When false, only reveals on empty workspace
                property list<string> pinnedApps: [ // IDs of pinned entries
                    "org.kde.dolphin", "kitty",]
                property list<string> ignoredAppRegexes: []
            }

            property JsonObject interactions: JsonObject {
                property JsonObject scrolling: JsonObject {
                    property bool fasterTouchpadScroll: false // Enable faster scrolling with touchpad
                    property int mouseScrollDeltaThreshold: 120 // delta >= this then it gets detected as mouse scroll rather than touchpad
                    property int mouseScrollFactor: 120
                    property int touchpadScrollFactor: 450
                }
                property JsonObject deadPixelWorkaround: JsonObject {
                    // Hyprland leaves out 1 pixel on the right for interactions
                    property bool enable: false
                }
            }

            property JsonObject language: JsonObject {
                property string ui: "auto" // UI language. "auto" for system locale, or specific language code like "zh_CN", "en_US"
            }

            property JsonObject light: JsonObject {
                property JsonObject night: JsonObject {
                    property bool automatic: true
                    property string from: "19:00" // Format: "HH:mm", 24-hour time
                    property string to: "06:30"   // Format: "HH:mm", 24-hour time
                    property int colorTemperature: 5000
                }
            }

            property JsonObject media: JsonObject {
                // Attempt to remove dupes (the aggregator playerctl one and browsers' native ones when there's plasma browser integration)
                property bool filterDuplicatePlayers: true
            }

            property JsonObject networking: JsonObject {
                property string userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/123.0.0.0 Safari/537.36"
            }

            property JsonObject notifications: JsonObject {
                property JsonObject sounds: JsonObject {
                    property bool enabled: true
                    property string defaultSound: "message" // Theme event name or absolute file path
                    property list<var> overrides: []
                }
            }

            property JsonObject osd: JsonObject {
                property int timeout: 1000
            }

            property JsonObject overview: JsonObject {
                property bool enable: true
                property real scale: 0.18 // Relative to screen size
                property real rows: 2
                property real columns: 5
            }

            property JsonObject search: JsonObject {
                property int nonAppResultDelay: 30 // This prevents lagging when typing
                property string engineBaseUrl: "https://www.google.com/search?q="
                property list<string> excludedSites: ["quora.com"]
                property bool sloppy: false // Uses levenshtein distance based scoring instead of fuzzy sort. Very weird.
                property JsonObject prefix: JsonObject {
                    property bool showDefaultActionsWithoutPrefix: true
                    property string action: "/"
                    property string clipboard: ";"
                    property string emojis: ":"
                    property string glyphs: "|"
                    property string math: "="
                    property string shellCommand: "$"
                    property string tools: ">"
                    property string webSearch: "?"
                }
            }

            property JsonObject sidebar: JsonObject {
                property bool keepRightSidebarLoaded: true
                property JsonObject cornerOpen: JsonObject {
                    property bool enable: true
                    property bool bottom: false
                    property bool valueScroll: true
                    property bool clickless: false
                    property real cornerRegionWidth: 60
                    property real cornerRegionHeight: 2
                    property bool visualize: false
                }
            }

            property JsonObject time: JsonObject {
                // https://doc.qt.io/qt-6/qtime.html#toString
                property string format: "hh:mm"
                property string shortDateFormat: "dd/MM"
                property string dateFormat: "ddd, dd/MM"
                property JsonObject pomodoro: JsonObject {
                    property string alertSound: ""
                    property int breakTime: 300
                    property int cyclesBeforeLongBreak: 4
                    property int focus: 1500
                    property int longBreak: 900
                }
            }

            property JsonObject windows: JsonObject {
                property bool showTitlebar: true // Client-side decoration for shell apps
                property bool centerTitle: true
            }

            property JsonObject hacks: JsonObject {
                property int arbitraryRaceConditionDelay: 20 // milliseconds
            }

            property JsonObject screenshotTool: JsonObject {
                property bool showContentRegions: true
            }
        }
    }
}
