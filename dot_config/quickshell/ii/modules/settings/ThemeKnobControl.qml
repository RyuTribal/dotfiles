import qs.services
import qs.modules.common
import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts

/**
 * The settings control of one knob of the active theme (section 40.9), built from its row in the
 * `theme` topic: a switch for `bool`, a spin box with the row's `min` and `max` for `int`, a selector
 * over `values` for `enum`, and a text field for `color` (empty or `#rrggbb`) and `float` (a number
 * within `min`..`max`; Qt's SpinBox holds whole numbers only).
 *
 * It writes `Theme.setKnob` on user interaction only (50.1), never when a snapshot moves the value.
 * The write logic is BackendSpinBox's, FollowedWrites, keyed to the row's `value`: a written value
 * stays shown until the topic shows it, a refused one snaps back at once with the reply's error
 * under the control, and an entry that ends as the value it started from writes nothing and shows
 * the topic again. The row's warning (`knob_value_invalid`, `orphan_knobs`) shows under it too.
 */
ColumnLayout {
    id: root

    required property string knobId
    readonly property var row: Theme.knobs.find(knob => knob.id === knobId) ?? null
    readonly property string kind: row?.type ?? ""
    readonly property string label: row?.label ?? knobId
    readonly property var shown: writes.shown // the value the control shows

    visible: row !== null
    enabled: AxonConfig.writable
    spacing: 4

    // A value the control already shows is no change and writes nothing.
    function write(value) {
        if (value !== shown)
            writes.write(value);
    }

    FollowedWrites {
        id: writes
        topicValue: root.row?.value
        send: (value, done) => Theme.setKnob(root.knobId, value, done)
    }

    StyledText {
        visible: root.kind === "enum" || root.kind === "color" || root.kind === "float"
        Layout.leftMargin: 8
        text: root.label
        font.pixelSize: Appearance.font.pixelSize.small
        color: Appearance.colors.colOnSecondaryContainer
    }

    Loader {
        Layout.fillWidth: true
        sourceComponent: ({
                bool: switchControl,
                int: spinControl,
                float: textControl,
                enum: enumControl,
                color: textControl
            })[root.kind] ?? null
    }

    StyledText {
        objectName: "warningRow" // the tests find the rows and controls by these names
        visible: text !== ""
        Layout.leftMargin: 8
        text: Theme.knobWarning(root.knobId)
        font.pixelSize: Appearance.font.pixelSize.small
        color: Appearance.colors.colSubtext
    }

    StyledText {
        objectName: "errorRow"
        visible: text !== ""
        Layout.leftMargin: 8
        text: writes.errorText
        font.pixelSize: Appearance.font.pixelSize.small
        color: Appearance.colors.colError
    }

    Component {
        id: switchControl

        ConfigSwitch {
            objectName: "switch"
            text: root.label
            checked: root.shown === true
            // ConfigSwitch flips `checked` on a click, which drops the binding: bind it again.
            onClicked: {
                root.write(!(root.shown === true));
                checked = Qt.binding(() => root.shown === true);
            }
        }
    }

    Component {
        id: spinControl

        ConfigSpinBox {
            objectName: "spinBox"
            text: root.label
            from: root.row?.min ?? 0
            to: root.row?.max ?? 0
            stepSize: 1
            value: root.shown
            // Typing and a write both drop the value's binding; while a write waits, `shown` is it.
            onValueModified: {
                root.write(value);
                value = Qt.binding(() => root.shown);
            }
            onEntryEndedUnchanged: value = Qt.binding(() => root.shown)
        }
    }

    Component {
        id: enumControl

        ConfigSelectionArray {
            objectName: "enumSelector"
            currentValue: root.shown
            options: (root.row?.values ?? []).map(value => ({
                        displayName: value.split("-").map(word => word.charAt(0).toUpperCase() + word.slice(1)).join(" "),
                        value: value
                    }))
            onSelected: newValue => root.write(newValue)
        }
    }

    Component {
        id: textControl

        MaterialTextField {
            id: field
            objectName: "textField"
            placeholderText: root.kind === "color" ? "#rrggbb" : ""
            text: String(root.shown ?? "")
            validator: root.kind === "color" ? colorValidator : numberValidator
            // Only text the validator accepts finishes an entry; leaving the field with any other text
            // shows the value again.
            onEditingFinished: {
                root.write(root.kind === "float" ? Number(text) : text);
                field.text = Qt.binding(() => String(root.shown ?? ""));
            }
            onActiveFocusChanged: {
                if (!activeFocus)
                    field.text = Qt.binding(() => String(root.shown ?? ""));
            }

            RegularExpressionValidator {
                id: colorValidator
                regularExpression: /^(#[0-9A-Fa-f]{6})?$/
            }
            DoubleValidator {
                id: numberValidator
                bottom: root.row?.min ?? 0
                top: root.row?.max ?? 0
                locale: "C"
                notation: DoubleValidator.StandardNotation
            }
        }
    }
}
