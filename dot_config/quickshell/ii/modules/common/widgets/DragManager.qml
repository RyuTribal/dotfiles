import qs.modules.common
import qs.services
import QtQuick

/**
 * A convenience MouseArea for handling drag events.
 */
MouseArea {
    id: root
    hoverEnabled: true
    acceptedButtons: Qt.LeftButton

    property bool interactive: true
    property bool automaticallyReset: true
    readonly property real dragDiffX: _dragDiffX
    readonly property real dragDiffY: _dragDiffY
    // The drag's horizontal speed in px/s from the press to the release, set before dragReleased. Qt
    // Quick Controls' SwipeDelegate measures a swipe the same way (QQuickVelocityCalculator).
    readonly property real releaseVelocityX: _releaseVelocityX
    // A release faster than this to the right counts as a flick: SwipeDelegate's
    // exposeVelocityThreshold (qquickswipedelegate.cpp, Qt 6.12).
    property real flickVelocity: 300

    signal dragPressed(diffX: real, diffY: real)
    signal dragReleased(diffX: real, diffY: real)

    property real startX: 0
    property real startY: 0
    property bool dragging: false
    property real _dragDiffX: 0
    property real _dragDiffY: 0
    property real _releaseVelocityX: 0
    property double _pressTime: 0

    // Whether a release at diffX throws the item out to the right: dragged past `distance`, or flicked,
    // which SwipeDelegate counts once the swipe has begun (past the drag distance) and is fast enough.
    function throwsRight(diffX, distance) {
        if (diffX > distance)
            return true;
        return diffX > Qt.styleHints.startDragDistance && root.releaseVelocityX > root.flickVelocity;
    }

    function resetDrag() {
        _dragDiffX = 0
        _dragDiffY = 0
    }

    onPressed: (mouse) => {
        if (!root.interactive) {
            if (mouse.button === Qt.LeftButton) {
                mouse.accepted = false;
            }
            return;
        }
        if (mouse.button === Qt.LeftButton) {
            startX = mouse.x
            startY = mouse.y
            _pressTime = Date.now();
        }
    }
    onReleased: (mouse) => {
        root.preventStealing = false;
        if (!root.interactive) {
            return;
        }
        dragging = false
        const elapsed = Date.now() - _pressTime;
        _releaseVelocityX = elapsed > 0 ? _dragDiffX * 1000 / elapsed : 0;
        root.dragReleased(_dragDiffX, _dragDiffY);
        if (root.automaticallyReset) {
            root.resetDrag();
        }
    }
    onPositionChanged: (mouse) => {
        if (!root.interactive) {
            return;
        }
        if (mouse.buttons & Qt.LeftButton) {
            root._dragDiffX = mouse.x - startX
            root._dragDiffY = mouse.y - startY
            // A drag that goes sideways more than down, past the drag distance, is this item's, as
            // SwipeDelegate takes a swipe: otherwise a list around it takes a slightly diagonal drag
            // as a scroll and cancels it. The list lets through the move it detects the drag on, so
            // claiming it here keeps the grab.
            const sideways = Math.abs(root._dragDiffX);
            if (!root.preventStealing && sideways > Qt.styleHints.startDragDistance && sideways > Math.abs(root._dragDiffY))
                root.preventStealing = true;
            root.dragPressed(_dragDiffX, _dragDiffY);
            root.dragging = true;
        }
    }
    onCanceled: (mouse) => {
        root.preventStealing = false;
        if (!root.interactive) {
            return;
        }
        released(mouse);
    }
}
