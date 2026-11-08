pragma Singleton
pragma ComponentBehavior: Bound

import qs
import QtQuick
import Quickshell

/**
 * The wallpaper selector's client of axon's theme engine (section 40.13): the active theme's images
 * as one lazy list of `theme.list` rows, and the call that makes one of them the wallpaper. axon
 * makes the thumbnails and keeps the folder; the shell only shows the rows.
 *
 * The list is active only while the selector is open, as section 20 binds a list's `active` to its
 * view: this singleton outlives the selector, and the collection's rev moves as each thumbnail lands,
 * so an always active list would fetch page 0 again for every new thumbnail with nothing on screen.
 */
Singleton {
    id: root

    property string searchQuery: "" // every word must appear in the file name, in any case

    readonly property BackendList list: BackendList {
        method: "theme.list"
        collection: "wallpapers"
        args: ({
                theme: Theme.activeThemeId,
                query: root.searchQuery
            })
        collectionRev: Theme.wallpapersRev
        // Until the first theme snapshot names the active theme, theme "" would be answered not_found.
        active: GlobalStates.wallpaperSelectorOpen && Theme.activeThemeId !== ""
    }

    // `name` is a row's id, a file name in the active theme's folder; `done` gets the reply.
    function setWallpaper(name, done) {
        Backend.call("theme.set_wallpaper", {
            name: name
        }, done);
    }
}
