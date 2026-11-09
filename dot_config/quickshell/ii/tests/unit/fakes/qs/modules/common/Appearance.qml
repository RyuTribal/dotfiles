pragma Singleton

import QtQuick

// Stands in for modules/common/Appearance.qml in tst_spin_box.qml, tst_backend_spin_box.qml and
// tst_sweep_panel.qml: the colors, sizes and the color animation that StyledSpinBox and
// StyledTextInput read, the colError and size that BackendSpinBox's error row reads, and every key
// SweepPanel.qml reads. tst_backend_spin_box.qml also loads ConfigSpinBox, StyledText and
// OptionalMaterialSymbol over it; it checks text, not colors, so the keys only those three read are
// not here. tst_wallpapers.qml loads WallpaperTile.qml and ThemeKnobControl.qml over it: the tile's
// colours and sizes are here, with each highlight colour distinct so the test can tell them apart.
// tst_claude_panel.qml loads ClaudePanel.qml over it, which adds colOutlineVariant.
// tst_claude_indicator.qml loads ClaudeIndicator.qml and the real StyledToolTip.qml over it, which add
// barHeight, colOnSurfaceVariant, the tooltip colours and the number animation.
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
            colOnPrimary: "#002f6a",
            colOnSecondaryContainer: "#f0f0f0",
            colOnSurfaceVariant: "#b2cad3",
            colOnTooltip: "#ffffff",
            colTooltip: "#3c4043",
            colOutlineVariant: "#344a51",
            colPrimary: "#a0c0ff",
            colPrimaryContainer: "#284777",
            colSecondaryContainer: "#505050",
            colShadow: "#80000000",
            colSubtext: "#a0a0a0"
        })
    property var sizes: ({
            barHeight: 40,
            elevationMargin: 10,
            wallpaperSelectorItemMargins: 8,
            wallpaperSelectorItemPadding: 6
        })
    // Every role key the real Appearance declares, since tst_theme.qml's Theme refuses a file with a
    // role Appearance lacks; the keys no other test reads hold cozy's dark values.
    property var m3colors: ({
            m3accent: "#ffb4ab",
            m3background: "#091519",
            m3error: "#ffb4ab",
            m3errorContainer: "#93000a",
            m3inverseOnSurface: "#273236",
            m3inversePrimary: "#006c50",
            m3inverseSurface: "#d8e5e9",
            m3onAccent: "#690005",
            m3onBackground: "#d8e5e9",
            m3onError: "#690005",
            m3onErrorContainer: "#ffdad6",
            m3onPrimary: "#003828",
            m3onPrimaryContainer: "#d8e2ff",
            m3onPrimaryFixed: "#002116",
            m3onPrimaryFixedVariant: "#00513b",
            m3onSecondary: "#003828",
            m3onSecondaryContainer: "#ffffff",
            m3onSecondaryFixed: "#002116",
            m3onSecondaryFixedVariant: "#00513b",
            m3onSurface: "#d8e5e9",
            m3onSurfaceVariant: "#b2cad3",
            m3onTertiary: "#003641",
            m3onTertiaryContainer: "#b1ecff",
            m3onTertiaryFixed: "#001f27",
            m3onTertiaryFixedVariant: "#004e5e",
            m3outline: "#7d949d",
            m3outlineVariant: "#344a51",
            m3primary: "#a0c0ff",
            m3primaryContainer: "#284777",
            m3primaryFixed: "#85f8cc",
            m3primaryFixedDim: "#67dbb0",
            m3scrim: "#000000",
            m3screenCorner: "#000000",
            m3secondary: "#8bd6b6",
            m3secondaryContainer: "#3e4759",
            m3secondaryFixed: "#a6f2d1",
            m3secondaryFixedDim: "#8bd6b6",
            m3shadow: "#000000",
            m3surface: "#091519",
            m3surfaceBright: "#2f3b3f",
            m3surfaceContainer: "#162125",
            m3surfaceContainerHigh: "#202c30",
            m3surfaceContainerHighest: "#2b373a",
            m3surfaceContainerLow: "#121d21",
            m3surfaceContainerLowest: "#051013",
            m3surfaceDim: "#091519",
            m3surfaceTint: "#67dbb0",
            m3surfaceVariant: "#344a51",
            m3tertiary: "#86d1e9",
            m3tertiaryContainer: "#004e5e",
            m3tertiaryFixed: "#b1ecff",
            m3tertiaryFixedDim: "#86d1e9"
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
                larger: 19,
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
    property Component numberAnimation: Component {
        NumberAnimation {
            duration: 0
        }
    }
    property var animation: ({
            elementMoveFast: {
                colorAnimation: colorAnimation,
                numberAnimation: numberAnimation
            }
        })
}
