import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts

// A short text beside a FittedText in a row: its own width while there is
// room, never more, elided before it can push the row wider.
StyledText {
    id: label

    Layout.fillWidth: true
    Layout.maximumWidth: label.implicitWidth
    elide: Text.ElideRight
}
