import "brightness.js" as Br
import QtQuick

/**
 * One output's brightness, as Brightness.getMonitorForScreen() hands it to the bar, the sidebar and the
 * OSD (D18). Brightness keeps one per output name for the life of the shell and feeds it the output's
 * topic row. Without a row (a screen axon has no backlight or DDC display for, a null screen, or no
 * outputs at all) it is inert: `available` false and the functions send nothing.
 */
QtObject {
    id: root

    property string name // the Hyprland output name, the row's `id`

    readonly property bool available: _available
    readonly property bool ready: _ready // the row has a reading
    readonly property real brightness: _brightness // 0..1, the last reading
    readonly property bool isDdc: _isDdc
    readonly property double osdAt: _osdAt // the row's osd_at, 0 when none

    property bool _available: false
    property bool _ready: false
    property real _brightness: 0
    property bool _isDdc: false
    property double _osdAt: 0

    // v on the slider's 0..1 scale.
    function setBrightness(v) {
        if (!_available)
            return;
        Backend.call("brightness.set", {
            output: name,
            percent: Br.percentFor(v)
        });
    }

    // delta in percent; axon moves from the latest requested level, so a scroll burst loses no step.
    function stepBrightness(delta) {
        if (!_available)
            return;
        Backend.call("brightness.step", {
            output: name,
            delta: delta
        });
    }

    // Takes brightness.js rowState(row). A row without a reading keeps the last brightness.
    function _update(state) {
        _available = state.available;
        _ready = state.ready;
        if (state.ready)
            _brightness = state.brightness;
        _isDdc = state.isDdc;
        _osdAt = state.osdAt;
    }

    // The row is gone, or the topic says no outputs are present: inert, the last brightness kept.
    function _clear() {
        _available = false;
        _ready = false;
        _osdAt = 0;
    }
}
