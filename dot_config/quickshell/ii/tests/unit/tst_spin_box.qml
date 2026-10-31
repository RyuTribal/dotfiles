import QtQuick
import QtTest
import SpinBoxUnderTest

// The real modules/common/widgets/StyledSpinBox.qml: what counts as one user modification, which is
// what a settings page writes, and the value a typed entry ends as. Run it through
// backend_tests.sh, which builds the SpinBoxUnderTest module.
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

    // The numbers of axon's polling interval, where the first digits of most typed values lie below
    // `from`.
    StyledSpinBox {
        id: intervalSpin
        from: 100
        to: 10000
        stepSize: 100
        value: 3000
    }

    SignalSpy {
        id: intervalModified
        target: intervalSpin
        signalName: "valueModified"
    }

    SignalSpy {
        id: intervalUnchanged
        target: intervalSpin
        signalName: "entryEndedUnchanged"
    }

    // The numbers of the audio protection's maxAllowed box (GeneralConfig).
    StyledSpinBox {
        id: volumeSpin
        from: 0
        to: 154
        stepSize: 2
        value: 100
    }

    SignalSpy {
        id: volumeModified
        target: volumeSpin
        signalName: "valueModified"
    }

    function field() {
        return fieldOf(spin);
    }

    function fieldOf(spinBox) {
        return findChild(spinBox, "valueField");
    }

    // Focuses the field with its text selected, so the next key typed starts a new entry.
    function startEntry(spinBox, value, spy) {
        spinBox.value = value;
        fieldOf(spinBox).forceActiveFocus();
        fieldOf(spinBox).selectAll();
        spy.clear();
    }

    function init() {
        startEntry(spin, 100, modified);
        intervalUnchanged.clear();
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

    // The 2 and 25 of a typed 2500 lie below `from` (100). They stay in the field as typed and
    // leave the value alone, so the entry ends as 2500, where SpinBox's clamp would make it 100,
    // then 1005, then 10000.
    function test_aNumberBelowFromIsNotClampedWhileTyped() {
        startEntry(intervalSpin, 3000, intervalModified);
        keyClick(Qt.Key_2);
        keyClick(Qt.Key_5);
        compare(fieldOf(intervalSpin).text, "25");
        compare(intervalSpin.value, 3000);
        keyClick(Qt.Key_0);
        keyClick(Qt.Key_0);
        compare(fieldOf(intervalSpin).text, "2500");
        compare(intervalModified.count, 0);
        keyClick(Qt.Key_Return);
        compare(intervalModified.count, 1);
        compare(intervalSpin.value, 2500);
    }

    // An entry that ends outside from..to is clamped into it when it is finished, and the field
    // shows that value and keeps following the value.
    function test_anEntryBelowFromEndsAsFrom() {
        startEntry(intervalSpin, 3000, intervalModified);
        keyClick(Qt.Key_5);
        keyClick(Qt.Key_0);
        compare(intervalSpin.value, 3000);
        keyClick(Qt.Key_Enter);
        compare(fieldOf(intervalSpin).text, "100");
        compare(intervalSpin.value, 100);
        compare(intervalModified.count, 1);
        compare(intervalUnchanged.count, 0);
        intervalSpin.value = 4000;
        compare(fieldOf(intervalSpin).text, "4000");
    }

    // 2000, the last number within from..to while 20000 was typed, is not where the entry ends.
    function test_anEntryAboveToEndsAsTo() {
        startEntry(intervalSpin, 3000, intervalModified);
        for (const key of [Qt.Key_2, Qt.Key_0, Qt.Key_0, Qt.Key_0, Qt.Key_0])
            keyClick(key);
        keyClick(Qt.Key_Return);
        compare(fieldOf(intervalSpin).text, "10000");
        compare(intervalSpin.value, 10000);
        compare(intervalModified.count, 1);
    }

    function test_leavingAnEntryBelowFromEndsAsFrom() {
        startEntry(intervalSpin, 3000, intervalModified);
        keyClick(Qt.Key_5);
        tc.forceActiveFocus(); // focus moves elsewhere
        compare(fieldOf(intervalSpin).text, "100");
        compare(intervalSpin.value, 100);
        compare(intervalModified.count, 1);
        // That entry is over, so focusing the field and leaving it again modifies nothing.
        fieldOf(intervalSpin).forceActiveFocus();
        tc.forceActiveFocus();
        compare(intervalModified.count, 1);
    }

    // An emptied field is no number, so the entry ends as the value from before it, which is no
    // modification.
    function test_anEmptiedFieldKeepsTheValue() {
        startEntry(intervalSpin, 3000, intervalModified);
        keyClick(Qt.Key_Backspace);
        keyClick(Qt.Key_Return);
        compare(fieldOf(intervalSpin).text, "3000");
        compare(intervalSpin.value, 3000);
        compare(intervalModified.count, 0);
        compare(intervalUnchanged.count, 1);
    }

    // Deleting 3000 key by key passes 300, which lies within from..to. Once the text leaves
    // from..to the value is the one from before the entry again, so the emptied field keeps 3000.
    function test_backspacingToAnEmptyFieldKeepsTheValue() {
        startEntry(intervalSpin, 3000, intervalModified);
        keyClick(Qt.Key_End);
        keyClick(Qt.Key_Backspace);
        compare(intervalSpin.value, 300);
        keyClick(Qt.Key_Backspace);
        compare(fieldOf(intervalSpin).text, "30");
        compare(intervalSpin.value, 3000);
        keyClick(Qt.Key_Backspace);
        keyClick(Qt.Key_Backspace);
        keyClick(Qt.Key_Return);
        compare(fieldOf(intervalSpin).text, "3000");
        compare(intervalSpin.value, 3000);
        compare(intervalModified.count, 0);
        compare(intervalUnchanged.count, 1);
    }

    function test_theSameNumberRetypedIsNoModification_data() {
        return [
            {
                tag: "Return",
                finish: () => keyClick(Qt.Key_Return)
            },
            {
                tag: "leaving the field",
                finish: () => tc.forceActiveFocus()
            }
        ];
    }

    // Retyping 3000 previews 300 on the way, since 300 lies within from..to, and the entry still
    // ends as the value from before it.
    function test_theSameNumberRetypedIsNoModification(data) {
        startEntry(intervalSpin, 3000, intervalModified);
        for (const key of [Qt.Key_3, Qt.Key_0, Qt.Key_0])
            keyClick(key);
        compare(intervalSpin.value, 300);
        keyClick(Qt.Key_0);
        data.finish();
        compare(fieldOf(intervalSpin).text, "3000");
        compare(intervalSpin.value, 3000);
        compare(intervalModified.count, 0);
        compare(intervalUnchanged.count, 1);
    }

    // A typed 50 ends as 100 once SpinBox clamps it into 100..10000. Over 100 that is the value
    // from before the entry, so it is no modification.
    function test_anEntryClampedToTheValueBeforeItIsNoModification() {
        startEntry(intervalSpin, 100, intervalModified);
        keyClick(Qt.Key_5);
        keyClick(Qt.Key_0);
        keyClick(Qt.Key_Return);
        compare(fieldOf(intervalSpin).text, "100");
        compare(intervalSpin.value, 100);
        compare(intervalModified.count, 0);
        compare(intervalUnchanged.count, 1);
    }

    // While 200 is typed into 0..154, the step starts from 100, the value before the entry, not
    // from 20, the last number within from..to.
    function test_aStepAfterAnEntryAboveToStepsFromTheValueBeforeIt() {
        startEntry(volumeSpin, 100, volumeModified);
        for (const key of [Qt.Key_2, Qt.Key_0, Qt.Key_0])
            keyClick(key);
        compare(volumeSpin.value, 100);
        keyClick(Qt.Key_Up);
        compare(volumeSpin.value, 102);
        compare(fieldOf(volumeSpin).text, "102");
        compare(volumeModified.count, 1);
    }

    // A step during a typed entry ends the entry and is its one modification, so finishing the
    // entry afterwards modifies nothing more.
    function test_aStepEndsATypedEntry() {
        startEntry(intervalSpin, 3000, intervalModified);
        for (const key of [Qt.Key_2, Qt.Key_5, Qt.Key_0])
            keyClick(key);
        keyClick(Qt.Key_Up);
        compare(intervalSpin.value, 350);
        compare(fieldOf(intervalSpin).text, "350");
        compare(intervalModified.count, 1);
        keyClick(Qt.Key_Return);
        tc.forceActiveFocus(); // focus moves elsewhere
        compare(intervalSpin.value, 350);
        compare(intervalModified.count, 1);
        compare(intervalUnchanged.count, 0);
    }

    // The field's validator reads the text by the spin box's locale, and en_US takes a group
    // separator, so a typed 1,000 ends as 1000, not 1.
    function test_aGroupSeparatorReadsAsTheLocaleDoes() {
        intervalSpin.locale = Qt.locale("en_US");
        startEntry(intervalSpin, 3000, intervalModified);
        for (const character of "1,000")
            keyClick(character);
        compare(fieldOf(intervalSpin).text, "1,000");
        keyClick(Qt.Key_Return);
        compare(intervalSpin.value, 1000);
        compare(fieldOf(intervalSpin).text, "1000");
        compare(intervalModified.count, 1);
    }

    // The field shows textFromValue, and the default one is the plain number, never Qt's locale
    // grouping, so a spin box that sets no textFromValue of its own looks as it always did.
    function test_theFieldShowsThePlainNumber() {
        spin.locale = Qt.locale("en_US");
        spin.value = 1000;
        compare(field().text, "1000");
    }
}
