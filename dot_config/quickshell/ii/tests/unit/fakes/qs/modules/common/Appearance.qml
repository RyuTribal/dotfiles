pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_spin_box.qml, tst_backend_spin_box.qml and
// tst_sweep_panel.qml: the colors, sizes and the color animation that StyledSpinBox and
// StyledTextInput read, the colError and size that BackendSpinBox's error row reads, and every key
// SweepPanel.qml reads. tst_backend_spin_box.qml also loads ConfigSpinBox, StyledText and
// OptionalMaterialSymbol over it; it checks text, not colors, so the keys only those three read are
// not here.
QtObject {
    property var colors: ({
            colError: "#ffb4ab",
            colLayer0: "#101010",
            colLayer1: "#181818",
            colLayer1Hover: "#282828",
            colLayer2: "#202020",
            colLayer2Active: "#303030",
            colLayer2Hover: "#404040",
            colOnLayer0: "#e0e0e0",
            colOnLayer1: "#e0e0e0",
            colOnLayer2: "#e0e0e0",
            colPrimary: "#a0c0ff",
            colSecondaryContainer: "#505050",
            colSubtext: "#a0a0a0"
        })
    property var m3colors: ({
            m3error: "#ffb4ab",
            m3errorContainer: "#93000a",
            m3onErrorContainer: "#ffdad6",
            m3onPrimaryContainer: "#d8e2ff",
            m3onSecondaryContainer: "#ffffff",
            m3primary: "#a0c0ff",
            m3primaryContainer: "#284777",
            m3scrim: "#000000",
            m3secondaryContainer: "#3e4759"
        })
    property var rounding: ({
            full: 9999,
            normal: 17,
            small: 4,
            unsharpen: 2,
            unsharpenmore: 6,
            verysmall: 8
        })
    property var font: ({
            family: {
                main: "sans-serif",
                monospace: "monospace",
                title: "sans-serif"
            },
            pixelSize: {
                huge: 22,
                normal: 16,
                small: 15,
                smaller: 14,
                smallest: 10
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
