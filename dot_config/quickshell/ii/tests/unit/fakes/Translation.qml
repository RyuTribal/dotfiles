pragma Singleton

import QtQuick

// Stands in for Translation.qml beside the copy of services/ClaudeUsage.qml in tst_claude_usage.qml:
// every text comes back as written.
QtObject {
    function tr(text) {
        return text;
    }
}
