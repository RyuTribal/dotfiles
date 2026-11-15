import QtQuick
import QtTest
import qs.modules.common.widgets

// The real modules/common/widgets/BackendSlider.qml over the real StyledSlider.qml (section 20, the
// dragged-slider rule): at most one `send` per 33 ms while dragged, the rest of a burst sent when the
// one-shot timer fires (`_flush`), a final `send` on release while a moved value is unsent (none for
// a click that never moves the handle, fresh or after a drag), and the service value put back only while the slider is not
// pressed. backend_tests.sh stages both into qs.modules.common.widgets. Run
// it through backend_tests.sh.
TestCase {
    id: tc
    name: "BackendSlider"
    when: windowShown
    visible: true
    width: 300
    height: 100

    property double clock: 1000
    property var sent: []

    Component {
        id: sliderComponent
        BackendSlider {
            width: 200
            height: 40
        }
    }

    function makeSlider(serviceValue) {
        const slider = createTemporaryObject(sliderComponent, tc, {
            serviceValue: serviceValue
        });
        slider.wallClock = () => tc.clock;
        slider.send.connect(value => tc.sent.push(value));
        return slider;
    }

    function init() {
        tc.clock = 1000;
        tc.sent = [];
    }

    // Drags go through the mouse, the handle's own path: StyledSlider animates a `value` written from
    // QML (its Behavior), so a test that wrote `value` would read the old one back.
    function press(slider, fraction) {
        mousePress(slider, slider.width * fraction, slider.height / 2);
    }

    function move(slider, fraction) {
        mouseMove(slider, slider.width * fraction, slider.height / 2);
    }

    function release(slider, fraction) {
        mouseRelease(slider, slider.width * fraction, slider.height / 2);
    }

    function test_burstSendsOneThenFlushSendsTheRest() {
        const slider = makeSlider(0.2);
        press(slider, 0.3);
        compare(tc.sent.length, 1);
        const first = slider.value;
        compare(tc.sent[0], first);
        tc.clock += 10;
        move(slider, 0.6);
        compare(tc.sent.length, 1, "a move 10 ms after a send waits");
        verify(slider.value > first);
        slider._flush();
        compare(tc.sent.length, 2);
        compare(tc.sent[1], slider.value);
        release(slider, 0.6);
    }

    function test_timerSendsTheRestOfABurst() {
        const slider = createTemporaryObject(sliderComponent, tc, {
            serviceValue: 0.2
        });
        slider.send.connect(value => tc.sent.push(value));
        press(slider, 0.3);
        move(slider, 0.6);
        compare(tc.sent.length, 1);
        tryCompare(tc.sent, "length", 2, 1000);
        compare(tc.sent[1], slider.value);
        release(slider, 0.6);
    }

    function test_moveAfterTheThrottleGoesOutAtOnce() {
        const slider = makeSlider(0.2);
        press(slider, 0.3);
        compare(tc.sent.length, 1);
        tc.clock += 40;
        move(slider, 0.6);
        compare(tc.sent.length, 2);
        compare(tc.sent[1], slider.value);
        release(slider, 0.6);
    }

    function test_releaseSendsTheFinalValue() {
        const slider = makeSlider(0.2);
        press(slider, 0.25);
        verify(slider.pressed);
        move(slider, 0.75);
        release(slider, 0.75);
        verify(!slider.pressed);
        verify(tc.sent.length >= 1);
        compare(tc.sent[tc.sent.length - 1], slider.value);
        verify(slider.value > 0.5, "the drag moved the value: " + slider.value);
    }

    // A press and release on the handle that never moves it sends nothing: a sink at 120 % shows
    // the handle at 1.0, and a plain click must not write 100.
    function test_clickOnTheHandleSendsNothing() {
        const slider = makeSlider(1.2);
        compare(slider.value, 1);
        const x = slider.handle.x + slider.handle.width / 2;
        const y = slider.handle.y + slider.handle.height / 2;
        mousePress(slider, x, y);
        verify(slider.pressed);
        mouseRelease(slider, x, y);
        verify(!slider.pressed);
        compare(tc.sent.length, 0, "sent " + JSON.stringify(tc.sent));
    }

    // The same click after a drag: the drag's release sends its value and must leave nothing pending,
    // or every later click on the handle writes again.
    function test_clickAfterADragSendsNothing() {
        const slider = makeSlider(0.2);
        press(slider, 0.3);
        tc.clock += 10;
        move(slider, 0.6);
        release(slider, 0.6);
        verify(!slider.pressed);
        verify(tc.sent.length >= 1);
        slider.serviceValue = 1.2;
        tryCompare(slider, "value", 1);
        tc.sent = [];
        tc.clock += 1000;
        const x = slider.handle.x + slider.handle.width / 2;
        const y = slider.handle.y + slider.handle.height / 2;
        mousePress(slider, x, y);
        verify(slider.pressed);
        mouseRelease(slider, x, y);
        verify(!slider.pressed);
        compare(tc.sent.length, 0, "sent " + JSON.stringify(tc.sent));
    }

    function test_valueFollowsServiceValue() {
        const slider = makeSlider(0.2);
        compare(slider.value, 0.2);
        slider.serviceValue = 0.6;
        tryCompare(slider, "value", 0.6);
    }

    function test_valueHeldWhilePressed() {
        // The Binding sets the value at creation, before StyledSlider's Behavior on value is
        // finalized, so no animation runs: the value is there at once, and no queued animation tick
        // can land on the pressed handle.
        const slider = makeSlider(0.6);
        compare(slider.value, 0.6);
        press(slider, 0.25);
        verify(slider.pressed);
        const held = slider.value;
        slider.serviceValue = 0.9;
        wait(50);
        compare(slider.value, held, "pressed: the handle keeps its value");
        release(slider, 0.25);
        tryCompare(slider, "value", 0.9);
    }
}
