#!/usr/bin/env bash
# Runs the axon client's QML tests: bash tests/unit/backend_tests.sh
#
# The name must not start with run_: chezmoi runs a run_ file as a script on every apply instead of
# deploying it.
#
# tst_backend.qml needs nothing. tst_backend_connection.qml loads services/Backend.qml in place
# against the stand-in Quickshell modules in fakes/. tst_backend_behaviour.qml needs BackendTopic.qml
# and BackendList.qml to see a stand-in Backend singleton. They name it as a sibling, which Qt only
# resolves through a qmldir in their own directory: qs writes one for every directory it loads, and
# qmltestrunner does not. So this copies the two files and backend.js next to fakes/Backend.qml in a
# temporary module, ShellUnderTest, each run. tst_system_clients.qml loads SystemInfo.qml and
# DateTime.qml from the same module; DateTime's Config comes from fakes/qs/modules/common.
# tst_config_error_toast.qml loads ConfigErrorToast.qml from the same module, with the same Config.
# tst_favicon_cache.qml loads FaviconCache.qml from it too, over fakes/Quickshell/Io/Process.qml.
# tst_resource_usage.qml loads ResourceUsage.qml from the same module.
# tst_sweep_service.qml loads Sweep.qml from it as SweepService, a copy without its `pragma Singleton`
# line, so each test makes its own instance; sweep.js sits beside it. tst_sweep.qml needs nothing.
# tst_nerd_glyphs.qml loads NerdGlyphs.qml from it as NerdGlyphsService in the same way, over
# fakes/Quickshell/Io/Process.qml and StdioCollector.qml, with the real Fuzzy and Levendist of the
# functions module below.
# tst_notifications_service.qml loads Notifications.qml as NotificationsService the same way, beside
# notifications.js, over fakes/qs/GlobalStates.qml and the iconPath of fakes/Quickshell/Quickshell.qml.
# tst_notifications.qml tests notifications.js by its relative path and needs nothing.
# tst_claude_usage.qml loads ClaudeUsage.qml as ClaudeUsageService the same way, beside claude.js and fakes/Translation.qml.
# tst_config.qml tests the real modules/common/Config.qml, so it gets its own module, ConfigUnderTest:
# Config.qml sits there beside fakes/Directories.qml, over the stand-in FileView of fakes/Quickshell/Io.

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
services="$here/../../services"
imports="$(mktemp -d)"
trap 'rm -rf "$imports"' EXIT

module="$imports/ShellUnderTest"
mkdir "$module"
cp "$services/BackendTopic.qml" "$services/BackendList.qml" "$services/SystemInfo.qml" "$services/DateTime.qml" \
    "$services/FaviconCache.qml" "$services/ResourceUsage.qml" "$services/backend.js" \
    "$services/sweep.js" "$services/claude.js" "$services/notifications.js" "$here/../../ConfigErrorToast.qml" "$module/"
cp "$here/fakes/Backend.qml" "$here/fakes/Translation.qml" "$module/"
grep -v '^pragma Singleton$' "$services/Sweep.qml" > "$module/SweepService.qml"
grep -v '^pragma Singleton$' "$services/Notifications.qml" > "$module/NotificationsService.qml"
grep -v '^pragma Singleton$' "$services/NerdGlyphs.qml" > "$module/NerdGlyphsService.qml"
grep -v '^pragma Singleton$' "$services/ClaudeUsage.qml" > "$module/ClaudeUsageService.qml"
cat > "$module/qmldir" <<'EOF'
module ShellUnderTest
singleton Backend 1.0 Backend.qml
BackendTopic 1.0 BackendTopic.qml
BackendList 1.0 BackendList.qml
ConfigErrorToast 1.0 ConfigErrorToast.qml
singleton SystemInfo 1.0 SystemInfo.qml
singleton DateTime 1.0 DateTime.qml
singleton FaviconCache 1.0 FaviconCache.qml
singleton ResourceUsage 1.0 ResourceUsage.qml
SweepService 1.0 SweepService.qml
NotificationsService 1.0 NotificationsService.qml
NerdGlyphsService 1.0 NerdGlyphsService.qml
ClaudeUsageService 1.0 ClaudeUsageService.qml
singleton Translation 1.0 Translation.qml
EOF

config_module="$imports/ConfigUnderTest"
mkdir "$config_module"
cp "$here/../../modules/common/Config.qml" "$here/fakes/Directories.qml" "$config_module/"
cat > "$config_module/qmldir" <<'EOF'
module ConfigUnderTest
singleton Config 1.0 Config.qml
singleton Directories 1.0 Directories.qml
EOF

# tst_spin_box.qml loads the real StyledSpinBox.qml and StyledTextInput.qml from SpinBoxUnderTest, beside
# fakes/spinbox/MaterialSymbol.qml, with the real ColorUtils.qml as qs.modules.common.functions and
# Appearance from fakes/qs/modules/common. The real Fuzzy.qml and Levendist.qml join ColorUtils there for
# NerdGlyphsService.
spinbox_module="$imports/SpinBoxUnderTest"
mkdir "$spinbox_module"
cp "$here/../../modules/common/widgets/StyledSpinBox.qml" "$here/../../modules/common/widgets/StyledTextInput.qml" \
    "$here/fakes/spinbox/MaterialSymbol.qml" "$spinbox_module/"
cat > "$spinbox_module/qmldir" <<'EOF'
module SpinBoxUnderTest
StyledSpinBox 1.0 StyledSpinBox.qml
StyledTextInput 1.0 StyledTextInput.qml
MaterialSymbol 1.0 MaterialSymbol.qml
EOF
functions_module="$imports/qs/modules/common/functions"
mkdir -p "$functions_module"
functions="$here/../../modules/common/functions"
cp "$functions/ColorUtils.qml" "$functions/Fuzzy.qml" "$functions/fuzzysort.js" "$functions/Levendist.qml" \
    "$functions/levendist.js" "$functions_module/"
cat > "$functions_module/qmldir" <<'EOF'
module qs.modules.common.functions
singleton ColorUtils 1.0 ColorUtils.qml
singleton Fuzzy 1.0 Fuzzy.qml
singleton Levendist 1.0 Levendist.qml
EOF

# tst_backend_spin_box.qml loads the real modules/settings/BackendSpinBox.qml as qs.modules.settings, beside
# the real FollowedWrites.qml it writes through, under the names it imports: the real ConfigSpinBox.qml and the widgets it is built from as
# qs.modules.common.widgets, beside fakes/spinbox/MaterialSymbol.qml, and the real AxonConfig.qml and
# BackendTopic.qml as qs.services, beside fakes/Backend.qml. Appearance comes from fakes/qs/modules/common
# and ColorUtils from the functions module above.
widgets="$here/../../modules/common/widgets"
widgets_module="$imports/qs/modules/common/widgets"
mkdir "$widgets_module"
cp "$widgets/ConfigSpinBox.qml" "$widgets/StyledSpinBox.qml" "$widgets/StyledTextInput.qml" "$widgets/StyledText.qml" \
    "$widgets/OptionalMaterialSymbol.qml" "$widgets/StyledRectangularShadow.qml" "$widgets/StyledToolTip.qml" \
    "$here/fakes/spinbox/MaterialSymbol.qml" \
    "$here/fakes/knobs/ConfigSwitch.qml" "$here/fakes/knobs/ConfigSelectionArray.qml" \
    "$here/fakes/knobs/MaterialTextField.qml" "$here/fakes/progress/StyledProgressBar.qml" "$widgets_module/"
cat > "$widgets_module/qmldir" <<'EOF'
module qs.modules.common.widgets
ConfigSpinBox 1.0 ConfigSpinBox.qml
StyledSpinBox 1.0 StyledSpinBox.qml
StyledTextInput 1.0 StyledTextInput.qml
StyledText 1.0 StyledText.qml
OptionalMaterialSymbol 1.0 OptionalMaterialSymbol.qml
StyledRectangularShadow 1.0 StyledRectangularShadow.qml
StyledToolTip 1.0 StyledToolTip.qml
MaterialSymbol 1.0 MaterialSymbol.qml
ConfigSwitch 1.0 ConfigSwitch.qml
ConfigSelectionArray 1.0 ConfigSelectionArray.qml
MaterialTextField 1.0 MaterialTextField.qml
StyledProgressBar 1.0 StyledProgressBar.qml
EOF
services_module="$imports/qs/services"
mkdir "$services_module"
cp "$services/AxonConfig.qml" "$services/BackendTopic.qml" "$services/BackendList.qml" "$services/Sweep.qml" \
    "$services/Theme.qml" "$services/Wallpapers.qml" "$services/ClaudeUsage.qml" "$services/backend.js" \
    "$services/sweep.js" "$services/claude.js" "$here/fakes/Backend.qml" "$services_module/"
cat > "$services_module/qmldir" <<'EOF'
module qs.services
singleton Backend 1.0 Backend.qml
singleton AxonConfig 1.0 AxonConfig.qml
singleton ClaudeUsage 1.0 ClaudeUsage.qml
singleton Sweep 1.0 Sweep.qml
singleton Theme 1.0 Theme.qml
singleton Wallpapers 1.0 Wallpapers.qml
BackendTopic 1.0 BackendTopic.qml
BackendList 1.0 BackendList.qml
EOF
settings_module="$imports/qs/modules/settings"
mkdir "$settings_module"
cp "$here/../../modules/settings/BackendSpinBox.qml" "$here/../../modules/settings/ThemeKnobControl.qml" \
    "$here/../../modules/settings/FollowedWrites.qml" "$settings_module/"
cat > "$settings_module/qmldir" <<'EOF'
module qs.modules.settings
BackendSpinBox 1.0 BackendSpinBox.qml
FollowedWrites 1.0 FollowedWrites.qml
ThemeKnobControl 1.0 ThemeKnobControl.qml
EOF

# tst_sweep_panel.qml and tst_sweep_panel_toggle.qml load the real SweepPanel.qml from
# SweepPanelUnderTest. The panel finds the real services/Sweep.qml as the Sweep singleton of the
# qs.services module above, over the same stand-in Backend, and the Quickshell types it is built from in
# fakes/Quickshell.
panel_module="$imports/SweepPanelUnderTest"
mkdir "$panel_module"
cp "$here/../../SweepPanel.qml" "$panel_module/"
cat > "$panel_module/qmldir" <<'EOF'
module SweepPanelUnderTest
SweepPanel 1.0 SweepPanel.qml
EOF

# tst_claude_panel.qml loads the real modules/topMenu/ClaudePanel.qml as qs.modules.topMenu, where its
# "../../services/claude.js" import reaches claude.js in the qs.services module above. The panel reads
# the real services/ClaudeUsage.qml, the ClaudeUsage singleton there, over the stand-in Backend;
# Translation comes from fakes/qs and StyledProgressBar from fakes/progress, in the widgets module above.
# Its FittedText and RowLabel are the real files beside it.
top_menu_module="$imports/qs/modules/topMenu"
mkdir "$top_menu_module"
cp "$here/../../modules/topMenu/ClaudePanel.qml" "$here/../../modules/topMenu/FittedText.qml" \
    "$here/../../modules/topMenu/RowLabel.qml" "$top_menu_module/"
cat > "$top_menu_module/qmldir" <<'EOF'
module qs.modules.topMenu
ClaudePanel 1.0 ClaudePanel.qml
FittedText 1.0 FittedText.qml
RowLabel 1.0 RowLabel.qml
EOF

# tst_claude_indicator.qml loads the real modules/bar/ClaudeIndicator.qml as qs.modules.bar, beside the
# stand-in StyledPopup of fakes/bar, which lays the popup's content out in place. Its claude.js import
# reaches the qs.services module above, as ClaudePanel's does; the real StyledToolTip and StyledText come
# from the widgets module above, MaterialSymbol from fakes/spinbox, GlobalStates from fakes/qs.
bar_module="$imports/qs/modules/bar"
mkdir "$bar_module"
cp "$here/../../modules/bar/ClaudeIndicator.qml" "$here/fakes/bar/StyledPopup.qml" "$bar_module/"
cat > "$bar_module/qmldir" <<'EOF'
module qs.modules.bar
ClaudeIndicator 1.0 ClaudeIndicator.qml
StyledPopup 1.0 StyledPopup.qml
EOF

# tst_theme.qml loads the real services/Theme.qml as the Theme singleton of the qs.services module above,
# over the stand-in Backend, the stand-in FileView of fakes/Quickshell/Io and the Appearance and Directories
# of fakes/qs/modules/common, into which it writes the colours. For the hover colours it also loads the real
# modules/common/Appearance.qml from AppearanceUnderTest, beside fakes/appearance/Config.qml (the keys
# Appearance reads), with the stand-in ColorQuantizer of fakes/Quickshell and the real ColorUtils above.
appearance_module="$imports/AppearanceUnderTest"
mkdir "$appearance_module"
cp "$here/../../modules/common/Appearance.qml" "$here/fakes/appearance/Config.qml" "$appearance_module/"
cat > "$appearance_module/qmldir" <<'EOF'
module AppearanceUnderTest
singleton Appearance 1.0 Appearance.qml
singleton Config 1.0 Config.qml
EOF

# tst_wallpapers.qml loads the real services/Wallpapers.qml as the Wallpapers singleton of the qs.services
# module above, with GlobalStates from fakes/qs, the real modules/settings/ThemeKnobControl.qml from the
# qs.modules.settings module above, over the stand-in ConfigSwitch, ConfigSelectionArray and
# MaterialTextField of fakes/knobs, and the real modules/wallpaperSelector/WallpaperTile.qml from
# qs.modules.wallpaperSelector, with the real StyledRectangularShadow of the widgets module above. It
# imports services/theme.js by relative path.
selector_module="$imports/qs/modules/wallpaperSelector"
mkdir "$selector_module"
cp "$here/../../modules/wallpaperSelector/WallpaperTile.qml" "$selector_module/"
cat > "$selector_module/qmldir" <<'EOF'
module qs.modules.wallpaperSelector
WallpaperTile 1.0 WallpaperTile.qml
EOF

# tst_notification_list.qml loads the real NotificationListView.qml, StyledListView.qml and
# NotificationGroup.qml as qs.modules.common.widgets over the real services/Notifications.qml as
# qs.services, fed `notify` frames through the stand-in Backend. Those names clash with the modules
# above, so it runs over its own import root, which holds every module it needs. Its Appearance and
# Config are fakes/notifications' (the real animation durations), its ScriptModel the keyed one there,
# and the widgets that need Hyprland, Quickshell.Widgets or effects are the stand-ins there; DragManager,
# StyledScrollBar and StyledText are real.
list_root="$imports/notification-list"
mkdir -p "$list_root/Quickshell" "$list_root/qs/modules/common/functions" "$list_root/qs/modules/common/widgets" \
    "$list_root/qs/services"
cp "$here/fakes/Quickshell/Quickshell.qml" "$here/fakes/Quickshell/Singleton.qml" \
    "$here/fakes/notifications/ScriptModel.qml" "$list_root/Quickshell/"
cat > "$list_root/Quickshell/qmldir" <<'EOF'
module Quickshell
singleton Quickshell 1.0 Quickshell.qml
Singleton 1.0 Singleton.qml
ScriptModel 1.0 ScriptModel.qml
EOF
cp "$here/fakes/qs/GlobalStates.qml" "$here/fakes/Translation.qml" "$list_root/qs/"
cat > "$list_root/qs/qmldir" <<'EOF'
module qs
singleton GlobalStates 1.0 GlobalStates.qml
singleton Translation 1.0 Translation.qml
EOF
cp "$here/fakes/notifications/Appearance.qml" "$here/fakes/notifications/Config.qml" "$list_root/qs/modules/common/"
cat > "$list_root/qs/modules/common/qmldir" <<'EOF'
module qs.modules.common
singleton Appearance 1.0 Appearance.qml
singleton Config 1.0 Config.qml
EOF
cp "$functions/ColorUtils.qml" "$list_root/qs/modules/common/functions/"
cat > "$list_root/qs/modules/common/functions/qmldir" <<'EOF'
module qs.modules.common.functions
singleton ColorUtils 1.0 ColorUtils.qml
EOF
cp "$services/Notifications.qml" "$services/notifications.js" "$services/backend.js" "$services/BackendTopic.qml" \
    "$here/fakes/Backend.qml" "$list_root/qs/services/"
cat > "$list_root/qs/services/qmldir" <<'EOF'
module qs.services
singleton Backend 1.0 Backend.qml
singleton Notifications 1.0 Notifications.qml
BackendTopic 1.0 BackendTopic.qml
EOF
cp "$widgets/NotificationListView.qml" "$widgets/StyledListView.qml" "$widgets/NotificationGroup.qml" \
    "$widgets/notification_utils.js" "$widgets/DragManager.qml" "$widgets/StyledScrollBar.qml" "$widgets/StyledText.qml" \
    "$here/fakes/notifications/NotificationItem.qml" "$here/fakes/notifications/NotificationAppIcon.qml" \
    "$here/fakes/notifications/NotificationGroupExpandButton.qml" "$here/fakes/notifications/StyledRectangularShadow.qml" \
    "$here/fakes/notifications/StyledToolTip.qml" "$list_root/qs/modules/common/widgets/"
cat > "$list_root/qs/modules/common/widgets/qmldir" <<'EOF'
module qs.modules.common.widgets
NotificationListView 1.0 NotificationListView.qml
StyledListView 1.0 StyledListView.qml
NotificationGroup 1.0 NotificationGroup.qml
DragManager 1.0 DragManager.qml
StyledScrollBar 1.0 StyledScrollBar.qml
StyledText 1.0 StyledText.qml
NotificationItem 1.0 NotificationItem.qml
NotificationAppIcon 1.0 NotificationAppIcon.qml
NotificationGroupExpandButton 1.0 NotificationGroupExpandButton.qml
StyledRectangularShadow 1.0 StyledRectangularShadow.qml
StyledToolTip 1.0 StyledToolTip.qml
EOF

# ClaudeUsage.qml starts no process and reads no file; only grep's status 1 (no match) passes.
status=0
grep -nE 'Process|FileView|bunx|ccusage|stat ' "$services/ClaudeUsage.qml" || status=$?
if [ "$status" -ne 1 ]; then
    echo "ClaudeUsage.qml: Process/FileView/bunx/ccusage/stat found, or the file is unreadable (grep status $status)" >&2
    exit 1
fi

# Notifications.qml is a thin client of axon: it runs no server, starts no process, reads no file and
# declares no inline component; only grep's status 1 (no match) passes.
status=0
grep -nE 'NotificationServer|Services\.Notifications|canberra|FileView|Process|execDetached|component ' "$services/Notifications.qml" || status=$?
if [ "$status" -ne 1 ]; then
    echo "Notifications.qml: an old-path name was found, or the file is unreadable (grep status $status)" >&2
    exit 1
fi

status=0
for test in tst_backend.qml tst_backend_connection.qml tst_backend_behaviour.qml \
    tst_system_clients.qml tst_config_error_toast.qml tst_config.qml tst_spin_box.qml \
    tst_backend_spin_box.qml tst_favicon_cache.qml tst_resource_usage.qml tst_sweep.qml \
    tst_sweep_service.qml tst_sweep_panel.qml tst_sweep_panel_toggle.qml tst_nerd_glyphs.qml \
    tst_theme.qml tst_wallpapers.qml tst_claude.qml tst_claude_usage.qml tst_claude_panel.qml \
    tst_claude_indicator.qml tst_notifications.qml tst_notifications_service.qml; do
    QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -import "$here/fakes" -import "$imports" \
        -input "$here/$test" || status=1
done
QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -import "$list_root" \
    -input "$here/tst_notification_list.qml" || status=1
exit "$status"
