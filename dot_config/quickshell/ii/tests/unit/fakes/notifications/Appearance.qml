pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_notification_list.qml: the keys
// NotificationListView, StyledListView, NotificationGroup, DragManager, StyledScrollBar and StyledText
// read. The animations keep the real durations and curves (elementMove 500 ms, elementMoveFast
// 200 ms), since the test is about what the list does while they run.
QtObject {
    id: root

    property real backgroundTransparency: 0
    property var colors: ({
            colLayer2: "#202020",
            colOnLayer2: "#e0e0e0",
            colOnSurfaceVariant: "#b2cad3",
            colSubtext: "#a0a0a0"
        })
    property var rounding: ({
            normal: 17
        })
    property var font: ({
            family: {
                main: "sans-serif"
            },
            pixelSize: {
                small: 15,
                smaller: 14
            }
        })
    property var m3colors: ({
            m3onBackground: "#d8e5e9",
            m3primary: "#a0c0ff"
        })
    // The real curves: expressiveFastSpatial here, and below elementMove's expressiveDefaultSpatial,
    // elementMoveFast's expressiveEffects and scroll's standardDecel.
    property QtObject animationCurves: QtObject {
        readonly property list<real> expressiveFastSpatial: [0.42, 1.67, 0.21, 0.90, 1, 1]
    }
    property QtObject animation: QtObject {
        property QtObject elementMove: QtObject {
            property int duration: 500
            property int type: Easing.BezierSpline
            property list<real> bezierCurve: [0.38, 1.21, 0.22, 1.00, 1, 1]
            property Component numberAnimation: Component {
                NumberAnimation {
                    duration: root.animation.elementMove.duration
                    easing.type: root.animation.elementMove.type
                    easing.bezierCurve: root.animation.elementMove.bezierCurve
                }
            }
        }
        property QtObject elementMoveFast: QtObject {
            property int duration: 200
            property int type: Easing.BezierSpline
            property list<real> bezierCurve: [0.34, 0.80, 0.34, 1.00, 1, 1]
            property Component numberAnimation: Component {
                NumberAnimation {
                    duration: root.animation.elementMoveFast.duration
                    easing.type: root.animation.elementMoveFast.type
                    easing.bezierCurve: root.animation.elementMoveFast.bezierCurve
                }
            }
        }
        property QtObject scroll: QtObject {
            property int duration: 200
            property int type: Easing.BezierSpline
            property list<real> bezierCurve: [0, 0, 0, 1, 1, 1]
        }
    }
}
