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
    "$services/sweep.js" "$here/../../ConfigErrorToast.qml" "$module/"
cp "$here/fakes/Backend.qml" "$module/"
grep -v '^pragma Singleton$' "$services/Sweep.qml" > "$module/SweepService.qml"
grep -v '^pragma Singleton$' "$services/NerdGlyphs.qml" > "$module/NerdGlyphsService.qml"
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
NerdGlyphsService 1.0 NerdGlyphsService.qml
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

# tst_backend_spin_box.qml loads the real modules/settings/BackendSpinBox.qml as qs.modules.settings, under
# the names it imports: the real ConfigSpinBox.qml and the widgets it is built from as
# qs.modules.common.widgets, beside fakes/spinbox/MaterialSymbol.qml, and the real AxonConfig.qml and
# BackendTopic.qml as qs.services, beside fakes/Backend.qml. Appearance comes from fakes/qs/modules/common
# and ColorUtils from the functions module above.
widgets="$here/../../modules/common/widgets"
widgets_module="$imports/qs/modules/common/widgets"
mkdir "$widgets_module"
cp "$widgets/ConfigSpinBox.qml" "$widgets/StyledSpinBox.qml" "$widgets/StyledTextInput.qml" "$widgets/StyledText.qml" \
    "$widgets/OptionalMaterialSymbol.qml" "$here/fakes/spinbox/MaterialSymbol.qml" "$widgets_module/"
cat > "$widgets_module/qmldir" <<'EOF'
module qs.modules.common.widgets
ConfigSpinBox 1.0 ConfigSpinBox.qml
StyledSpinBox 1.0 StyledSpinBox.qml
StyledTextInput 1.0 StyledTextInput.qml
StyledText 1.0 StyledText.qml
OptionalMaterialSymbol 1.0 OptionalMaterialSymbol.qml
MaterialSymbol 1.0 MaterialSymbol.qml
EOF
services_module="$imports/qs/services"
mkdir "$services_module"
cp "$services/AxonConfig.qml" "$services/BackendTopic.qml" "$services/BackendList.qml" "$services/Sweep.qml" \
    "$services/backend.js" "$services/sweep.js" "$here/fakes/Backend.qml" "$services_module/"
cat > "$services_module/qmldir" <<'EOF'
module qs.services
singleton Backend 1.0 Backend.qml
singleton AxonConfig 1.0 AxonConfig.qml
singleton Sweep 1.0 Sweep.qml
BackendTopic 1.0 BackendTopic.qml
BackendList 1.0 BackendList.qml
EOF
settings_module="$imports/qs/modules/settings"
mkdir "$settings_module"
cp "$here/../../modules/settings/BackendSpinBox.qml" "$settings_module/"
cat > "$settings_module/qmldir" <<'EOF'
module qs.modules.settings
BackendSpinBox 1.0 BackendSpinBox.qml
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

status=0
for test in tst_backend.qml tst_backend_connection.qml tst_backend_behaviour.qml \
    tst_system_clients.qml tst_config_error_toast.qml tst_config.qml tst_spin_box.qml \
    tst_backend_spin_box.qml tst_favicon_cache.qml tst_resource_usage.qml tst_sweep.qml \
    tst_sweep_service.qml tst_sweep_panel.qml tst_sweep_panel_toggle.qml tst_nerd_glyphs.qml; do
    QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -import "$here/fakes" -import "$imports" \
        -input "$here/$test" || status=1
done
exit "$status"
