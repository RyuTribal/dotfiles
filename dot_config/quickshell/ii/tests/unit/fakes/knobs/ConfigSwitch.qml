import QtQuick
import QtQuick.Controls

// Stands in for modules/common/widgets/ConfigSwitch.qml in tst_wallpapers.qml, which loads
// ThemeKnobControl.qml: a button with the same API, whose own click handler flips `checked` first,
// as the real one's does, so a control built on it must bind `checked` again after a click.
Button {
    id: root
    property string buttonIcon
    onClicked: checked = !checked
}
