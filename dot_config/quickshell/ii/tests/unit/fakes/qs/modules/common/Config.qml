pragma Singleton

import QtQuick

// Stands in for modules/common/Config.qml in tst_system_clients.qml: DateTime reads only
// Config.options.time, and empty format keys make it use its default formats. tst_config_error_toast.qml
// sets parseError. tst_favicon_cache.qml reads the user agent. tst_audio_service.qml's Audio reads the
// OSD timeout, the live value.
QtObject {
    property string parseError: ""
    property var options: ({
            time: ({}),
            networking: ({
                    userAgent: "test-agent"
                }),
            osd: ({
                    timeout: 1000
                })
        })
}
