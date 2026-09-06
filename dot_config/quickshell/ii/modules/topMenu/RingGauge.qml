import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Shapes
import QtQuick.Layouts

// Hollow ring gauge for SystemPanel's 2x2 metric grid. Unlike
// modules/common/widgets/ClippedFilledCircularProgress (a filled pie wedge —
// its arc closes back to the center via an explicit PathLine, which is the
// right look for the bar's small circles but not for a "hollow ring with a
// label in the middle"), this draws just the arc's stroke with
// moveToStart left at its default (true), so nothing connects the arc back
// to center and the middle stays empty for the label/value text.
Item {
    id: root

    property real value: 0 // 0..1, already clamped by the caller
    property string label: "" // metric name, e.g. "RAM" or "/"
    property string valueText: "" // e.g. "60%" or "71°C"
    property bool warning: false // switches the arc + value to the error color
    property int diameter: 100
    property int lineWidth: 8

    implicitWidth: diameter
    implicitHeight: diameter

    readonly property color trackColor: Appearance.colors.colSecondaryContainer
    readonly property color arcColor: root.warning ? Appearance.colors.colError : Appearance.colors.colOnSecondaryContainer

    property real degree: Math.max(0, Math.min(1, root.value)) * 360
    Behavior on degree {
        NumberAnimation {
            duration: Appearance.animation.elementMoveEnter.duration
            easing.type: Appearance.animation.elementMoveEnter.type
        }
    }

    // Dim full-circle track underneath, so the ring reads as a gauge even
    // at 0%.
    Shape {
        anchors.fill: parent
        preferredRendererType: Shape.CurveRenderer

        ShapePath {
            strokeColor: root.trackColor
            strokeWidth: root.lineWidth
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap

            startX: root.diameter / 2 + (root.diameter / 2 - root.lineWidth / 2)
            startY: root.diameter / 2

            PathAngleArc {
                centerX: root.diameter / 2
                centerY: root.diameter / 2
                radiusX: root.diameter / 2 - root.lineWidth / 2
                radiusY: root.diameter / 2 - root.lineWidth / 2
                startAngle: 0
                sweepAngle: 359.999
            }
        }
    }

    // Value arc, starting at the top and sweeping clockwise — same
    // convention as ClippedFilledCircularProgress's startAngle: -90.
    Shape {
        anchors.fill: parent
        preferredRendererType: Shape.CurveRenderer

        ShapePath {
            strokeColor: root.arcColor
            strokeWidth: root.lineWidth
            fillColor: "transparent"
            capStyle: ShapePath.RoundCap

            startX: root.diameter / 2
            startY: root.lineWidth / 2

            PathAngleArc {
                centerX: root.diameter / 2
                centerY: root.diameter / 2
                radiusX: root.diameter / 2 - root.lineWidth / 2
                radiusY: root.diameter / 2 - root.lineWidth / 2
                startAngle: -90
                sweepAngle: root.degree
            }
        }
    }

    ColumnLayout {
        anchors.centerIn: parent
        spacing: 0

        StyledText {
            Layout.alignment: Qt.AlignHCenter
            text: root.label
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
            font.pixelSize: Appearance.font.pixelSize.smaller
            color: Appearance.colors.colSubtext
        }
        StyledText {
            Layout.alignment: Qt.AlignHCenter
            text: root.valueText
            font.pixelSize: Appearance.font.pixelSize.normal
            font.weight: Font.Medium
            color: root.warning ? Appearance.colors.colError : Appearance.colors.colOnLayer1
        }
    }
}
