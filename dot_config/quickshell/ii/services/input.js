.pragma library

// Pure helpers for services/Input.qml. Both answers stay empty unless the input topic's status is
// "ok" (section 30, input): without fcitx5 the keyboard falls back to its own keymap, so the last
// input method would be wrong, and the indicator hides.

// The codes the bar shows, in fcitx5's order.
function layoutCodes(status, methods) {
    if (status !== "ok")
        return [];
    return methods.map(method => method.code);
}

// The code of the current input method, or "" when the list does not hold it.
function currentCode(status, methods, current) {
    if (status !== "ok")
        return "";
    return methods.find(method => method.id === current)?.code ?? "";
}
