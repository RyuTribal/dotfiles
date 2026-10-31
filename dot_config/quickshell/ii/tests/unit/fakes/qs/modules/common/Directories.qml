pragma Singleton

import QtQuick

// Stands in for modules/common/Directories.qml in tst_favicon_cache.qml and tst_nerd_glyphs.qml: only
// the favicon cache and the config directory.
QtObject {
    property string favicons: "/nonexistent/favicons"
    property string config: "/nonexistent/config"
}
