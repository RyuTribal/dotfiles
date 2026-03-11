.pragma library

function create() {
    return _state(false, false);
}

function open(state) {
    if (state.isOpen)
        return state;
    return _state(true, true);
}

function close(state) {
    if (!state.isOpen && !state.ignoreClearedOnce)
        return state;
    return _state(false, false);
}

function toggle(state) {
    return state.isOpen ? close(state) : open(state);
}

function finishOpenTick(state) {
    if (!state.isOpen || !state.ignoreClearedOnce)
        return state;
    return _state(state.isOpen, false);
}

function handleCleared(state, grabIsActive) {
    if (state.ignoreClearedOnce)
        return state;
    if (!grabIsActive && state.isOpen)
        return close(state);
    return state;
}

function _state(isOpen, ignoreClearedOnce) {
    return {
        isOpen: isOpen,
        ignoreClearedOnce: ignoreClearedOnce
    };
}
