import QtQuick 2.15
import QtTest 1.2
import "../../services/backend.js" as B

TestCase {
    name: "Backend"

    readonly property double minute: 60 * 1000
    readonly property double hour: 60 * minute
    readonly property double day: 24 * hour

    function tr(text) {
        return text;
    }

    function test_reuseRowsKeepsIdenticalObject() {
        const previous = B.reuseRows({}, [{ id: "a", rev: 1, name: "first" }]);

        const next = B.reuseRows(previous.index, [{ id: "a", rev: 1, name: "first" }]);

        verify(next.rows[0] === previous.rows[0], "same id and rev keeps the old object");
        verify(next.index["a"] === previous.rows[0]);
    }

    function test_reuseRowsReplacesOnRevChange() {
        const previous = B.reuseRows({}, [{ id: "a", rev: 1, name: "first" }]);
        const changed = { id: "a", rev: 2, name: "renamed" };

        const next = B.reuseRows(previous.index, [changed]);

        verify(next.rows[0] === changed, "a new rev takes the new object");
        compare(next.rows[0].name, "renamed");
    }

    function test_reuseRowsDropsRemovedRows() {
        const previous = B.reuseRows({}, [{ id: "a", rev: 1 }, { id: "b", rev: 1 }]);

        const next = B.reuseRows(previous.index, [{ id: "b", rev: 1 }]);

        compare(next.rows.length, 1);
        verify(next.rows[0] === previous.rows[1]);
        compare(Object.keys(next.index), ["b"]);
    }

    function test_reuseRowsTreatsEveryIdAsAPlainKey() {
        const rows = () => [{ id: "__proto__", rev: 1 }, { id: "constructor", rev: 1 }];
        const previous = B.reuseRows({}, rows());

        const next = B.reuseRows(previous.index, rows());

        verify(next.rows[0] === previous.rows[0]);
        verify(next.rows[1] === previous.rows[1]);
        compare(Object.keys(next.index).length, 2);
    }

    function test_mediaPositionClamps() {
        const nearTheEnd = { position_ms: 60000, rate: 1, sampled_at: 1000, length_ms: 61000 };
        compare(B.mediaPosition(nearTheEnd, 1000 + 5000), 61000);

        const sampledInTheFuture = { position_ms: 1000, rate: 1, sampled_at: 10000, length_ms: 61000 };
        compare(B.mediaPosition(sampledInTheFuture, 10000 - 5000), 0);

        const paused = { position_ms: 1290400, rate: 0, sampled_at: 1000, length_ms: 3412000 };
        compare(B.mediaPosition(paused, 1000 + day), 1290400);

        const noLength = { position_ms: 1000, rate: 1, sampled_at: 1000, length_ms: null };
        compare(B.mediaPosition(noLength, 1000 + hour), 1000 + hour);
    }

    function test_remainingAndElapsed() {
        compare(B.remaining(10000, 4000), 6000);
        compare(B.remaining(4000, 10000), 0);

        compare(B.elapsed({ started_at: 1000, elapsed_ms: null }, 5000), 4000);
        compare(B.elapsed({ started_at: null, elapsed_ms: 83450 }, 99999), 83450);
    }

    function test_ageText() {
        const monday = new Date(2026, 9, 26, 12, 0, 0).getTime();

        compare(B.ageText(4000, monday, tr), "now");
        compare(B.ageText(45000, monday, tr), "45 s");
        compare(B.ageText(12 * minute, monday, tr), "12 min");
        compare(B.ageText(3 * hour, monday, tr), "3 h");
        compare(B.ageText(30 * hour, monday, tr), "since Sun");
    }

    // Every word goes through the caller's tr (D23), one template per text.
    function test_ageTextThroughTr() {
        const monday = new Date(2026, 9, 26, 12, 0, 0).getTime();
        const marked = text => "<" + text + ">";

        compare(B.ageText(4000, monday, marked), "<now>");
        compare(B.ageText(45000, monday, marked), "<45 s>");
        compare(B.ageText(12 * minute, monday, marked), "<12 min>");
        compare(B.ageText(3 * hour, monday, marked), "<3 h>");
        compare(B.ageText(30 * hour, monday, marked), "<since <Sun>>");
    }

    function test_ageTextBoundaries() {
        const monday = new Date(2026, 9, 26, 12, 0, 0).getTime();

        compare(B.ageText(-5000, monday, tr), "now");
        compare(B.ageText(4999, monday, tr), "now");
        compare(B.ageText(5000, monday, tr), "5 s");
        compare(B.ageText(minute - 1, monday, tr), "59 s");
        compare(B.ageText(minute, monday, tr), "1 min");
        compare(B.ageText(hour - 1, monday, tr), "59 min");
        compare(B.ageText(day - 1, monday, tr), "23 h");
        compare(B.ageText(day, monday, tr), "since Sun");
    }

    function test_uptimeText() {
        compare(B.uptimeText(0), "0m");
        compare(B.uptimeText(59 * 1000), "0m");
        compare(B.uptimeText(5 * minute), "5m");
        compare(B.uptimeText(2 * hour), "2h");
        compare(B.uptimeText(2 * hour + 3 * minute), "2h, 3m");
        compare(B.uptimeText(day + 5 * minute), "1d, 5m");
        compare(B.uptimeText(B.uptime(1000, 1000 + 2 * day + 4 * hour)), "2d, 4h");
    }

    function test_uptimeTextNeverNegative() {
        compare(B.uptimeText(-60000), "0m");
        compare(B.uptimeText(B.uptime(10000, 5000)), "0m");
    }

    // system.boot_at is null when /proc/stat has no readable btime: "--", never a made-up uptime.
    function test_uptimeUnknownBootAt() {
        compare(B.uptime(null, 5000), null);
        compare(B.uptime(undefined, 5000), null);
        compare(B.uptimeText(null), "--");
        compare(B.uptimeText(B.uptime(null, 5000)), "--");
    }
}
