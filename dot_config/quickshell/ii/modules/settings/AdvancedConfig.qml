import QtQuick
import QtQuick.Layouts
import Quickshell
import qs
import qs.services
import qs.modules.common
import qs.modules.common.widgets

ContentPage {
    forceWidth: true

    // One control per knob the active theme declares (section 40.9), written to axon.toml's
    // [theme.<id>] table. The model holds the knob ids: each snapshot gives new row objects, and the
    // ids keep every control, with a write it still shows, across them.
    ContentSection {
        icon: "colors"
        title: Translation.tr("Theme")

        Repeater {
            model: ScriptModel {
                values: Theme.knobs.map(knob => knob.id)
            }
            delegate: ThemeKnobControl {
                required property string modelData
                Layout.fillWidth: true
                knobId: modelData
            }
        }

        StyledText {
            visible: Theme.knobs.length === 0
            text: Translation.tr("The active theme has no settings")
            font.pixelSize: Appearance.font.pixelSize.small
            color: Appearance.colors.colSubtext
        }
    }
}
