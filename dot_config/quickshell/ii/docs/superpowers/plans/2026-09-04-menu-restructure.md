# Menu Restructure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the left sidebar (AI chat + translator), add a tabbed top menu (Dashboard / Planner / Stats with Claude usage), and reshape the right sidebar into a quick-settings + notifications panel.

**Architecture:** Quickshell (QML) config "ii" at `~/.config/quickshell/ii`, loaded via `shell.qml` LazyLoaders, one Scope per surface, `GlobalStates` singleton for open/close state, Hyprland global shortcuts bound in `~/.config/hypr/hyprland/keybinds.lua` (Lua config: `hyprctl keyword` is rejected; use `hyprctl eval 'hl.config({...})'`; dispatchers are `hl.dsp.*`). New top menu clones the left sidebar's Scope/PanelWindow/FocusGrab pattern with top anchoring.

**Tech Stack:** QML (Quickshell, Qt6), Hyprland Lua config, bash, `bunx ccusage` (npm, run ad-hoc), Claude Code statusline JSON.

**Spec:** `docs/superpowers/specs/2026-09-04-menu-restructure-design.md`

## Global Constraints

- The media player must keep its current styling: embed `PlayerControl` from `modules/mediaControls/` unmodified wherever possible.
- `Translation.qml` (UI i18n) and `translations/` stay — only the translator *feature* (left sidebar tab) is removed.
- No behavior changes to notifications, quick toggles internals, or the bar beyond what tasks state.
- Verification after every task: `qs -c ii` must reload without errors; check with `timeout 3 qs -c ii log 2>&1 | grep -iE "error|cannot|failed" | grep -v Warp`.
- There is no test framework for this QML config: each task's "test" is a reload check plus the stated manual/CLI verification. Run them; do not skip.
- Backup exists at `~/config-backup-menus-20260904-191210.tar.gz`. Do not delete it.
- Commit after every task (Task 0 creates the repo).

---

### Task 0: Initialize git repository for stepwise rollback

**Files:**
- Create: `~/.config/quickshell/ii/.git` (repo), `~/.config/quickshell/ii/.gitignore`

**Interfaces:**
- Produces: git history; later tasks commit to it.

- [ ] **Step 1: Init repo and ignore runtime dirs**

```bash
cd ~/.config/quickshell/ii
git init
printf '.superpowers/\n' > .gitignore
git add -A
git commit -m "chore: snapshot before menu restructure"
```

- [ ] **Step 2: Verify**

Run: `git -C ~/.config/quickshell/ii log --oneline`
Expected: one commit.

Note: `~/.config/hypr` is not a repo; hypr edits in later tasks are small and covered by the tarball backup.

---

### Task 1: Remove left sidebar module and AI service

**Files:**
- Delete: `modules/sidebarLeft/` (whole directory), `services/Ai.qml`, `services/ai/` (whole directory)
- Modify: `shell.qml`, `GlobalStates.qml`, `modules/background/Background.qml`, `modules/screenCorners/ScreenCorners.qml`

**Interfaces:**
- Consumes: nothing.
- Produces: `GlobalStates.sidebarLeftOpen` no longer exists; any remaining reference is a bug caught by reload check.

- [ ] **Step 1: Delete directories**

```bash
cd ~/.config/quickshell/ii
git rm -r modules/sidebarLeft services/Ai.qml services/ai
```

- [ ] **Step 2: Edit `shell.qml`**

Remove line `import qs.modules.sidebarLeft`, the property line `property bool enableSidebarLeft: true`, and the loader line:
```qml
    LazyLoader { active: enableSidebarLeft; component: SidebarLeft {} }
```

- [ ] **Step 3: Edit `GlobalStates.qml`**

Remove line 13: `property bool sidebarLeftOpen: false`

- [ ] **Step 4: Fix remaining references**

Run: `grep -rn "sidebarLeft" --include="*.qml" .`
For each hit in `Background.qml`, `ScreenCorners.qml`, and any other file: remove the condition or branch that references `GlobalStates.sidebarLeftOpen` (typically visibility/dim logic — drop the `sidebarLeftOpen` term from the boolean expression, keep the rest).
Do NOT touch `modules/bar/` hits yet (Task 2) — if the grep shows bar hits, leave them.

- [ ] **Step 5: Reload check**

Run: `timeout 3 qs -c ii log 2>&1 | grep -iE "error|cannot|failed" | grep -v Warp`
Expected: no new errors mentioning sidebarLeft, Ai, or missing imports. If quickshell did not auto-reload, run `qs kill -c ii; qs -c ii -d` first.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: remove left sidebar module and AI service"
```

---

### Task 2: Remove left sidebar bar button, keybinds, and dead config options

**Files:**
- Delete: `modules/bar/LeftSidebarButton.qml`
- Modify: `modules/bar/BarContent.qml`, `modules/verticalBar/VerticalBarContent.qml`, `~/.config/hypr/hyprland/keybinds.lua`, `modules/common/Config.qml` (or wherever `policies.ai` defaults live — find with grep), `settings.qml`

**Interfaces:**
- Produces: `SUPER + A` freed for the top menu (Task 3 rebinds it).

- [ ] **Step 1: Remove bar button**

```bash
git rm modules/bar/LeftSidebarButton.qml
grep -n "LeftSidebarButton" modules/bar/BarContent.qml modules/verticalBar/VerticalBarContent.qml
```
Remove each instantiation block (the element and any layout spacing tied to it).

- [ ] **Step 2: Remove keybinds**

In `~/.config/hypr/hyprland/keybinds.lua` delete lines 31-34:
```lua
hl.bind("SUPER + A", hl.dsp.global("quickshell:sidebarLeftToggle"), { description = "Toggle left sidebar" }) -- Toggle left sidebar
hl.bind("SUPER+ALT + A", hl.dsp.global("quickshell:sidebarLeftToggleDetach")) -- [hidden]
hl.bind("SUPER + B", hl.dsp.global("quickshell:sidebarLeftToggle")) -- [hidden]
hl.bind("SUPER + O", hl.dsp.global("quickshell:sidebarLeftToggle")) -- [hidden]
```

- [ ] **Step 3: Remove AI/translator config options**

Run: `grep -rn "policies.ai\|\"ai\"\|translator" --include="*.qml" -i . | grep -v Translation`
Remove: the `ai` policy default, AI model options, translator options from the config schema file, and their sections in `settings.qml`. Keep unrelated `policies.*` entries (e.g. weeb).

- [ ] **Step 4: Verify**

```bash
hyprctl reload && hyprctl binds | grep -c sidebarLeft   # expected: 0
timeout 3 qs -c ii log 2>&1 | grep -iE "error|cannot" | grep -v Warp
grep -rn "sidebarLeft" --include="*.qml" . | wc -l       # expected: 0
```

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: remove left sidebar button, keybinds, and AI/translator options"
```

---

### Task 3: Top menu skeleton (panel, state, shortcuts, keybind)

**Files:**
- Create: `modules/topMenu/TopMenu.qml`, `modules/topMenu/TopMenuContent.qml`
- Modify: `shell.qml`, `GlobalStates.qml`, `~/.config/hypr/hyprland/keybinds.lua`

**Interfaces:**
- Produces: `GlobalStates.topMenuOpen` (bool), global shortcuts `quickshell:topMenuToggle/Open/Close`, IPC target `topMenu`, `TopMenuContent` with `property int selectedTab` and tab list — Tasks 4/5/7/8 fill the tabs.

- [ ] **Step 1: Add state**

`GlobalStates.qml`, next to the other open flags:
```qml
    property bool topMenuOpen: false
```

- [ ] **Step 2: Create `modules/topMenu/TopMenu.qml`**

Clone the deleted `SidebarLeft.qml` pattern (Task 1's git history has it: `git show HEAD~2:modules/sidebarLeft/SidebarLeft.qml` — after Task 1+2 commits; adjust the ref if needed). Changes from that source:
- `Scope` keeps the same Loader/detach machinery but rename all `sidebarLeft*` identifiers to `topMenu*` and drop the detach feature entirely (no `detach` property, no `detachedSidebarLoader`, no `toggleDetach` shortcut) — YAGNI.
- Window anchors: `top: true, left: true, right: true` (no bottom); panel is horizontally centered by the background item instead of filling: background `Rectangle` uses `anchors.top: parent.top; anchors.horizontalCenter: parent.horizontalCenter`, `implicitWidth: 860`, `height: 520`, margins `Appearance.sizes.hyprlandGapsOut`, same color/border/radius tokens as the old sidebar background.
- `WlrLayershell.namespace: "quickshell:topMenu"`.
- `visible: GlobalStates.topMenuOpen`; `hide()` sets it false; Escape hides; `HyprlandFocusGrab` identical pattern.
- `implicitWidth`: 860 + `Appearance.sizes.elevationMargin * 2`; `implicitHeight`: 520 + `Appearance.sizes.elevationMargin * 2`.
- IPC + shortcuts:
```qml
    IpcHandler {
        target: "topMenu"
        function toggle(): void { GlobalStates.topMenuOpen = !GlobalStates.topMenuOpen }
        function close(): void { GlobalStates.topMenuOpen = false }
        function open(): void { GlobalStates.topMenuOpen = true }
    }
    GlobalShortcut { name: "topMenuToggle"; description: "Toggles top menu on press"
        onPressed: GlobalStates.topMenuOpen = !GlobalStates.topMenuOpen }
    GlobalShortcut { name: "topMenuOpen"; description: "Opens top menu on press"
        onPressed: GlobalStates.topMenuOpen = true }
    GlobalShortcut { name: "topMenuClose"; description: "Closes top menu on press"
        onPressed: GlobalStates.topMenuOpen = false }
```

- [ ] **Step 3: Create `modules/topMenu/TopMenuContent.qml`**

Clone the tab strip pattern from `modules/sidebarRight/SidebarRightContent.qml`'s sibling `SidebarLeftContent.qml` (`git show` as above): `PrimaryTabBar` + `SwipeView`-style content with `tabButtonList` and keyboard cycling (Tab/Backtab/arrow keys — copy that key handling verbatim). Tabs (placeholders until later tasks):
```qml
    property var tabButtonList: [
        {"icon": "dashboard", "name": Translation.tr("Dashboard")},
        {"icon": "calendar_month", "name": Translation.tr("Planner")},
        {"icon": "monitoring", "name": Translation.tr("Stats")}
    ]
```
Each tab body: an empty `Item` with a centered `StyledText { text: "..." }` placeholder.

- [ ] **Step 4: Wire into `shell.qml`**

Add `import qs.modules.topMenu`, `property bool enableTopMenu: true`, and:
```qml
    LazyLoader { active: enableTopMenu; component: TopMenu {} }
```

- [ ] **Step 5: Bind keys**

`keybinds.lua`, where the removed binds were:
```lua
hl.bind("SUPER + A", hl.dsp.global("quickshell:topMenuToggle"), { description = "Toggle top menu" }) -- Toggle top menu
```

- [ ] **Step 6: Verify**

```bash
hyprctl reload
hyprctl globalshortcuts | grep topMenu    # expected: 3 entries
qs -c ii ipc call topMenu toggle          # panel appears top-center
qs -c ii ipc call topMenu close
timeout 3 qs -c ii log 2>&1 | grep -iE "error|cannot" | grep -v Warp
```
Manual: SUPER+A opens, Escape and click-outside close, tab keys cycle.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: add top menu skeleton with tabs, shortcuts, and keybind"
```

---

### Task 4: Planner tab (move calendar, todo, pomodoro)

**Files:**
- Create: `modules/topMenu/PlannerTab.qml`
- Modify: `modules/topMenu/TopMenuContent.qml`, `modules/sidebarRight/SidebarRightContent.qml`
- Delete: `modules/sidebarRight/BottomWidgetGroup.qml`
- Move: `modules/sidebarRight/calendar/` → `modules/topMenu/calendar/`, same for `todo/`, `pomodoro/`

**Interfaces:**
- Consumes: Task 3's tab placeholders.
- Produces: right sidebar no longer shows the bottom group; planner widgets live in the top menu.

- [ ] **Step 1: Move widget directories**

```bash
cd ~/.config/quickshell/ii
git mv modules/sidebarRight/calendar modules/topMenu/calendar
git mv modules/sidebarRight/todo modules/topMenu/todo
git mv modules/sidebarRight/pomodoro modules/topMenu/pomodoro
grep -rn "sidebarRight.calendar\|sidebarRight.todo\|sidebarRight.pomodoro" --include="*.qml" .
```
Update every import hit to `qs.modules.topMenu.<name>`.

- [ ] **Step 2: Create `PlannerTab.qml`**

Row of three panes side by side (the top menu is wide; no tabs-within-tabs). Reuse the widgets exactly as `BottomWidgetGroup.qml` instantiated them (see `git show HEAD:modules/sidebarRight/BottomWidgetGroup.qml` before deleting for the exact component names and required properties):
```qml
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.topMenu.calendar
import qs.modules.topMenu.todo
import qs.modules.topMenu.pomodoro
import QtQuick
import QtQuick.Layouts

RowLayout {
    spacing: 10
    // One pane per widget; each pane mirrors BottomWidgetGroup's instantiation
    // of that widget (same component name, same properties), wrapped in a
    // Rectangle with color Appearance.colors.colLayer1 and
    // radius Appearance.rounding.normal, Layout.fillWidth/fillHeight true.
}
```
Fill in the three panes from the old file's instantiations verbatim.

- [ ] **Step 3: Remove bottom group from right sidebar**

`git rm modules/sidebarRight/BottomWidgetGroup.qml`; in `SidebarRightContent.qml` remove its instantiation and imports of calendar/todo/pomodoro. Let the notification/center group expand into the freed space (change its `Layout.fillHeight` or remove a fixed-height constraint if one bound to the bottom group).

- [ ] **Step 4: Wire tab**

In `TopMenuContent.qml` replace the Planner placeholder with `PlannerTab {}`.

- [ ] **Step 5: Verify**

Reload check command from Global Constraints. Manual: Planner tab shows all three widgets working (click a calendar day, check a todo, start/stop pomodoro); right sidebar bottom group gone, notifications fill space; `Persistent.states.sidebar.bottomGroup` references removed or unused (grep it — if `Persistent` schema errors on unknown keys, drop the state entry too).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: move planner widgets to top menu Planner tab"
```

---

### Task 5: Dashboard tab (media player + glance row)

**Files:**
- Create: `modules/topMenu/DashboardTab.qml`
- Modify: `modules/topMenu/TopMenuContent.qml`

**Interfaces:**
- Consumes: `PlayerControl` from `qs.modules.mediaControls` (existing popup component — check its required properties at the top of `modules/mediaControls/PlayerControl.qml`; it takes a player from `MprisController`), `Todo` service, `TimerService`.
- Produces: Dashboard tab. Claude gauge placeholder filled by Task 7.

- [ ] **Step 1: Create `DashboardTab.qml`**

```qml
import qs.modules.common
import qs.modules.common.widgets
import qs.modules.mediaControls
import qs.services
import QtQuick
import QtQuick.Layouts

ColumnLayout {
    spacing: 10
    // Media player, unchanged styling: instantiate PlayerControl exactly as
    // MediaControls.qml does (same property bindings; typically one per
    // MprisController.meaningfulPlayers entry — copy that Repeater/delegate
    // structure, bounded to the first player or a small list).
    // Below it, the glance row:
    RowLayout {
        Layout.fillWidth: true
        spacing: 10
        // Pane 1: next todo — first unchecked item from Todo.list
        // Pane 2: pomodoro state from TimerService (running/idle + remaining)
        // Pane 3: Claude 5h gauge placeholder — StyledText "Claude: --%" with
        //         objectName "claudeGlance" (Task 7 replaces it)
        // Each pane: Rectangle, color Appearance.colors.colLayer1,
        // radius Appearance.rounding.normal, Layout.fillWidth: true
    }
}
```
Copy `MediaControls.qml`'s `PlayerControl` instantiation verbatim (bindings included) so styling is identical. Read `Todo.qml` and `TimerService.qml` service APIs before wiring the panes; show simple text (icon + one line), no new styling systems.

- [ ] **Step 2: Wire tab, verify, commit**

Replace Dashboard placeholder with `DashboardTab {}`. Reload check. Manual: play music (any MPRIS player), open Dashboard tab — player card must look identical to the SUPER+M media popup. Commit: `git add -A && git commit -m "feat: add Dashboard tab with embedded media player and glance row"`.

---

### Task 6: Claude statusline tee + ClaudeUsage service (live window metrics)

**Files:**
- Create: `~/.config/claude-widget/statusline-tee.sh`, `services/ClaudeUsage.qml`
- Modify: `~/.claude/settings.json` (statusline command)

**Interfaces:**
- Produces: `ClaudeUsage` singleton with properties: `fiveHourPct` (real, 0-100 or -1 when unknown), `fiveHourResetsAt` (real, epoch secs, -1 unknown), `weeklyPct` (real, -1 unknown), `sessionCostUsd` (real), `contextPct` (real), `live` (bool — statusline file fresher than 10 min). Task 7 extends this file; Tasks 5/7 consume it.

- [ ] **Step 1: Check existing statusline config**

Run: `grep -A3 statusLine ~/.claude/settings.json; grep -A3 statusline ~/.claude/settings.json`
If a statusline command exists, the tee script must exec it after teeing (chain, not replace). If none, the script just consumes stdin.

- [ ] **Step 2: Create the tee script**

`~/.config/claude-widget/statusline-tee.sh` (then `chmod +x`):
```bash
#!/usr/bin/env bash
# Claude Code statusline hook: persist the status JSON for desktop widgets,
# then delegate to the original statusline command if one is configured.
mkdir -p ~/.cache/claude-widget
tee ~/.cache/claude-widget/status.json
# If a previous statusline command existed, replace the line above with:
#   tee ~/.cache/claude-widget/status.json | exec <original command>
```
Wire into `~/.claude/settings.json`:
```json
"statusLine": { "type": "command", "command": "~/.config/claude-widget/statusline-tee.sh" }
```
(Merge into existing JSON — do not clobber other keys. Field name is `statusLine`.)

- [ ] **Step 3: Verify tee**

Statusline runs on Claude Code turns. Simulate instead:
```bash
echo '{"rate_limits":{"five_hour":{"used_percentage":42,"resets_at":1790000000},"seven_day":{"used_percentage":10}},"cost":{"total_cost_usd":1.23},"context_window":{"used_percentage":55}}' | ~/.config/claude-widget/statusline-tee.sh
jq .rate_limits.five_hour.used_percentage ~/.cache/claude-widget/status.json   # expected: 42
```

- [ ] **Step 4: Create `services/ClaudeUsage.qml`**

Follow the singleton pattern of existing services (see `services/ResourceUsage.qml` for the `Singleton` + `FileView`/`Process` idioms):
```qml
pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io

Singleton {
    id: root
    property real fiveHourPct: -1
    property real fiveHourResetsAt: -1
    property real weeklyPct: -1
    property real sessionCostUsd: 0
    property real contextPct: -1
    property bool live: false

    property string statusPath: Quickshell.env("HOME") + "/.cache/claude-widget/status.json"

    FileView {
        id: statusFile
        path: root.statusPath
        watchChanges: true
        onFileChanged: reload()
        onLoaded: root.parseStatus(statusFile.text())
    }

    // Freshness: re-evaluate every minute
    Timer {
        interval: 60000; running: true; repeat: true
        onTriggered: statusFile.reload()
    }

    function parseStatus(text) {
        try {
            const j = JSON.parse(text);
            fiveHourPct = j.rate_limits?.five_hour?.used_percentage ?? -1;
            fiveHourResetsAt = j.rate_limits?.five_hour?.resets_at ?? -1;
            weeklyPct = j.rate_limits?.seven_day?.used_percentage ?? -1;
            sessionCostUsd = j.cost?.total_cost_usd ?? 0;
            contextPct = j.context_window?.used_percentage ?? -1;
            live = true; // refined in Step 5
        } catch (e) {
            live = false;
        }
    }
}
```

- [ ] **Step 5: Staleness**

`live` must be false when the file's mtime is older than 10 minutes. FileView lacks mtime; use a `Process` running `stat -c %Y` on the timer tick:
```qml
    Process {
        id: mtimeCheck
        command: ["stat", "-c", "%Y", root.statusPath]
        stdout: StdioCollector {
            onStreamFinished: root.live = (Date.now()/1000 - parseInt(text)) < 600
        }
    }
```
Trigger `mtimeCheck.running = true` from the minute Timer and after `parseStatus`.

- [ ] **Step 6: Verify**

```bash
qs -c ii ipc call topMenu open   # no errors; service loads lazily on first use — force by referencing in Step 7 of Task 7, so for now just:
timeout 3 qs -c ii log 2>&1 | grep -iE "error|cannot" | grep -v Warp
```
Then re-run the Step 3 echo command and confirm no QML parse errors appear in the log.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -m "feat: add ClaudeUsage service fed by statusline tee script"
```
Also note in the commit body that `~/.claude/settings.json` and `~/.config/claude-widget/` changed outside the repo.

---

### Task 7: ccusage integration + Claude panel in Stats tab + glance gauge

**Files:**
- Modify: `services/ClaudeUsage.qml`, `modules/topMenu/DashboardTab.qml`
- Create: `modules/topMenu/StatsTab.qml`, `modules/topMenu/ClaudePanel.qml`

**Interfaces:**
- Consumes: `ClaudeUsage` properties from Task 6.
- Produces: `ClaudeUsage` gains: `todayTokens` (int), `todayCostUsd` (real), `dailyTrend` (var — array of `{date, tokens, cost}` for up to 30 days), `topProjects` (var — array of `{name, tokens, cost}`, max 5), `ccusageOk` (bool), `refreshCcusage()` (function). `StatsTab` with `ClaudePanel` (left half) and a system pane placeholder (Task 8 fills).

- [ ] **Step 1: Extend service with ccusage polling**

Add to `ClaudeUsage.qml`:
```qml
    property int todayTokens: 0
    property real todayCostUsd: 0
    property var dailyTrend: []
    property var topProjects: []
    property bool ccusageOk: false
    property bool pollingEnabled: false   // StatsTab sets true while visible

    function refreshCcusage() { dailyProc.running = true; instancesProc.running = true }

    Timer {
        interval: 120000; running: root.pollingEnabled; repeat: true
        triggeredOnStart: true
        onTriggered: root.refreshCcusage()
    }

    Process {
        id: dailyProc
        command: ["bunx", "ccusage", "daily", "--json"]
        stdout: StdioCollector {
            onStreamFinished: {
                try {
                    const j = JSON.parse(text);
                    const days = j.daily ?? [];
                    root.dailyTrend = days.slice(-30).map(d => ({
                        date: d.date, tokens: d.totalTokens, cost: d.totalCost }));
                    const today = new Date().toISOString().slice(0,10);
                    const t = days.find(d => d.date === today);
                    root.todayTokens = t ? t.totalTokens : 0;
                    root.todayCostUsd = t ? t.totalCost : 0;
                    root.ccusageOk = true;
                } catch (e) { root.ccusageOk = false; }
            }
        }
    }

    Process {
        id: instancesProc
        command: ["bash", "-c", "bunx ccusage daily --instances --json"]
        stdout: StdioCollector {
            onStreamFinished: {
                try {
                    const j = JSON.parse(text);
                    // Aggregate per project across days; shape verified in Step 2.
                    root.topProjects = root.aggregateProjects(j);
                } catch (e) {}
            }
        }
    }
```
Before finalizing field names, verify actual JSON shape: run `bunx ccusage daily --json | jq 'keys, .daily[0]'` and `bunx ccusage daily --instances --json | jq 'keys'` and adapt the parse code to the real schema (ccusage versions differ). Write `aggregateProjects()` against the verified shape (sum tokens/cost per project key, sort desc, slice 5).

- [ ] **Step 2: Verify schema and parsing**

```bash
bunx ccusage daily --json | jq '.daily[-1]'
bunx ccusage daily --instances --json | jq '.' | head -40
```
Expected: real numbers. Adjust QML parse fields to match exactly.

- [ ] **Step 3: Create `ClaudePanel.qml`**

Sections top-to-bottom (all existing common widgets: `StyledText`, `StyledProgressBar` if present — check `modules/common/widgets/` for the progress/slider component the right sidebar uses and reuse it; no new styling):
1. 5h window: progress bar bound to `ClaudeUsage.fiveHourPct`, label with reset countdown (`fiveHourResetsAt` minus now, formatted mm); greyed (opacity 0.5, "no active session") when `!ClaudeUsage.live`.
2. Weekly: same pattern with `weeklyPct`, hidden when -1.
3. Today: `todayTokens` (human-formatted, e.g. 1.2M) + `todayCostUsd` labeled "API-equivalent".
4. Trend: 30-day bar mini-chart — `Repeater` over `dailyTrend`, one `Rectangle` per day, height proportional to tokens/max(tokens), color `Appearance.colors.colPrimary` or nearest token used by existing charts (check `ResourceUsage` consumers for precedent).
5. Top projects: `Repeater` over `topProjects`, row of name + tokens.
6. When `!ccusageOk`: replace 3-5 with hint text: "ccusage unavailable — install: npm i -g ccusage (or keep bunx)".
Set `ClaudeUsage.pollingEnabled = visible` via `onVisibleChanged`.

- [ ] **Step 4: Create `StatsTab.qml`, wire, glance gauge**

`StatsTab.qml`: `RowLayout` — left `ClaudePanel {}`, right a placeholder pane (`StyledText { text: "System" }`) for Task 8. Wire into `TopMenuContent.qml` Stats placeholder. In `DashboardTab.qml` replace the `claudeGlance` placeholder with a small progress bar + `%` bound to `ClaudeUsage.fiveHourPct` (same greyed-when-stale rule).

- [ ] **Step 5: Verify**

Reload check. Open Stats tab: numbers match `bunx ccusage daily` output for today; run the Task 6 Step 3 echo with a fresh timestamp and confirm the 5h gauge updates (FileView watch). Kill test: `mv ~/.cache/claude-widget/status.json{,.bak}` — gauge greys out within a minute; restore.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: add Claude usage panel with ccusage trends and live rate-limit gauge"
```

---

### Task 8: System stats pane + updater move

**Files:**
- Create: `modules/topMenu/SystemPanel.qml`
- Modify: `modules/topMenu/StatsTab.qml`, `services/ResourceUsage.qml` (only if temps/disk missing), `modules/sidebarRight/SidebarRightContent.qml` (remove updater), move `modules/sidebarRight/updater/` → `modules/topMenu/updater/` (if it is a directory; adjust to the actual file layout found)

**Interfaces:**
- Consumes: `ResourceUsage` (existing: CPU/RAM — read the file first to learn exact property names), updater component.
- Produces: complete Stats tab.

- [ ] **Step 1: Audit existing service**

Read `services/ResourceUsage.qml`. Note which of CPU load, RAM, swap, temperature, disk usage it already exposes and the exact property names.

- [ ] **Step 2: Extend service for missing metrics**

Temperature (if missing): `Process` polling
```bash
bash -c "cat /sys/class/hwmon/hwmon*/temp1_input 2>/dev/null | sort -rn | head -1"
```
(millidegrees; divide by 1000; expose `property real cpuTempC`). Disk (if missing): `Process` on `df --output=target,pcent,avail -x tmpfs -x devtmpfs -x efivarfs`, parse to `property var mounts` array of `{target, pcent, avail}`. Poll both on a 5s Timer gated by a `pollingEnabled` property the StatsTab sets, mirroring Task 7's pattern.

- [ ] **Step 3: Create `SystemPanel.qml`**

Column: CPU bar, RAM bar, temp label, per-mount disk bars (Repeater over `mounts`), updater widget at bottom (instantiate exactly as the right sidebar did — find with `grep -rn "updater" modules/sidebarRight/ -i` before moving). Reuse the same progress-bar widget as ClaudePanel.

- [ ] **Step 4: Move updater out of right sidebar**

`git mv` the updater files to `modules/topMenu/updater/`, update imports, remove instantiation from `SidebarRightContent.qml` (or its widget group file).

- [ ] **Step 5: Verify + commit**

Reload check; Stats tab shows live CPU/RAM/temp/disk; updater renders and its actions work; right sidebar shows no updater. Sanity: temp within 30-95, disk pcents match `df`. Commit: `git add -A && git commit -m "feat: add system stats panel and move updater to top menu"`.

---

### Task 9: Right sidebar reshape (sliders, drill-ins, session row)

**Files:**
- Modify: `modules/sidebarRight/SidebarRightContent.qml`, `modules/sidebarRight/CenterWidgetGroup.qml`, quickToggles files as found
- Possibly create: `modules/sidebarRight/DrillInView.qml`

**Interfaces:**
- Consumes: existing `volumeMixer/`, `wifiNetworks/`, `bluetoothDevices/` components.
- Produces: final right sidebar per spec.

- [ ] **Step 1: Audit current structure**

Read `SidebarRightContent.qml` and `CenterWidgetGroup.qml` fully. Map: where volumeMixer/wifiNetworks/bluetoothDevices currently render (likely tabs in CenterWidgetGroup), where toggles row is, whether a session/power row exists (check `modules/sessionScreen` trigger buttons in the sidebar header). THEN adapt the following steps to reality — the intent is fixed, the mechanics follow the code found.

- [ ] **Step 2: Restructure content column**

Target order in `SidebarRightContent.qml`: header (unchanged) → quickToggles grid (unchanged) → sliders block (volume + brightness — reuse the OSD slider widget or mixer's slider component; volume slider gets a chevron button) → notifications (fill height) → bottom session/power row (keep if exists, else skip — do not invent one if none exists; spec's "existing pattern" means keep what's there).

- [ ] **Step 3: Drill-in navigation**

Implement one mechanism: a `StackView`-like swap in the sidebar content area (QML `StackView` or the config's existing pattern if CenterWidgetGroup already swaps views — prefer existing pattern). Drill-ins: volume chevron → volumeMixer view; NetworkToggle right-click or chevron → wifiNetworks view; BluetoothToggle likewise → bluetoothDevices view. Each drill-in view gets a back button header (icon "arrow_back", closes to main view). Check how `wifiNetworks`/`bluetoothDevices` are currently opened (grep for their component names) — if toggles already navigate to them, keep that trigger wiring and only re-root where they render.

- [ ] **Step 4: Verify + commit**

Reload check. Manual: toggles work; volume slider live; chevron opens mixer, back returns; wifi/bt drill-ins list devices and connect; notifications fill remaining space and clear correctly. Commit: `git add -A && git commit -m "feat: reshape right sidebar into quick settings with drill-in views"`.

---

### Task 10: Cleanup, dead-reference sweep, bonus zoom fix

**Files:**
- Modify: `GlobalStates.qml:31`, any files the sweep finds

**Interfaces:** none new.

- [ ] **Step 1: Fix broken zoom (same Lua-parser bug class)**

`GlobalStates.qml:31` uses `hyprctl keyword` — rejected by the Lua config ("keyword can't work with non-legacy parsers"). Replace:
```qml
        Quickshell.execDetached(["hyprctl", "keyword", "cursor:zoom_factor", root.screenZoom.toString()]);
```
with:
```qml
        Quickshell.execDetached(["hyprctl", "eval", `hl.config({ cursor = { zoom_factor = ${root.screenZoom} } })`]);
```
Verify: `qs -c ii ipc call zoom zoomIn` zooms; `zoomOut` restores.

- [ ] **Step 2: Sweep**

```bash
grep -rn "sidebarLeft\|SidebarLeft" --include="*.qml" --include="*.js" ~/.config/quickshell/ii | grep -v ".git"
grep -rniE "translator|aichat|\bAi\b" --include="*.qml" ~/.config/quickshell/ii | grep -v Translation | grep -v ".git"
grep -rn "sidebarLeft" ~/.config/hypr/hyprland/
hyprctl globalshortcuts | grep -i "sidebarLeft"
```
Expected: all empty. Fix any stragglers. Also grep `hyprctl keyword` across ii — fix any other instance with the eval pattern above.

- [ ] **Step 3: Full manual pass**

Restart clean: `qs kill -c ii; qs -c ii -d`. Walk: SUPER+A (top menu, 3 tabs all functional), SUPER+N (right sidebar full walk), notifications arrive, media popup still works standalone, lock still works (SUPER+L), game mode toggle works.

- [ ] **Step 4: Final commit**

```bash
git add -A && git commit -m "chore: cleanup dead references and fix zoom under lua config"
```

---

## Self-review notes

- Spec coverage: removals (Tasks 1-2), top menu + tabs (3-5, 7-8), right sidebar (9), Claude stats incl. error handling (6-7), system stats (8), testing (per-task + Task 10), rollback (Task 0 + tarball). Settings-UI removal covered in Task 2 Step 3.
- Deliberate deviation from spec: pomodoro/calendar/todo presented side-by-side in Planner (wide panel) instead of tabbed — noted in Task 4.
- Tasks 4, 8, 9 depend on reading current file state before editing; instructions pin intent and verification, not blind line edits, because these files were not fully read at plan time. Executors MUST read the named files first.
- ccusage JSON schema verified at execution time (Task 7 Step 2) — field names in code are the current documented shape and may need adaptation.
