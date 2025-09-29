import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import "." // ensure Singleton NerdGlyphs is visible if it lives alongside this file

Item { // Wrapper
    id: root
    readonly property string xdgConfigHome: Directories.config
    property string searchingText: ""
    property bool showResults: searchingText != ""
    property string pendingText: ""
    property real searchBarHeight: searchBar.height + Appearance.sizes.elevationMargin * 2
    implicitWidth: searchWidgetContent.implicitWidth + Appearance.sizes.elevationMargin * 2
    implicitHeight: searchWidgetContent.implicitHeight + Appearance.sizes.elevationMargin * 2

    property string mathResult: ""

    // --- Paging state for Nerd Fonts (result limiting + load-more) ---
    property var glyphAll: []          // full fuzzy set
    property var glyphPaged: []        // currently rendered page
    property int glyphPageSize: 200    // initial page size
    property int glyphPageStep: 200    // how many to add each load
    property bool glyphLoadingMore: false
    property string glyphLastQuery: ""

    function rebuildGlyphResults(query) {
        glyphAll = NerdGlyphs.fuzzyQuery(query);
        glyphPaged = glyphAll.slice(0, glyphPageSize);
        glyphLastQuery = query;
    }

    function loadMoreGlyphs() {
        if (glyphLoadingMore)
            return;
        if (glyphPaged.length >= glyphAll.length)
            return;
        glyphLoadingMore = true;
        const next = Math.min(glyphPaged.length + glyphPageStep, glyphAll.length);
        glyphPaged = glyphAll.slice(0, next);
        glyphLoadingMore = false;
    }

    // Rebuild paging when the search text changes (glyph mode only)
    Connections {
        target: root
        function onSearchingTextChanged() {
            const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
            if (root.searchingText.startsWith(glyphPrefix)) {
                const q = root.searchingText.slice(glyphPrefix.length);

                // If data exists -> build immediately
                if (!NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) > 0) {
                    if (root.glyphLastQuery !== q)
                        rebuildGlyphResults(q);
                } else {
                    // If nothing yet, start loading once, then build on load-complete
                    if (!NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) === 0)
                        NerdGlyphs.load(false);
                    root.glyphAll = [];
                    root.glyphPaged = [];
                    root.glyphLastQuery = q;
                }
            } else {
                root.glyphAll = [];
                root.glyphPaged = [];
                root.glyphLastQuery = "";
            }
        }
    }

    // Build the initial page as soon as NerdGlyphs becomes ready
    Connections {
        target: NerdGlyphs
        function onLoadingChanged() {
            if (!NerdGlyphs.loading) {
                const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
                if (root.searchingText.startsWith(glyphPrefix)) {
                    const q = root.searchingText.slice(glyphPrefix.length);
                    rebuildGlyphResults(q);
                }
            }
        }
        function onListChanged() {
            const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
            if (!NerdGlyphs.loading && root.searchingText.startsWith(glyphPrefix)) {
                const q = root.searchingText.slice(glyphPrefix.length);
                rebuildGlyphResults(q);
            }
        }
    }

    // Ensure we kick a load *when the panel opens* with glyph prefix injected
    Connections {
        target: GlobalStates
        function onOverviewOpenChanged() {
            if (GlobalStates.overviewOpen) {
                const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
                if (root.searchingText.startsWith(glyphPrefix)) {
                    const q = root.searchingText.slice(glyphPrefix.length);
                    if (!NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) === 0) {
                        NerdGlyphs.load(false);
                    } else if (!NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) > 0) {
                        // Build immediately on open
                        rebuildGlyphResults(q);
                    }
                }
            }
        }
    }

    Component.onCompleted: {
        const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
        if (root.searchingText.startsWith(glyphPrefix) && !NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) === 0) {
            NerdGlyphs.load(false);
        }
    }

    function disableExpandAnimation() {
        searchWidthBehavior.enabled = false;
    }

    function cancelSearch() {
        searchInput.selectAll();
        root.searchingText = "";
        searchWidthBehavior.enabled = true;
    }

    function setSearchingText(text) {
        searchInput.text = text;
        root.searchingText = text;
        // If we get called by the toggle with a glyph prefix, build page 1 now if data is ready
        const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
        if (typeof text === "string" && text.startsWith(glyphPrefix)) {
            const q = text.slice(glyphPrefix.length);
            if (!NerdGlyphs.loading && (NerdGlyphs.list?.length ?? 0) > 0) {
                rebuildGlyphResults(q);
            }
        }
    }

    property var searchActions: [
        {
            action: "accentcolor",
            execute: args => {
                Quickshell.execDetached([Directories.wallpaperSwitchScriptPath, "--noswitch", "--color", ...(args != '' ? [`${args}`] : [])]);
            }
        },
        {
            action: "dark",
            execute: () => {
                Quickshell.execDetached([Directories.wallpaperSwitchScriptPath, "--mode", "dark", "--noswitch"]);
            }
        },
        {
            action: "konachanwallpaper",
            execute: () => {
                Quickshell.execDetached([Quickshell.shellPath("scripts/colors/random_konachan_wall.sh")]);
            }
        },
        {
            action: "light",
            execute: () => {
                Quickshell.execDetached([Directories.wallpaperSwitchScriptPath, "--mode", "light", "--noswitch"]);
            }
        },
        {
            action: "superpaste",
            execute: args => {
                if (!/^(\d+)/.test(args.trim())) {
                    Quickshell.execDetached(["notify-send", Translation.tr("Superpaste"), Translation.tr("Usage: <tt>%1superpaste NUM_OF_ENTRIES[i]</tt>\nSupply <tt>i</tt> when you want images\nExamples:\n<tt>%1superpaste 4i</tt> for the last 4 images\n<tt>%1superpaste 7</tt> for the last 7 entries").arg(Config.options.search.prefix.action), "-a", "Shell"]);
                    return;
                }
                const syntaxMatch = /^(?:(\d+)(i)?)/.exec(args.trim());
                const count = syntaxMatch[1] ? parseInt(syntaxMatch[1]) : 1;
                const isImage = !!syntaxMatch[2];
                Cliphist.superpaste(count, isImage);
            }
        },
        {
            action: "todo",
            execute: args => {
                Todo.addTask(args);
            }
        },
        {
            action: "wallpaper",
            execute: () => {
                GlobalStates.wallpaperSelectorOpen = true;
            }
        },
    ]

    function focusFirstItem() {
        appResults.currentIndex = 0;
    }

    Timer {
        id: nonAppResultsTimer
        interval: Config.options.search.nonAppResultDelay
        onTriggered: {
            let expr = root.searchingText;
            if (expr.startsWith(Config.options.search.prefix.math)) {
                expr = expr.slice(Config.options.search.prefix.math.length);
            }
            mathProcess.calculateExpression(expr);
        }
    }

    Process {
        id: mathProcess
        property list<string> baseCommand: ["qalc", "-t"]
        function calculateExpression(expression) {
            mathProcess.running = false;
            mathProcess.command = baseCommand.concat(expression);
            mathProcess.running = true;
        }
        stdout: SplitParser {
            onRead: data => {
                root.mathResult = data;
                root.focusFirstItem();
            }
        }
    }

    Keys.onPressed: event => {
        if (event.key === Qt.Key_Escape)
            return;

        if (event.key === Qt.Key_Backspace) {
            if (!searchInput.activeFocus) {
                searchInput.forceActiveFocus();
                if (event.modifiers & Qt.ControlModifier) {
                    let text = searchInput.text;
                    let pos = searchInput.cursorPosition;
                    if (pos > 0) {
                        let left = text.slice(0, pos);
                        let match = left.match(/(\s*\S+)\s*$/);
                        let deleteLen = match ? match[0].length : 1;
                        searchInput.text = text.slice(0, pos - deleteLen) + text.slice(pos);
                        searchInput.cursorPosition = pos - deleteLen;
                    }
                } else {
                    if (searchInput.cursorPosition > 0) {
                        searchInput.text = searchInput.text.slice(0, searchInput.cursorPosition - 1) + searchInput.text.slice(searchInput.cursorPosition);
                        searchInput.cursorPosition -= 1;
                    }
                }
                searchInput.cursorPosition = searchInput.text.length;
                event.accepted = true;
            }
            return;
        }

        if (event.text && event.text.length === 1 && event.key !== Qt.Key_Enter && event.key !== Qt.Key_Return && event.text.charCodeAt(0) >= 0x20) {
            if (!searchInput.activeFocus) {
                searchInput.forceActiveFocus();
                searchInput.text = searchInput.text.slice(0, searchInput.cursorPosition) + event.text + searchInput.text.slice(searchInput.cursorPosition);
                searchInput.cursorPosition += 1;
                event.accepted = true;
            }
        }
    }

    StyledRectangularShadow {
        target: searchWidgetContent
    }
    Rectangle {
        id: searchWidgetContent
        anchors.centerIn: parent
        implicitWidth: columnLayout.implicitWidth
        implicitHeight: columnLayout.implicitHeight
        radius: Appearance.rounding.large
        color: Appearance.colors.colLayer0
        border.width: 1
        border.color: Appearance.colors.colLayer0Border

        ColumnLayout {
            id: columnLayout
            anchors.centerIn: parent
            spacing: 0

            layer.enabled: true
            layer.effect: OpacityMask {
                maskSource: Rectangle {
                    width: searchWidgetContent.width
                    height: searchWidgetContent.width
                    radius: searchWidgetContent.radius
                }
            }

            RowLayout {
                id: searchBar
                spacing: 5
                MaterialSymbol {
                    id: searchIcon
                    Layout.leftMargin: 15
                    iconSize: Appearance.font.pixelSize.huge
                    color: Appearance.m3colors.m3onSurface
                    text: root.searchingText.startsWith(Config.options.search.prefix.clipboard) ? 'content_paste_search' : 'search'
                }
                TextField {
                    id: searchInput
                    focus: GlobalStates.overviewOpen
                    Layout.rightMargin: 15
                    padding: 15
                    renderType: Text.NativeRendering
                    font {
                        family: Appearance?.font.family.main ?? "sans-serif"
                        pixelSize: Appearance?.font.pixelSize.small ?? 15
                        hintingPreference: Font.PreferFullHinting
                    }
                    color: activeFocus ? Appearance.m3colors.m3onSurface : Appearance.m3colors.m3onSurfaceVariant
                    selectedTextColor: Appearance.m3colors.m3onSecondaryContainer
                    selectionColor: Appearance.colors.colSecondaryContainer
                    placeholderText: Translation.tr("Search, calculate or run")
                    placeholderTextColor: Appearance.m3colors.m3outline
                    implicitWidth: root.searchingText == "" ? Appearance.sizes.searchWidthCollapsed : Appearance.sizes.searchWidth

                    Behavior on implicitWidth {
                        id: searchWidthBehavior
                        enabled: false
                        NumberAnimation {
                            duration: 300
                            easing.type: Appearance.animation.elementMove.type
                            easing.bezierCurve: Appearance.animation.elementMove.bezierCurve
                        }
                    }

                    onTextChanged: {
                        root.pendingText = text;
                        typingDebounce.restart();
                    }
                    Timer {
                        id: typingDebounce
                        interval: 85
                        repeat: false
                        onTriggered: root.searchingText = root.pendingText
                    }

                    onAccepted: {
                        if (appResults.count > 0) {
                            let firstItem = appResults.itemAtIndex(0);
                            if (firstItem && firstItem.clicked)
                                firstItem.clicked();
                        }
                    }

                    background: null

                    cursorDelegate: Rectangle {
                        width: 1
                        color: searchInput.activeFocus ? Appearance.colors.colPrimary : "transparent"
                        radius: 1
                    }
                }
            }

            Rectangle {
                visible: root.showResults
                Layout.fillWidth: true
                height: 1
                color: Appearance.colors.colOutlineVariant
            }

            ListView {
                id: appResults
                visible: root.showResults
                Layout.fillWidth: true
                implicitHeight: Math.min(600, appResults.contentHeight + topMargin + bottomMargin)
                clip: true
                topMargin: 10
                bottomMargin: 10
                spacing: 2
                KeyNavigation.up: searchBar
                highlightMoveDuration: 100

                // Load-more on scroll (ListView inherits Flickable)
                Timer {
                    id: loadMoreThrottle
                    interval: 120
                    repeat: false
                    onTriggered: {
                        const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
                        if (root.searchingText.startsWith(glyphPrefix))
                            root.loadMoreGlyphs();
                    }
                }
                onAtYEndChanged: if (atYEnd && !loadMoreThrottle.running)
                    loadMoreThrottle.start()
                onContentYChanged: {
                    const nearEnd = (contentY + height) >= (contentHeight * 0.85);
                    if (nearEnd && !loadMoreThrottle.running)
                        loadMoreThrottle.start();
                }

                onFocusChanged: if (focus)
                    appResults.currentIndex = 1

                Connections {
                    target: root
                    function onSearchingTextChanged() {
                        if (appResults.count > 0)
                            appResults.currentIndex = 0;
                    }
                }

                model: ScriptModel {
                    id: model
                    onValuesChanged: root.focusFirstItem()
                    values: {
                        if (root.searchingText == "")
                            return [];

                        if (root.searchingText.startsWith(Config.options.search.prefix.clipboard)) {
                            const searchString = root.searchingText.slice(Config.options.search.prefix.clipboard.length);
                            return Cliphist.fuzzyQuery(searchString).map(entry => {
                                return {
                                    cliphistRawString: entry,
                                    name: StringUtils.cleanCliphistEntry(entry),
                                    clickActionName: "",
                                    type: `#${entry.match(/^\s*(\S+)/)[1] || ""}`,
                                    execute: () => {
                                        Cliphist.copy(entry);
                                    },
                                    actions: [
                                        {
                                            name: "Copy",
                                            materialIcon: "content_copy",
                                            execute: () => Cliphist.copy(entry)
                                        },
                                        {
                                            name: "Delete",
                                            materialIcon: "delete",
                                            execute: () => Cliphist.deleteEntry(entry)
                                        }
                                    ]
                                };
                            }).filter(Boolean);
                        } else if (root.searchingText.startsWith(Config.options.search.prefix.emojis)) {
                            const searchString = root.searchingText.slice(Config.options.search.prefix.emojis.length);
                            return Emojis.fuzzyQuery(searchString).map(entry => {
                                return {
                                    cliphistRawString: entry,
                                    bigText: entry.match(/^\s*(\S+)/)[1] || "",
                                    name: entry.replace(/^\s*\S+\s+/, ""),
                                    clickActionName: "",
                                    type: "Emoji",
                                    execute: () => {
                                        Quickshell.clipboardText = entry.match(/^\s*(\S+)/)[1];
                                    }
                                };
                            }).filter(Boolean);
                        } else if (root.searchingText.startsWith(Config.options.search.prefix.glyphs)) {
                            const glyphPrefix = Config?.options?.search?.prefix?.glyphs ?? ".";
                            if (root.searchingText.startsWith(glyphPrefix)) {
                                const searchString = root.searchingText.slice(glyphPrefix.length);

                                if (root.glyphLastQuery !== searchString && !NerdGlyphs.loading) {
                                    root.rebuildGlyphResults(searchString);
                                }

                                if (NerdGlyphs.loading && root.glyphPaged.length === 0) {
                                    return [
                                        {
                                            name: Translation.tr("Loading Nerd Fonts…"),
                                            type: "Nerd Font",
                                            materialSymbol: "hourglass_top",
                                            clickActionName: ""
                                        }
                                    ];
                                }
                                if (!NerdGlyphs.loading && root.glyphAll.length === 0 && searchString.length > 0) {
                                    return [
                                        {
                                            name: Translation.tr("No results"),
                                            type: "Nerd Font",
                                            materialSymbol: "search_off",
                                            clickActionName: ""
                                        }
                                    ];
                                }

                                return root.glyphPaged.map(entry => {
                                    const line = String(entry).trim();
                                    if (!line)
                                        return null;

                                    const firstSpace = line.indexOf(" ");
                                    const glyph = firstSpace === -1 ? line : line.slice(0, firstSpace);

                                    const idMatch = line.match(/\s(nf-[a-z0-9-]+)$/i);
                                    const nfId = idMatch ? idMatch[1] : "";

                                    const label = line.replace(/^\s*\S+\s+/, "").replace(/\s+nf-[a-z0-9-]+$/i, "").trim();

                                    return {
                                        bigText: glyph,
                                        name: label || nfId || glyph,
                                        subtitle: nfId,
                                        clickActionName: "",
                                        type: "Nerd Font",
                                        execute: () => {
                                            Quickshell.clipboardText = glyph;
                                        },
                                        actions: [
                                            {
                                                name: "Copy glyph",
                                                materialIcon: "content_copy",
                                                execute: () => {
                                                    Quickshell.clipboardText = glyph;
                                                }
                                            },
                                            nfId ? {
                                                name: "Copy class",
                                                materialIcon: "sell",
                                                execute: () => {
                                                    Quickshell.clipboardText = nfId;
                                                }
                                            } : null].filter(Boolean)
                                    };
                                }).filter(Boolean);
                            }
                        }

                        ////////////////// Init ///////////////////
                        nonAppResultsTimer.restart();
                        const mathResultObject = {
                            name: root.mathResult,
                            clickActionName: Translation.tr("Copy"),
                            type: Translation.tr("Math result"),
                            fontType: "monospace",
                            materialSymbol: 'calculate',
                            execute: () => {
                                Quickshell.clipboardText = root.mathResult;
                            }
                        };
                        const commandResultObject = {
                            name: searchingText.replace("file://", ""),
                            clickActionName: Translation.tr("Run"),
                            type: Translation.tr("Run command"),
                            fontType: "monospace",
                            materialSymbol: 'terminal',
                            execute: () => {
                                let cleanedCommand = root.searchingText.replace("file://", "");
                                if (cleanedCommand.startsWith(Config.options.search.prefix.shellCommand))
                                    cleanedCommand = cleanedCommand.slice(Config.options.search.prefix.shellCommand.length);
                                Quickshell.execDetached(["bash", "-c", searchingText.startsWith('sudo') ? `${Config.options.apps.terminal} fish -C '${cleanedCommand}'` : cleanedCommand]);
                            }
                        };
                        const webSearchResultObject = {
                            name: root.searchingText,
                            clickActionName: Translation.tr("Search"),
                            type: Translation.tr("Search the web"),
                            materialSymbol: 'travel_explore',
                            execute: () => {
                                let query = root.searchingText;
                                if (query.startsWith(Config.options.search.prefix.webSearch))
                                    query = query.slice(Config.options.search.prefix.webSearch.length);
                                let url = Config.options.search.engineBaseUrl + query;
                                for (let site of Config.options.search.excludedSites)
                                    url += ` -site:${site}`;
                                Qt.openUrlExternally(url);
                            }
                        };
                        const launcherActionObjects = root.searchActions.map(action => {
                            const actionString = `${Config.options.search.prefix.action}${action.action}`;
                            if (actionString.startsWith(root.searchingText) || root.searchingText.startsWith(actionString)) {
                                return {
                                    name: root.searchingText.startsWith(actionString) ? root.searchingText : actionString,
                                    clickActionName: Translation.tr("Run"),
                                    type: Translation.tr("Action"),
                                    materialSymbol: 'settings_suggest',
                                    execute: () => {
                                        action.execute(root.searchingText.split(" ").slice(1).join(" "));
                                    }
                                };
                            }
                            return null;
                        }).filter(Boolean);

                        //////// Prioritized by prefix /////////
                        let result = [];
                        const startsWithNumber = /^\d/.test(root.searchingText);
                        const startsWithMathPrefix = root.searchingText.startsWith(Config.options.search.prefix.math);
                        const startsWithShellCommandPrefix = root.searchingText.startsWith(Config.options.search.prefix.shellCommand);
                        const startsWithWebSearchPrefix = root.searchingText.startsWith(Config.options.search.prefix.webSearch);
                        if (startsWithNumber || startsWithMathPrefix) {
                            result.push(mathResultObject);
                        } else if (startsWithShellCommandPrefix) {
                            result.push(commandResultObject);
                        } else if (startsWithWebSearchPrefix) {
                            result.push(webSearchResultObject);
                        }

                        //////////////// Apps //////////////////
                        result = result.concat(AppSearch.fuzzyQuery(root.searchingText).map(entry => {
                            entry.clickActionName = Translation.tr("Launch");
                            entry.type = Translation.tr("App");
                            return entry;
                        }));

                        ////////// Launcher actions ////////////
                        result = result.concat(launcherActionObjects);

                        /// Math result, command, web search ///
                        if (Config.options.search.prefix.showDefaultActionsWithoutPrefix) {
                            if (!startsWithShellCommandPrefix)
                                result.push(commandResultObject);
                            if (!startsWithNumber && !startsWithMathPrefix)
                                result.push(mathResultObject);
                            if (!startsWithWebSearchPrefix)
                                result.push(webSearchResultObject);
                        }

                        return result;
                    }
                }

                delegate: SearchItem {
                    required property var modelData
                    anchors.left: parent?.left
                    anchors.right: parent?.right
                    entry: modelData
                    query: root.searchingText.startsWith(Config.options.search.prefix.clipboard) ? root.searchingText.slice(Config.options.search.prefix.clipboard.length) : root.searchingText
                }
            }
        }
    }
}
