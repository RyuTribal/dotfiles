import QtQuick 2.15
import QtTest 1.2
import "../../modules/cheatsheet/cheatsheetState.js" as CheatsheetState

TestCase {
    name: "CheatsheetState"

    function test_staleClearedAfterOpenIsIgnored() {
        let state = CheatsheetState.create();

        state = CheatsheetState.open(state);
        verify(state.isOpen);
        verify(state.ignoreClearedOnce);

        state = CheatsheetState.handleCleared(state, false);
        verify(state.isOpen);

        state = CheatsheetState.finishOpenTick(state);
        verify(!state.ignoreClearedOnce);

        state = CheatsheetState.handleCleared(state, false);
        verify(!state.isOpen);
    }

    function test_toggleRespectsCurrentState() {
        let state = CheatsheetState.create();

        state = CheatsheetState.toggle(state);
        verify(state.isOpen);

        state = CheatsheetState.finishOpenTick(state);
        state = CheatsheetState.toggle(state);
        verify(!state.isOpen);
    }
}
