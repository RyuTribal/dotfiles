pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_spin_box.qml: the colors, sizes and the color
// animation that StyledSpinBox and StyledTextInput read.
QtObject {
    property var colors: ({
            colLayer2: "#202020",
            colLayer2Active: "#303030",
            colLayer2Hover: "#404040",
            colOnLayer1: "#e0e0e0",
            colOnLayer2: "#e0e0e0",
            colSecondaryContainer: "#505050"
        })
    property var m3colors: ({
            m3onSecondaryContainer: "#ffffff"
        })
    property var rounding: ({
            small: 4,
            unsharpen: 2
        })
    property var font: ({
            family: {
                main: "sans-serif"
            },
            pixelSize: {
                small: 15
            }
        })
    property Component colorAnimation: Component {
        ColorAnimation {
            duration: 0
        }
    }
    property var animation: ({
            elementMoveFast: {
                colorAnimation: colorAnimation
            }
        })
}
