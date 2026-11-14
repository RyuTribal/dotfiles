.pragma library

// Pure helpers for Battery.qml, Power.qml and Idle.qml, the thin clients of axon's power engine
// (section 30, "power").

// UPower's State values in their numeric order, as `power.battery` names them. BatteryPopup and the
// indicators compare chargeState with these numbers (4 is fully charged, 1 is charging).
const CHARGE_STATES = ["unknown", "charging", "discharging", "empty", "fully_charged", "pending_charge", "pending_discharge"];

// The number UPower gives a charge state name; a name it does not list is 0, unknown.
function chargeStateNumber(state) {
    const number = CHARGE_STATES.indexOf(state);
    return number < 0 ? 0 : number;
}

// Whether a power widget shows: not before the first snapshot ("waiting") and not on a machine
// without the thing ("unconfigured"). A stale, failed or erroring topic still shows its last value.
function isAvailable(status) {
    return status !== "waiting" && status !== "unconfigured";
}

// Plugged in means charging or waiting to charge, as UPower's state says (the old Battery.qml rule).
function isPluggedIn(state) {
    return state === "charging" || state === "pending_charge";
}

// The mode the power-mode toggle switches to on a click: lap, balanced, max, then lap again. A mode
// this does not know goes to balanced, the mode axon starts every boot in.
function nextMode(mode) {
    switch (mode) {
    case "lap":
        return "balanced";
    case "balanced":
        return "max";
    case "max":
        return "lap";
    default:
        return "balanced";
    }
}

// The session screen's two warnings from a `power.session_busy` reply.
function busyFlags(reply) {
    return {
        packageManagers: reply.package_managers.length > 0,
        downloads: reply.downloads
    };
}
