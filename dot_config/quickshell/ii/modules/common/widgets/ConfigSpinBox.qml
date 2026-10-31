import qs.modules.common.widgets
import qs.modules.common
import QtQuick
import QtQuick.Layouts

RowLayout {
    id: root
    property string text: ""
    property string icon
    property alias value: spinBoxWidget.value
    property alias stepSize: spinBoxWidget.stepSize
    property alias from: spinBoxWidget.from
    property alias to: spinBoxWidget.to
    property alias textFromValue: spinBoxWidget.textFromValue
    // Emitted only when the user changes the value with the buttons, the wheel or the arrow keys, or
    // finishes a typed entry (Return, Enter or leaving the field), never when `value` is set from code
    // or a binding. A typed number is one emission, however many keys it took.
    signal valueModified
    spacing: 10
    Layout.leftMargin: 8
    Layout.rightMargin: 8

    RowLayout {
        spacing: 10
        OptionalMaterialSymbol {
            icon: root.icon
        }
        StyledText {
            id: labelWidget
            Layout.fillWidth: true
            text: root.text
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colOnSecondaryContainer
        }
    }

    StyledSpinBox {
        id: spinBoxWidget
        Layout.fillWidth: false
        value: root.value
        onValueModified: root.valueModified()
    }
}
