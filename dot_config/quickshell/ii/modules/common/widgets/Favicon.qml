import qs.modules.common
import qs.modules.common.functions
import Qt5Compat.GraphicalEffects
import QtQuick
import Quickshell.Io
import Quickshell.Widgets

// The favicon of a link's host. A favicon cached under Directories.favicons shows without a request.
// Only when there is none does curl run, once, with every value as its own argument (no shell).
IconImage {
    id: root
    property string url
    property real size: 32

    // A host with any other character (a quote, `$(`, a port, user info) shows no favicon and runs nothing.
    readonly property string host: StringUtils.getDomain(url) ?? ""
    readonly property bool hostIsValid: /^[A-Za-z0-9.-]{1,253}$/.test(host)
    readonly property string filePath: `${Directories.favicons}/${host}.ico`
    readonly property string fileUrl: hostIsValid ? `file://${filePath}` : ""
    property bool downloadStarted: false

    source: fileUrl
    implicitSize: root.size

    // Error here means no cached file (or one that does not decode). One download per instance, so a
    // file that never decodes cannot start curl again and again.
    onStatusChanged: {
        if (status === Image.Error && hostIsValid && !downloadStarted) {
            downloadStarted = true;
            download.running = true;
        }
    }

    Process {
        id: download
        command: ["curl", "-s", "-L", "--fail", "--max-time", "10", "--max-filesize", "1048576", "-A", Config.options?.networking.userAgent ?? "", "-o", root.filePath, `https://www.google.com/s2/favicons?domain=${root.host}&sz=32`]
        onExited: (exitCode, exitStatus) => {
            if (exitCode !== 0)
                return;
            // Load the same URL again. Qt does not cache an image that failed to load, so this reads the
            // new file.
            root.source = "";
            root.source = Qt.binding(() => root.fileUrl);
        }
    }

    layer.enabled: true
    layer.effect: OpacityMask {
        maskSource: Rectangle {
            width: root.implicitSize
            height: root.implicitSize
            radius: Appearance.rounding.full
        }
    }
}
