import QtQuick 2.15
import QtTest 1.2
import "../../services/notificationSoundPolicy.js" as NotificationSoundPolicy

TestCase {
    name: "NotificationSoundPolicy"

    readonly property var baseConfig: ({
        enabled: true,
        defaultSound: "message",
        overrides: [],
    })

    function test_overridePrefersDesktopEntry() {
        const sound = NotificationSoundPolicy.resolve({
            appName: "Discord",
            desktopEntry: "discord",
            hints: {},
        }, {
            enabled: true,
            defaultSound: "message",
            overrides: [
                { desktopEntry: "discord", sound: "message-new-instant" },
            ],
        });

        compare(sound.kind, "theme");
        compare(sound.value, "message-new-instant");
        compare(sound.source, "override");
    }

    function test_senderSoundHintIsUsedWhenNoOverride() {
        const sound = NotificationSoundPolicy.resolve({
            appName: "Signal",
            desktopEntry: "signal-desktop",
            hints: {
                "sound-name": "message-new-instant",
            },
        }, baseConfig);

        compare(sound.kind, "theme");
        compare(sound.value, "message-new-instant");
        compare(sound.source, "hint-name");
    }

    function test_defaultSoundIsFallback() {
        const sound = NotificationSoundPolicy.resolve({
            appName: "Discord",
            desktopEntry: "discord",
            hints: {},
        }, baseConfig);

        compare(sound.kind, "theme");
        compare(sound.value, "message");
        compare(sound.source, "default");
    }

    function test_suppressedNotificationsStaySilent() {
        const sound = NotificationSoundPolicy.resolve({
            appName: "Discord",
            desktopEntry: "discord",
            hints: {
                "suppress-sound": true,
            },
        }, {
            enabled: true,
            defaultSound: "message",
            overrides: [
                { desktopEntry: "discord", sound: "message-new-instant" },
            ],
        });

        compare(sound, null);
    }

    function test_fileCommandUsesFileFlag() {
        const command = NotificationSoundPolicy.buildCommand({
            kind: "file",
            value: "/tmp/notify.oga",
        });

        compare(command.length, 3);
        compare(command[0], "canberra-gtk-play");
        compare(command[1], "-f");
        compare(command[2], "/tmp/notify.oga");
    }
}
