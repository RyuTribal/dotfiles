pragma Singleton
pragma ComponentBehavior: Bound

import "brightness.js" as Br
import QtQuick
import Quickshell

/**
 * The brightness of each output and the brightness OSD, a thin client of axon's `brightness` topic
 * (section 30, "brightness"). axon reads the eDP backlight and the DDC displays, writes them and keeps
 * the instant of the last change that shows the OSD (`osd_at`) per output; this hands out one
 * BrightnessOutput per output name, steps the focused output, and keeps an OsdDeadline that shows the
 * focused output's OSD for Config.options.osd.timeout.
 *
 * While the topic is unconfigured with reason `not_present` (no backlight and no DDC display) every
 * monitor object is inert (D21). While axon is down the objects keep their last values.
 */
Singleton {
    id: root

    readonly property BackendTopic topic: BackendTopic {
        name: "brightness"
        onSnapshot: data => root._apply(data)
    }
    readonly property OsdDeadline osd: OsdDeadline {
        wallClock: root.wallClock
    }

    readonly property double osdAt: osd.osdAt // the focused output's osd_at, 0 when none
    readonly property bool osdVisible: osd.visible

    // The wall clock in ms, read through this one property so a test can drive it.
    property var wallClock: Date.now

    readonly property bool _notPresent: topic.status === "unconfigured" && topic.errorReason === "not_present"
    // The topic's rows by output id, and the monitor objects handed out by name. Both change in place:
    // getMonitorForScreen() runs inside the bindings that call it, which must not depend on them.
    property var _rows: ({})
    property var _outputs: ({})
    // What a null screen gets.
    readonly property BrightnessOutput _inert: BrightnessOutput {}

    // The monitor object of `screen`'s output: the same object for the life of the shell, never null.
    function getMonitorForScreen(screen) {
        const name = screen?.name ?? "";
        if (name === "")
            return _inert;
        let output = _outputs[name];
        if (!output) {
            output = outputComponent.createObject(root, {
                name: name
            }) as BrightnessOutput;
            const row = _rows[name];
            if (row)
                output._update(Br.rowState(row));
            _outputs[name] = output;
        }
        return output;
    }

    // The focused monitor; axon picks it.
    function increaseBrightness() {
        Backend.call("brightness.step", {
            delta: 5
        });
    }

    function decreaseBrightness() {
        Backend.call("brightness.step", {
            delta: -5
        });
    }

    // Hides the OSD until the next change that shows it.
    function hideOsd() {
        osd.hide();
    }

    function _apply(data) {
        _clearRows();
        for (const row of data.outputs)
            _rows[row.id] = row;
        _feedOutputs();
    }

    function _clearRows() {
        for (const name in _rows)
            delete _rows[name];
    }

    function _feedOutputs() {
        for (const name in _outputs) {
            const row = _rows[name];
            if (row)
                _outputs[name]._update(Br.rowState(row));
            else
                _outputs[name]._clear();
        }
        _moveOsd();
    }

    // The OSD follows the focused output's osd_at.
    function _moveOsd() {
        const row = _rows[Hypr.focusedMonitorName];
        const osdAt = row ? Br.rowState(row).osdAt : 0;
        if (osdAt !== osd.osdAt)
            osd.moveTo(osdAt);
    }

    // The data-null frame keeps the last data (section 20), so the reason is what clears the rows.
    on_NotPresentChanged: {
        if (!_notPresent)
            return;
        _clearRows();
        _feedOutputs();
    }

    Connections {
        target: Hypr
        function onFocusedMonitorNameChanged() {
            root._moveOsd();
        }
    }

    Component {
        id: outputComponent
        BrightnessOutput {}
    }
}
