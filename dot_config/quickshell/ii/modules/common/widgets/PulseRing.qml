import qs.modules.common
import QtQuick

// A ring of short radial bars around a circular piece of art, each bar's
// length driven by one cava visualizerPoints bin - a circular cousin of
// WaveVisualizer.qml's linear waveform. Reuses that same data shape and
// moving-average smoothing idiom (see WaveVisualizer.qml) rather than
// inventing a different one, just maps the result to an angle+radius
// instead of an x/y waveform.
Canvas {
    id: root
    property list<var> points
    property list<var> smoothPoints
    property real maxVisualizerValue: 1000
    property int smoothing: 2
    property bool live: true
    property color color: Appearance.m3colors.m3primary

    // Geometry: bars start at innerRadius (the art circle's edge, plus a
    // small gap) and extend outward by up to maxAmplitude at full signal.
    property real innerRadius: 90
    property real maxAmplitude: 16
    property real barWidth: 3
    property real gap: 4

    onPointsChanged: () => {
        root.requestPaint();
    }
    onLiveChanged: () => {
        root.requestPaint();
    }
    onColorChanged: () => {
        root.requestPaint();
    }

    anchors.fill: parent
    onPaint: {
        var ctx = getContext("2d");
        ctx.clearRect(0, 0, width, height);

        var pts = root.points;
        var n = pts.length;
        if (n < 2)
            return;

        var maxVal = root.maxVisualizerValue || 1;

        // Same simple moving-average smoothing as WaveVisualizer.
        var smoothWindow = root.smoothing;
        root.smoothPoints = [];
        for (var i = 0; i < n; ++i) {
            var sum = 0, count = 0;
            for (var j = -smoothWindow; j <= smoothWindow; ++j) {
                var idx = Math.max(0, Math.min(n - 1, i + j));
                sum += pts[idx];
                count++;
            }
            root.smoothPoints.push(sum / count);
        }
        if (!root.live)
            root.smoothPoints.fill(0);

        var cx = width / 2;
        var cy = height / 2;
        var r0 = root.innerRadius + root.gap;

        ctx.lineCap = "round";
        ctx.lineWidth = root.barWidth;
        ctx.strokeStyle = Qt.rgba(root.color.r, root.color.g, root.color.b, 0.8);

        for (var k = 0; k < n; ++k) {
            var angle = (k / n) * Math.PI * 2 - Math.PI / 2;
            var norm = Math.max(0, Math.min(1, root.smoothPoints[k] / maxVal));
            // Non-linear (^0.6) boost: cava's raw magnitudes are small during
            // quiet passages, and a plain linear map left the ring barely
            // moving except on loud peaks. Sub-1 exponents on a 0..1 value
            // push small inputs up disproportionately more than large ones
            // (0.1 -> ~0.25, 0.5 -> ~0.66, 1 -> 1), so quiet moments still
            // visibly animate without flattening out the loud ones.
            var amp = Math.pow(norm, 0.6) * root.maxAmplitude;
            var r1 = r0 + amp;
            var cosA = Math.cos(angle);
            var sinA = Math.sin(angle);

            ctx.beginPath();
            ctx.moveTo(cx + cosA * r0, cy + sinA * r0);
            ctx.lineTo(cx + cosA * r1, cy + sinA * r1);
            ctx.stroke();
        }
    }
}
