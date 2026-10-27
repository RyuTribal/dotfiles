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
    "$services/backend.js" "$here/../../ConfigErrorToast.qml" "$module/"
cp "$here/fakes/Backend.qml" "$module/"
cat > "$module/qmldir" <<'EOF'
module ShellUnderTest
singleton Backend 1.0 Backend.qml
BackendTopic 1.0 BackendTopic.qml
BackendList 1.0 BackendList.qml
ConfigErrorToast 1.0 ConfigErrorToast.qml
singleton SystemInfo 1.0 SystemInfo.qml
singleton DateTime 1.0 DateTime.qml
EOF

config_module="$imports/ConfigUnderTest"
mkdir "$config_module"
cp "$here/../../modules/common/Config.qml" "$here/fakes/Directories.qml" "$config_module/"
cat > "$config_module/qmldir" <<'EOF'
module ConfigUnderTest
singleton Config 1.0 Config.qml
singleton Directories 1.0 Directories.qml
EOF

status=0
for test in tst_backend.qml tst_backend_connection.qml tst_backend_behaviour.qml tst_system_clients.qml tst_config_error_toast.qml tst_config.qml; do
    QT_QPA_PLATFORM=offscreen /usr/lib/qt6/bin/qmltestrunner -import "$here/fakes" -import "$imports" \
        -input "$here/$test" || status=1
done
exit "$status"
