import QtQuick
import QtTest
import qs.modules.bar
import qs.services

// The real modules/bar/ClaudeIndicator.qml over the real ClaudeUsage singleton and the stand-in
// Backend, with the stand-in StyledPopup of fakes/bar, which lays the popup's content out in place.
// Once the probe runs, the engine's fetch_failed message can be about 270 characters ("Probe
// failed, retrying at HH:MM: " and 200 characters of a stderr line). With such a message the popup
// stays within the panel's 390 px, the status takes two lines at most, elided, the age shows whole,
// and the value has no tooltip of its own. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "ClaudeIndicator"
    when: windowShown

    readonly property double now: 1792922460000
    readonly property int minuteMs: 60 * 1000
    readonly property int widthCap: 390
    property double rev: 0

    // A window of its own, since TestCase is an invisible Item and every item under it would read
    // as hidden.
    Window {
        width: 1600
        height: 400
        visible: true

        ClaudeIndicator {
            id: indicator
        }
    }

    function init() {
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = tc.now;
        Backend.downSince = 0;
    }

    // A message as the engine writes it after a probe that printed a long stderr line: 270
    // characters.
    function longMessage() {
        let message = "Probe failed, retrying at 11:20: exited with status 1 without answering: ";
        while (message.length < 270)
            message += "error: the usage request to the server could not be completed ";
        return message.slice(0, 270);
    }

    // A stale `claude` frame 12 min old with the long message, the 5-hour window at 41 %.
    function showLongFailure() {
        tc.rev += 1;
        ClaudeUsage.topic._apply({
            t: "snapshot",
            topic: "claude",
            rev: tc.rev,
            status: "stale",
            source: "push",
            observed_at: tc.now - 12 * tc.minuteMs,
            error: {
                code: "fetch_failed",
                message: longMessage(),
                retry_at: tc.now + 10 * tc.minuteMs
            },
            data: {
                windows: {
                    five_hour: {
                        used_pct: 41.0,
                        resets_at: tc.now + 133 * tc.minuteMs,
                        state: "current",
                        source: "push",
                        observed_at: tc.now - 12 * tc.minuteMs
                    },
                    seven_day: null
                },
                last_session: null
            }
        });
        compare(longMessage().length, 270);
        compare(ClaudeUsage.statusLine, longMessage());
    }

    // The visible texts under item whose text starts with prefix.
    function textsStartingWith(item, prefix) {
        let found = [];
        for (const child of item.children) {
            if (!child.visible)
                continue;
            if (typeof child.text === "string" && child.text.startsWith(prefix))
                found.push(child);
            found = found.concat(textsStartingWith(child, prefix));
        }
        return found;
    }

    // The value's tooltip: the object in a "41%" text's data that has a tooltip's content.
    function valueToolTip() {
        for (const value of textsStartingWith(indicator, "41%")) {
            for (const object of value.data) {
                if (object.content !== undefined)
                    return object;
            }
        }
        return null;
    }

    function test_aLongStatusStaysWithinThePopupWidth() {
        showLongFailure();
        const statusTexts = textsStartingWith(indicator, "Probe failed");
        compare(statusTexts.length, 1, "the status shows once");
        const status = statusTexts[0];
        waitForItemPolished(status.parent);
        verify(status.width <= tc.widthCap + 0.5, "the status is " + status.width + " px wide");
        verify(status.lineCount <= 2, "the status takes " + status.lineCount + " lines");
        verify(status.truncated, "the rest of the status is elided");
        const content = status.parent;
        verify(content.implicitWidth <= tc.widthCap + 0.5, "the popup content's implicitWidth is " + content.implicitWidth);
        const ages = textsStartingWith(indicator, "as of 12 min ago");
        compare(ages.length, 1, "the age shows once");
        verify(!ages[0].truncated, "the age is elided");
    }

    // The popup is the only hover surface: a tooltip on the value drew inside the bar strip over
    // the widgets to its right and took their clicks (Ivan, 2026-11-12).
    function test_theValueHasNoToolTip() {
        showLongFailure();
        compare(textsStartingWith(indicator, "41%").length > 0, true, "the value shows");
        compare(valueToolTip(), null, "the value has no tooltip");
    }
}
