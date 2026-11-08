pragma Singleton

import QtQuick

// Stands in for modules/common/Directories.qml in tst_favicon_cache.qml, tst_nerd_glyphs.qml and
// tst_theme.qml: only the favicon cache, the config directory and axon's current theme generation.
QtObject {
    property string favicons: "/nonexistent/favicons"
    property string config: "/nonexistent/config"
    property string axonThemeCurrent: "/nonexistent/axon/theme/current"
}
