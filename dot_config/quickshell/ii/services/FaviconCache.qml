pragma Singleton
pragma ComponentBehavior: Bound

import qs.modules.common
import QtQuick
import Quickshell
import Quickshell.Io

/**
 * One favicon download per host for the whole shell. A Favicon whose cached file is missing or does
 * not decode asks here. The first ask starts curl, with every value as its own argument (no shell);
 * every later ask for that host in this session starts nothing: while curl runs the rows wait for
 * `downloaded`, after a success the file is there, and after a failure (no favicon, offline) the host
 * stays blank until the shell restarts. So the overview, which makes new rows on every search edit,
 * starts curl at most once per host.
 */
Singleton {
    id: root

    // Each host asked about this session, with "running", "done" or "failed".
    property var _hosts: ({})

    // A download of `host`'s favicon finished and its file is in place.
    signal downloaded(string host)

    // A host with any other character (a quote, `$(`, a port, user info) shows no favicon and runs nothing.
    function isValidHost(host) {
        return /^[A-Za-z0-9.-]{1,253}$/.test(host);
    }

    function fileOf(host) {
        return `${Directories.favicons}/${host}.ico`;
    }

    // Starts the download of `host`'s favicon, unless this session already asked for it.
    function request(host) {
        if (!root.isValidHost(host) || root._hosts[host] !== undefined)
            return;
        root._hosts[host] = "running";
        download.createObject(root, {
            host: host
        });
    }

    Component {
        id: download

        Process {
            id: process
            required property string host

            running: true
            command: ["curl", "-s", "-L", "--fail", "--max-time", "10", "--max-filesize", "1048576", "-A", Config.options?.networking.userAgent ?? "", "-o", root.fileOf(host), `https://www.google.com/s2/favicons?domain=${host}&sz=32`]
            onExited: (exitCode, exitStatus) => {
                const ok = exitCode === 0;
                root._hosts[process.host] = ok ? "done" : "failed";
                if (ok)
                    root.downloaded(process.host);
                process.destroy();
            }
        }
    }
}
