pragma Singleton

import QtQuick

// Stands in for modules/common/Config.qml beside the real Appearance.qml in tst_theme.qml: the keys
// Appearance reads. The test replaces `options` to switch transparency on and off.
QtObject {
    property var options: ({
            appearance: {
                transparency: {
                    enable: false,
                    automatic: false,
                    backgroundTransparency: 0,
                    contentTransparency: 0
                },
                extraBackgroundTint: false
            },
            bar: {
                cornerStyle: 0,
                verbose: false
            }
        })
}
