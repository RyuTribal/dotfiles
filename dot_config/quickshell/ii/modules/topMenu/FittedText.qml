import qs.modules.common.widgets
import QtQuick
import QtQuick.Layouts

// A text that takes its width from its layout, never from its text: it fills
// the row and elides (or wraps, when given a wrapMode) what does not fit. See
// ClaudePanel.qml for why its card needs this.
StyledText {
    Layout.fillWidth: true
    Layout.preferredWidth: 0
    elide: Text.ElideRight
}
