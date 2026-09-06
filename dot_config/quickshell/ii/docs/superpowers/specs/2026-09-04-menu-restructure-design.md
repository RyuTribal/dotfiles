# Menu Restructure: Remove Left Sidebar, Add Top Menu, Reshape Right Sidebar

Date: 2026-09-04
Status: Approved direction, pending spec review

## Goal

Replace the current two-sidebar layout of the illogical-impulse quickshell config
with a two-surface layout: a tabbed top menu (new) and a slimmed right sidebar.
Remove the AI chat and translator features entirely. Add a Claude Code usage
statistics view.

## Non-goals

- No visual redesign of retained widgets. The media player, calendar, todo,
  pomodoro, notification list, and quick toggles keep their current styling.
  The media player in particular must look the same as the current
  `mediaControls` popup.
- No changes to the bar layout beyond removing the left-sidebar button and
  adding the top-menu trigger.
- No multi-monitor-specific behavior changes.
- No upstream compatibility: this config diverges from end-4/dots-hyprland
  from this change onward.

## Removals

Delete entirely:

- `modules/sidebarLeft/` (AI chat, translator, and their support files)
- `services/Ai.qml` and `services/ai/`
- Any translator-feature service files (note: `Translation.qml` at the config
  root is UI-language i18n and stays)
- `modules/bar/LeftSidebarButton.qml` and its use in `BarContent.qml` /
  `VerticalBarContent.qml`
- `sidebarLeft` references in `GlobalStates.qml`, `shell.qml`,
  `Background.qml`, `ScreenCorners.qml`
- Hyprland keybinds and quickshell global shortcuts for the left sidebar
  (`quickshell:sidebarLeftToggle`, `quickshell:sidebarLeftOpen`,
  `quickshell:sidebarLeftClose`, `quickshell:sidebarLeftToggleDetach`) in
  `~/.config/hypr/hyprland/keybinds.lua`
- Config options that only serve the removed features (for example
  `policies.ai`, AI model settings, translator settings) in the settings
  schema and `settings.qml` UI

## Top menu (new module: `modules/topMenu/`)

A top-center panel that opens below the bar. Follows the tabbed dashboard
pattern used by caelestia, noctalia, and DankMaterialShell. Reuses the left
sidebar's interaction conventions (keyboard tab cycling, esc to close) since
that code already solves them.

Tabs:

1. **Dashboard** — the existing media player component embedded with its
   current styling, plus a glance row: next todo item, pomodoro state,
   Claude 5-hour window gauge.
2. **Planner** — calendar, todo, and pomodoro widgets moved from the right
   sidebar's bottom widget group.
3. **Stats** — system monitoring (CPU load, RAM, temperatures, per-mount disk
   space, updater moved from right sidebar) and the full Claude usage view.

Trigger: click on the bar clock and a new Hyprland keybind. Registered as
quickshell global shortcuts (`quickshell:topMenuToggle`, `quickshell:topMenuOpen`,
`quickshell:topMenuClose`) bound in `keybinds.lua`, replacing the left
sidebar's binds.

## Right sidebar (reshape `modules/sidebarRight/`)

Top to bottom:

1. Quick toggles grid (existing)
2. Volume and brightness sliders; the volume slider has a drill-in arrow that
   opens the volume mixer in place
3. Notification list, filling remaining space
4. Bottom row: session/power buttons (existing pattern)

Wifi and bluetooth device lists become drill-in pages opened from their
toggles (GNOME 43+ submenu style), replacing the sidebar content until backed
out. Calendar, todo, pomodoro, and updater move out (to the top menu).

## Claude usage statistics

New service `services/ClaudeUsage.qml` exposing properties for the four
metric groups. Data sources:

- **Live rate-limit window** (5-hour and weekly): Claude Code statusline JSON.
  A small script (`~/.config/claude-widget/statusline-tee.sh`, wired into
  `~/.claude/settings.json` statusline config) copies the statusline stdin
  JSON to `~/.cache/claude-widget/status.json`. The service watches that file.
  Fields used: `rate_limits.five_hour.used_percentage`, `.resets_at`,
  `rate_limits.seven_day.*`, `cost.total_cost_usd`, `context_window.used_percentage`.
- **Daily tokens and cost, weekly/monthly trend, per-project breakdown:**
  `bunx ccusage daily --json`, `blocks --json`, `daily --instances --json`,
  polled every 120 seconds only while the Stats tab is visible.
- Cost is labeled "API-equivalent value" (subscription usage is not billed
  per token).
- If the statusline file is missing or stale, the widget shows the ccusage
  block estimate and marks it approximate.

## System stats

Reuse `services/ResourceUsage.qml` and `services/SystemInfo.qml` where they
already expose the needed data; extend for temperatures (hwmon via
`/sys/class/hwmon`) and per-mount disk usage (`df` polling) only if missing.

## Error handling

- ccusage absent or fails: Claude panel shows a hint with the install/run
  command instead of numbers.
- statusline JSON stale (older than 10 minutes): live gauge greyed out with
  "no active session" note.
- All external process calls guarded by availability checks; no crashes when
  offline.

## Testing

- `qs -c ii` reload with no QML errors in `qs -c ii log`
- Manual: open/close each surface via keybind and bar click; verify tab
  cycling; verify media player renders identically; verify wifi/bluetooth
  drill-ins; verify notifications and toggles unaffected
- Claude stats: compare widget numbers against `bunx ccusage daily` output
  and the statusline JSON directly
- Verify removed keybinds are gone from `hyprctl binds` and no dangling
  `quickshell:sidebarLeft*` global shortcuts remain
- Full removal check: `grep -ri "sidebarLeft\|translator" --include="*.qml"`
  returns nothing (excluding `Translation.qml` i18n)

## Rollback

Backup tarball: `~/config-backup-menus-20260904-191210.tar.gz` (covers
`~/.config/quickshell/ii` and `~/.config/hypr`). chezmoi source state is a
second recovery path.
