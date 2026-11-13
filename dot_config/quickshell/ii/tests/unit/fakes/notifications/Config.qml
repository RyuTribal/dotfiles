pragma Singleton

import QtQuick

// Stands in for modules/common/Config.qml in tst_notification_list.qml: the scrolling keys
// StyledListView reads.
QtObject {
    property var options: ({
            interactions: {
                scrolling: {
                    fasterTouchpadScroll: false,
                    touchpadScrollFactor: 100,
                    mouseScrollFactor: 50,
                    mouseScrollDeltaThreshold: 120
                }
            }
        })
}
