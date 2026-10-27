import qs.modules.common
import qs.modules.common.functions
import qs.services
import Qt5Compat.GraphicalEffects
import QtQuick
import Quickshell.Widgets

// The favicon of a link's host. A favicon cached under Directories.favicons shows without a request.
// Only when there is none does FaviconCache download it, once per host for the whole shell.
IconImage {
    id: root
    property string url
    property real size: 32

    readonly property string host: StringUtils.getDomain(url) ?? ""
    readonly property bool hostIsValid: FaviconCache.isValidHost(host)
    readonly property string fileUrl: hostIsValid ? `file://${FaviconCache.fileOf(host)}` : ""

    source: fileUrl
    implicitSize: root.size

    // Error here means no cached file, or one that does not decode. FaviconCache starts curl only for
    // a host it has not been asked about this session, so a file that never decodes is fetched once.
    onStatusChanged: {
        if (status === Image.Error && hostIsValid)
            FaviconCache.request(host);
    }

    Connections {
        target: FaviconCache
        function onDownloaded(host) {
            if (host !== root.host)
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
