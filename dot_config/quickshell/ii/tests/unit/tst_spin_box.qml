import QtQuick
import QtTest
import SpinBoxUnderTest

// The real modules/common/widgets/StyledSpinBox.qml: what counts as one user modification, which is
// what a settings page writes. Run it through backend_tests.sh, which builds the SpinBoxUnderTest module.
TestCase {
    id: tc
    name: "StyledSpinBox"
    when: windowShown

    StyledSpinBox {
        id: spin
        from: 0
        to: 1000
        value: 100
    }

    SignalSpy {
        id: modified
        target: spin
        signalName: "valueModified"
    }

    function field() {
        return spin.contentItem.children[0];
    }

    function init() {
        spin.value = 100;
        field().forceActiveFocus();
        field().selectAll();
        modified.clear();
    }

    // Typing 500 previews each value at once but is one modification, when the entry is finished, so
    // a settings page writes 500 once instead of 5, 50 and 500.
    function test_aTypedNumberIsOneModificationWhenFinished() {
        keyClick(Qt.Key_5);
        keyClick(Qt.Key_0);
        keyClick(Qt.Key_0);
        compare(spin.value, 500);
        compare(modified.count, 0);
        keyClick(Qt.Key_Return);
        compare(modified.count, 1);
        compare(spin.value, 500);
    }

    function test_leavingTheFieldFinishesATypedEntry() {
        keyClick(Qt.Key_7);
        compare(modified.count, 0);
        tc.forceActiveFocus(); // focus moves elsewhere
        compare(modified.count, 1);
        compare(spin.value, 7);
    }

    function test_leavingTheFieldWithoutTypingIsNoModification() {
        tc.forceActiveFocus();
        keyClick(Qt.Key_Return);
        compare(modified.count, 0);
    }

    function test_theArrowKeysStillModify() {
        keyClick(Qt.Key_Up);
        compare(spin.value, 101);
        compare(modified.count, 1);
    }
}
