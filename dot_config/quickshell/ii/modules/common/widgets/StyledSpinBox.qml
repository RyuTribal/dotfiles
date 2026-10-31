import qs.modules.common
import qs.modules.common.functions
import QtQuick
import QtQuick.Controls

/**
 * Material 3 styled SpinBox component.
 */
SpinBox {
    id: root

    property real baseHeight: 35
    property real radius: Appearance.rounding.small
    property real innerButtonRadius: Appearance.rounding.unsharpen
    editable: true
    // The field shows this, so a spin box can stand "--" in for a value it does not know yet.
    // Without one of its own it is the plain number: Qt's default groups digits by locale (1,000,
    // not 1000).
    textFromValue: (value, locale) => String(value)
    // A step of SpinBox's own (its buttons, the wheel, the arrow keys) during a typed entry ends
    // the entry and is its one modification, so finishing the entry later modifies nothing more.
    onValueModified: {
        if (labelText.typed)
            labelText.endEntry();
    }

    background: Rectangle {
        color: Appearance.colors.colLayer2
        radius: root.radius
    }

    contentItem: Item {
        implicitHeight: root.baseHeight
        implicitWidth: Math.max(labelText.implicitWidth, 40)

        StyledTextInput {
            id: labelText
            objectName: "valueField" // the tests find the field by this name
            anchors.centerIn: parent
            text: valueText()
            color: Appearance.colors.colOnLayer2
            font.pixelSize: Appearance.font.pixelSize.small
            validator: root.validator
            // True from the first typed key until the entry ends, so leaving the field without
            // typing modifies nothing.
            property bool typed: false
            // The value when the entry started.
            property int valueBeforeEntry: 0
            // Qt emits valueModified only for its own buttons, wheel and arrow keys, not when this field
            // sets `value`. Typing previews each value at once, and the typed entry is one modification
            // when it is finished (Return, Enter or leaving the field), so a typed 500 is one write, not
            // 5, 50 and 500. textEdited fires for user edits only, never when `text` follows `value`.
            // Only a number within from..to is previewed, since SpinBox would clamp any other (with
            // from 100, a typed 2500 would read 100, then 1005, then 10000). While the text is
            // anything else, the value is the one from before the entry, so no number left over
            // from typing or deleting is kept: 3000 backspaced to 30 holds 3000, not 300, and a
            // step starts from 3000.
            onTextEdited: {
                if (!typed) {
                    typed = true;
                    valueBeforeEntry = root.value;
                    // Typing leaves the text binding in place, so the field would show each value
                    // set below. Assigning the text drops the binding; endEntry() puts it back.
                    text = text;
                }
                const typedValue = typedNumber(text);
                const lowest = Math.min(root.from, root.to);
                const highest = Math.max(root.from, root.to);
                const inRange = typedValue >= lowest && typedValue <= highest;
                root.value = inRange ? typedValue : valueBeforeEntry;
            }
            // These finish the entry, not editingFinished: TextInput emits that only for text its
            // validator accepts, and SpinBox's validator refuses a number outside from..to.
            Keys.onReturnPressed: event => {
                finishEntry();
                event.accepted = false; // the field still handles the key
            }
            Keys.onEnterPressed: event => {
                finishEntry();
                event.accepted = false;
            }
            onActiveFocusChanged: {
                if (!activeFocus)
                    finishEntry();
            }

            function valueText() {
                return root.textFromValue(root.value, root.locale);
            }

            function finishEntry() {
                if (!typed)
                    return;
                // The entry ends as the number typed, which SpinBox clamps into from..to (200 with
                // to 154 is 154). Text that is no number, such as an emptied field, ends it as the
                // value from before the entry.
                const typedValue = typedNumber(text);
                root.value = isNaN(typedValue) ? valueBeforeEntry : typedValue;
                endEntry();
                root.valueModified();
            }

            // Ends the entry with no modification of its own. The field shows the value again
            // through a binding: a plain string would stop it following `value`.
            function endEntry() {
                typed = false;
                text = Qt.binding(valueText);
            }

            // The text as a number, read with the spin box's locale, as the field's validator reads
            // it: en_US takes 1,000 as 1000, which parseFloat would read as 1. NaN for text that is
            // no number, such as an emptied field.
            function typedNumber(input) {
                try {
                    return Number.fromLocaleString(root.locale, input);
                } catch (error) {
                    return NaN;
                }
            }
        }
    }

    down.indicator: Rectangle {
        anchors {
            verticalCenter: parent.verticalCenter
            left: parent.left
        }
        implicitHeight: root.baseHeight
        implicitWidth: root.baseHeight
        topLeftRadius: root.radius
        bottomLeftRadius: root.radius
        topRightRadius: root.innerButtonRadius
        bottomRightRadius: root.innerButtonRadius

        color: root.down.pressed ? Appearance.colors.colLayer2Active : 
            root.down.hovered ? Appearance.colors.colLayer2Hover : 
            ColorUtils.transparentize(Appearance.colors.colLayer2)
        Behavior on color {
            animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
        }

        MaterialSymbol {
            anchors.centerIn: parent
            text: "remove"
            iconSize: 20
            color: Appearance.colors.colOnLayer2
        }
    }

    up.indicator: Rectangle {
        anchors {
            verticalCenter: parent.verticalCenter
            right: parent.right
        }
        implicitHeight: root.baseHeight
        implicitWidth: root.baseHeight
        topRightRadius: root.radius
        bottomRightRadius: root.radius
        topLeftRadius: root.innerButtonRadius
        bottomLeftRadius: root.innerButtonRadius

        color: root.up.pressed ? Appearance.colors.colLayer2Active : 
            root.up.hovered ? Appearance.colors.colLayer2Hover : 
            ColorUtils.transparentize(Appearance.colors.colLayer2)
        Behavior on color {
            animation: Appearance.animation.elementMoveFast.colorAnimation.createObject(this)
        }

        MaterialSymbol {
            anchors.centerIn: parent
            text: "add"
            iconSize: 20
            color: Appearance.colors.colOnLayer2
        }
    }
}
