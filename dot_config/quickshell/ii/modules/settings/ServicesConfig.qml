import QtQuick
import QtQuick.Layouts
import qs
import qs.services
import qs.modules.common
import qs.modules.common.functions
import qs.modules.common.widgets
import Quickshell

ContentPage {
    forceWidth: true

    ContentSection {
        icon: "cell_tower"
        title: Translation.tr("Networking")

        MaterialTextArea {
            Layout.fillWidth: true
            placeholderText: Translation.tr("User agent (for services that require it)")
            text: Config.options.networking.userAgent
            wrapMode: TextEdit.Wrap
            onEditingFinished: {
                Config.options.networking.userAgent = text;
            }
        }
    }

    ContentSection {
        icon: "memory"
        title: Translation.tr("Resources")

        BackendSpinBox {
            key: "resources.interval_ms"
            text: Translation.tr("Polling interval (ms)")
            from: 100
            to: 10000
            stepSize: 100
        }
    }

    ContentSection {
        id: notificationsSection
        icon: "notifications"
        title: Translation.tr("Notifications")

        property bool overridesInvalid: false // the overrides text is not JSON; it stays as typed

        // The sound settings live in axon.toml ([notify.sounds], section 31); the sound theme and the
        // default sound are the active theme's [sounds] (follow-up 10). Each control shows the
        // value of axon's `config` topic and writes `config.set` on user action only, never when a
        // snapshot moves the value. A refused write shows its error under the control, and until the
        // first `config` snapshot the controls say axon is unreachable, as BackendSpinBox does.
        FollowedWrites {
            id: soundsEnabledWrites
            topicValue: AxonConfig.value("notify.sounds.enabled")
            send: (value, done) => AxonConfig.set("notify.sounds.enabled", value, done)
        }

        // Holds the overrides as JSON text, so FollowedWrites compares them by value: an unrelated
        // config snapshot makes a new array but the same text.
        FollowedWrites {
            id: overridesWrites
            topicValue: JSON.stringify(AxonConfig.value("notify.sounds.overrides") ?? [])
            send: (value, done) => AxonConfig.set("notify.sounds.overrides", JSON.parse(value), done)
        }

        ConfigSwitch {
            text: Translation.tr("Enable notification sounds")
            enabled: AxonConfig.writable
            checked: soundsEnabledWrites.shown === true
            // ConfigSwitch flips `checked` on a click, which drops the binding: bind it again.
            onClicked: {
                soundsEnabledWrites.write(!(soundsEnabledWrites.shown === true));
                checked = Qt.binding(() => soundsEnabledWrites.shown === true);
            }
        }

        StyledText {
            visible: text !== ""
            color: Appearance.colors.colError
            text: AxonConfig.known ? soundsEnabledWrites.errorText : "axon unreachable"
        }

        MaterialTextArea {
            Layout.fillWidth: true
            implicitHeight: 180
            enabled: AxonConfig.writable
            placeholderText: Translation.tr("[\n  {\"desktop_entry\": \"discord\", \"sound\": \"message-new-instant\"}\n]")
            text: JSON.stringify(JSON.parse(overridesWrites.shown), null, 2)
            wrapMode: TextEdit.Wrap
            onEditingFinished: {
                let parsed;
                try {
                    parsed = JSON.parse(text);
                } catch (error) {
                    // Text that is not JSON writes nothing and stays as typed, with a message under it.
                    notificationsSection.overridesInvalid = true;
                    return;
                }
                notificationsSection.overridesInvalid = false;
                // Any JSON is sent; axon refuses what is not a list of overrides, and says why below.
                if (JSON.stringify(parsed) !== overridesWrites.shown)
                    overridesWrites.write(JSON.stringify(parsed));
                text = Qt.binding(() => JSON.stringify(JSON.parse(overridesWrites.shown), null, 2));
            }
        }

        StyledText {
            visible: text !== ""
            color: Appearance.colors.colError
            text: !AxonConfig.known ? "axon unreachable" : notificationsSection.overridesInvalid ? Translation.tr("Not valid JSON") : overridesWrites.errorText
        }

        StyledText {
            Layout.fillWidth: true
            color: Appearance.colors.colSubtext
            wrapMode: Text.Wrap
            text: Translation.tr("Overrides match <tt>desktop_entry</tt> and/or <tt>app_name</tt>, ignoring case. Example: <tt>{\"desktop_entry\": \"discord\", \"sound\": \"message-new-instant\"}</tt>. Sound names are looked up in the active theme's sounds; an absolute file path plays that file. If an app sends <tt>sound-name</tt> or <tt>sound-file</tt>, that is used when no override matches; otherwise the active theme's default sound plays.")
        }
    }

    ContentSection {
        icon: "search"
        title: Translation.tr("Search")

        ConfigSwitch {
            text: Translation.tr("Use Levenshtein distance-based algorithm instead of fuzzy")
            checked: Config.options.search.sloppy
            onCheckedChanged: {
                Config.options.search.sloppy = checked;
            }
            StyledToolTip {
                content: Translation.tr("Could be better if you make a ton of typos,\nbut results can be weird and might not work with acronyms\n(e.g. \"GIMP\" might not give you the paint program)")
            }
        }

        ContentSubsection {
            title: Translation.tr("Prefixes")
            ConfigRow {
                uniform: true
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Action")
                    text: Config.options.search.prefix.action
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.action = text;
                    }
                }
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Clipboard")
                    text: Config.options.search.prefix.clipboard
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.clipboard = text;
                    }
                }
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Emojis")
                    text: Config.options.search.prefix.emojis
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.emojis = text;
                    }
                }
            }

            ConfigRow {
                uniform: true
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Math")
                    text: Config.options.search.prefix.math
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.math = text;
                    }
                }
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Shell command")
                    text: Config.options.search.prefix.shellCommand
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.shellCommand = text;
                    }
                }
                MaterialTextArea {
                    Layout.fillWidth: true
                    placeholderText: Translation.tr("Web search")
                    text: Config.options.search.prefix.webSearch
                    wrapMode: TextEdit.Wrap
                    onEditingFinished: {
                        Config.options.search.prefix.webSearch = text;
                    }
                }
            }
        }
        ContentSubsection {
            title: Translation.tr("Web search")
            MaterialTextArea {
                Layout.fillWidth: true
                placeholderText: Translation.tr("Base URL")
                text: Config.options.search.engineBaseUrl
                wrapMode: TextEdit.Wrap
                onEditingFinished: {
                    Config.options.search.engineBaseUrl = text;
                }
            }
        }
    }
}
