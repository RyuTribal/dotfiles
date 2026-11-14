import QtQuick
import Quickshell
import QtQuick.Layouts
import qs
import qs.services
import qs.modules.common
import qs.modules.common.functions
import qs.modules.common.widgets
import "../../services/theme.js" as ThemeLogic

ContentPage {
    forceWidth: true

    ContentSection {
        icon: "volume_up"
        title: Translation.tr("Audio")

        ConfigSwitch {
            text: Translation.tr("Earbang protection")
            checked: Config.options.audio.protection.enable
            onCheckedChanged: {
                Config.options.audio.protection.enable = checked;
            }
            StyledToolTip {
                content: Translation.tr("Prevents abrupt increments and restricts volume limit")
            }
        }
        ConfigRow {
            // uniform: true
            ConfigSpinBox {
                text: Translation.tr("Max allowed increase")
                value: Config.options.audio.protection.maxAllowedIncrease
                from: 0
                to: 100
                stepSize: 2
                onValueChanged: {
                    Config.options.audio.protection.maxAllowedIncrease = value;
                }
            }
            ConfigSpinBox {
                text: Translation.tr("Volume limit")
                value: Config.options.audio.protection.maxAllowed
                from: 0
                to: 154 // pavucontrol allows up to 153%
                stepSize: 2
                onValueChanged: {
                    Config.options.audio.protection.maxAllowed = value;
                }
            }
        }
    }

    ContentSection {
        icon: "battery_android_full"
        title: Translation.tr("Battery")

        // The thresholds and the automatic suspend live in axon's [power.battery] (section 30, "power"),
        // which posts the warnings and suspends. Opening this page writes nothing: each control
        // writes `config.set` on a user action only.
        ConfigRow {
            uniform: true
            BackendSpinBox {
                key: "power.battery.low"
                text: Translation.tr("Low warning")
                from: 1
                to: 100
                stepSize: 5
            }
            BackendSpinBox {
                key: "power.battery.critical"
                text: Translation.tr("Critical warning")
                from: 1
                to: 100
                stepSize: 5
            }
        }
        FollowedWrites {
            id: autoSuspendWrites
            topicValue: AxonConfig.value("power.battery.auto_suspend")
            send: (value, done) => AxonConfig.set("power.battery.auto_suspend", value, done)
        }
        ConfigRow {
            uniform: true
            ConfigSwitch {
                text: Translation.tr("Automatic suspend")
                enabled: AxonConfig.writable
                checked: autoSuspendWrites.shown === true
                // ConfigSwitch flips `checked` on a click, which drops the binding: bind it again.
                onClicked: {
                    autoSuspendWrites.write(!(autoSuspendWrites.shown === true));
                    checked = Qt.binding(() => autoSuspendWrites.shown === true);
                }
                StyledToolTip {
                    content: Translation.tr("Automatically suspends the system when battery is low")
                }
            }
            BackendSpinBox {
                key: "power.battery.suspend"
                text: Translation.tr("Suspend at")
                from: 1
                to: 100
                stepSize: 5
            }
        }
        StyledText {
            visible: text !== ""
            color: Appearance.colors.colError
            text: AxonConfig.known ? autoSuspendWrites.errorText : "axon unreachable"
        }
    }

    ContentSection {
        icon: "language"
        title: Translation.tr("Language")

        ContentSubsection {
            title: Translation.tr("Interface Language")
            tooltip: Translation.tr("Select the language for the user interface.\n\"Auto\" will use your system's locale.")

            ConfigSelectionArray {
                id: languageSelector
                currentValue: Config.options.language.ui
                onSelected: newValue => {
                    Config.options.language.ui = newValue;
                }
                options: [
                    {
                        displayName: Translation.tr("Auto (System)"),
                        value: "auto"
                    },
                    ...Translation.availableLanguages.map(lang => {
                        return {
                            displayName: lang.replace('_', '-'),
                            value: lang
                        };
                    })]
            }
        }
    }

    ContentSection {
        icon: "nest_clock_farsight_analog"
        title: Translation.tr("Time")

        ContentSubsection {
            title: Translation.tr("Format")
            tooltip: ""

            ConfigSelectionArray {
                currentValue: Config.options.time.format
                // Two keys for one control (50.1, "Fallback lock clock"): the shell's clock reads the
                // view key, and the theme renders the fallback locker's clock from the axon key.
                onSelected: newValue => {
                    Config.options.time.format = newValue;
                    AxonConfig.set("theme.lock_clock", ThemeLogic.lockClock(newValue));
                }
                options: [
                    {
                        displayName: Translation.tr("24h"),
                        value: "hh:mm"
                    },
                    {
                        displayName: Translation.tr("12h am/pm"),
                        value: "h:mm ap"
                    },
                    {
                        displayName: Translation.tr("12h AM/PM"),
                        value: "h:mm AP"
                    },
                ]
            }
        }
    }
}
