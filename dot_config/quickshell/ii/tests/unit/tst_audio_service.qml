import QtQuick
import QtTest
import ShellUnderTest

// The real services/Audio.qml over the stand-in Backend of fakes/Backend.qml: the Parity names built
// from the `audio` topic, the OSD deadline and the calls the functions send (section 31 audio, D16).
// backend_tests.sh copies it without its `pragma Singleton` line into ShellUnderTest as AudioService,
// beside audio.js and backend.js, so each test makes its own instance. Config.options.osd.timeout is
// 1000 in fakes/qs/modules/common/Config.qml. Run it through backend_tests.sh.
TestCase {
    id: tc
    name: "AudioService"

    Component {
        id: audioComponent
        AudioService {}
    }

    property double rev: 0
    property double clock: 1792922458100

    function init() {
        Backend.calls = [];
        Backend.attachments = [];
        Backend.up = true;
        Backend.instance = "one";
        Backend.now = 1792922460000;
        Backend.downSince = 0;
        tc.rev = 0;
        tc.clock = 1792922458100;
    }

    // An instance whose wall clock reads tc.clock.
    function makeAudio() {
        const audio = createTemporaryObject(audioComponent, tc);
        audio.wallClock = () => tc.clock;
        return audio;
    }

    function frame(audio, status, data, error) {
        tc.rev += 1;
        audio.topic._apply({
            t: "snapshot",
            topic: "audio",
            rev: tc.rev,
            status: status,
            source: "live",
            observed_at: 1792922382000,
            error: error,
            ops: [],
            data: data
        });
    }

    function lastCall() {
        return Backend.calls[Backend.calls.length - 1];
    }

    // Section 31's `audio` example, with osd_at long past so no test sees the OSD it does not ask for.
    function audioData(overrides) {
        return Object.assign({
            ready: true,
            sink: {
                id: "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink",
                description: "Meteor Lake-P HD Audio Controller Speaker",
                volume_percent: 50,
                muted: false
            },
            source: {
                id: "alsa_input.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Mic__source",
                description: "Meteor Lake-P HD Audio Controller Digital Microphone",
                volume_percent: 100,
                muted: false
            },
            sinks: [
                {
                    id: "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink",
                    rev: 4,
                    description: "Meteor Lake-P HD Audio Controller Speaker",
                    default: true
                },
                {
                    id: "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__HDMI1__sink",
                    rev: 1,
                    description: "Meteor Lake-P HD Audio Controller HDMI / DisplayPort 1 Output",
                    default: false
                }
            ],
            sources: [
                {
                    id: "alsa_input.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Mic__source",
                    rev: 2,
                    description: "Meteor Lake-P HD Audio Controller Digital Microphone",
                    default: true
                }
            ],
            streams: [
                {
                    id: "4127",
                    rev: 9,
                    app_name: "Brave",
                    media_name: "Playback",
                    icon_name: "brave-browser",
                    node_name: "Brave",
                    volume_percent: 100,
                    muted: false
                }
            ],
            protection: {
                enabled: false,
                last_trigger: null
            },
            osd_at: 1792922000000
        }, overrides ?? {});
    }

    function sourceDown() {
        return {
            code: "source_down",
            message: "pipewire: /run/user/1000/pipewire-0 does not exist"
        };
    }

    function test_sinkFromSnapshot() {
        const audio = makeAudio();
        frame(audio, "ok", audioData());
        compare(audio.ready, true);
        compare(audio.sink.ready, true);
        compare(audio.sink.nodeName, "alsa_output.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Speaker__sink");
        compare(audio.source.nodeName, "alsa_input.pci-0000_00_1f.3-platform-sof_sdw.HiFi__Mic__source");
        compare(audio.sink.audio.volume, 0.5);
        compare(audio.volumePercent, 50);
        compare(audio.sink.description, "Meteor Lake-P HD Audio Controller Speaker");
        compare(audio.sink.audio.muted, false);
        compare(audio.isMuted, false);
        compare(audio.materialSymbol, "volume_up");
        compare(audio.source.description, "Meteor Lake-P HD Audio Controller Digital Microphone");
        compare(audio.source.audio.volume, 1);
        compare(audio.source.audio.muted, false);
        compare(audio.source.ready, true);
        compare(audio.sinks.length, 2);
        compare(audio.sinks[0].isDefault, true);
        compare(audio.sinks[1].isDefault, false);
        compare(audio.sources.length, 1);
        compare(audio.streams.length, 1);
        compare(audio.streams[0].appName, "Brave");
        compare(audio.streams[0].audio.volume, 1);
    }

    function test_descriptionNullUntilKnown() {
        const audio = makeAudio();
        compare(audio.sink.description, null);
        compare(audio.source.description, null);
        compare(audio.ready, false);
    }

    function test_readyNeedsOkStatus() {
        const audio = makeAudio();
        frame(audio, "starting", audioData({
            ready: false
        }));
        compare(audio.ready, false);
        frame(audio, "ok", audioData());
        compare(audio.ready, true);
        frame(audio, "failed", audioData());
        compare(audio.ready, false);
        compare(audio.sink.ready, false);
        frame(audio, "ok", audioData({
            ready: false
        }));
        compare(audio.ready, false);
        compare(audio.sink.ready, false);
        compare(audio.source.ready, false);
    }

    function test_isMutedFalseWhileNotReady() {
        const audio = makeAudio();
        frame(audio, "ok", audioData({
            sink: Object.assign(audioData().sink, {
                muted: true
            })
        }));
        compare(audio.isMuted, true);
        frame(audio, "stale", audioData({
            sink: Object.assign(audioData().sink, {
                muted: true
            })
        }), sourceDown());
        compare(audio.ready, false);
        compare(audio.isMuted, false);
    }

    function test_unavailableWhileSourceDown() {
        const audio = makeAudio();
        frame(audio, "ok", audioData());
        compare(audio.unavailable, false);
        frame(audio, "stale", audioData(), sourceDown());
        compare(audio.unavailable, true);
        compare(audio.ready, false);
        compare(audio.materialSymbol, "volume_off");
        frame(audio, "ok", audioData());
        compare(audio.unavailable, false);
        compare(audio.materialSymbol, "volume_up");
    }

    function test_keepsLastValueWhileAxonDown() {
        const audio = makeAudio();
        frame(audio, "ok", audioData());
        Backend.up = false;
        Backend.downSince = 1792922459000;
        compare(audio.volumePercent, 50);
        compare(audio.sink.audio.volume, 0.5);
        compare(audio.sink.description, "Meteor Lake-P HD Audio Controller Speaker");
        compare(audio.streams.length, 1);
        // A stale frame carries the last data: nothing reads 0.
        frame(audio, "stale", audioData(), sourceDown());
        compare(audio.volumePercent, 50);
        compare(audio.sink.audio.volume, 0.5);
    }

    function test_nudgeSendsAudioNudge() {
        const audio = makeAudio();
        audio.nudgeVolume(2);
        compare(Backend.calls.length, 1);
        compare(lastCall().method, "audio.nudge");
        compare(lastCall().args, {
            target: "sink",
            delta: 2
        });
        audio.nudgeVolume(-1);
        compare(lastCall().args, {
            target: "sink",
            delta: -1
        });
    }

    function test_setVolumeLinearSendsPercent() {
        const audio = makeAudio();
        audio.setVolumeLinear(0.42);
        compare(lastCall().method, "audio.set_volume");
        compare(lastCall().args, {
            target: "sink",
            percent: 42
        });
        audio.setVolumePercent(37);
        compare(lastCall().method, "audio.set_volume");
        compare(lastCall().args, {
            target: "sink",
            percent: 37
        });
    }

    function test_muteCalls() {
        const audio = makeAudio();
        audio.toggleMute();
        compare(lastCall().method, "audio.toggle_mute");
        compare(lastCall().args, {
            target: "sink"
        });
        audio.setMuted(true);
        compare(lastCall().method, "audio.set_mute");
        compare(lastCall().args, {
            target: "sink",
            muted: true
        });
    }

    function test_toggleSourceMute() {
        const audio = makeAudio();
        audio.toggleSourceMute();
        compare(Backend.calls.length, 1);
        compare(lastCall().method, "audio.toggle_mute");
        compare(lastCall().args, {
            target: "source"
        });
    }

    function test_setDefault() {
        const audio = makeAudio();
        audio.setDefault("source", "x");
        compare(lastCall().method, "audio.set_default");
        compare(lastCall().args, {
            kind: "source",
            id: "x"
        });
    }

    function test_setStreamVolume() {
        const audio = makeAudio();
        audio.setStreamVolume("4127", 0.3);
        compare(lastCall().method, "audio.set_volume");
        compare(lastCall().args, {
            target: "stream",
            id: "4127",
            percent: 30
        });
    }

    function test_osdVisibleUntilTimeout() {
        const audio = makeAudio();
        frame(audio, "ok", audioData());
        compare(audio.osdVisible, false);
        frame(audio, "ok", audioData({
            osd_at: tc.clock
        }));
        compare(audio.osdAt, tc.clock);
        compare(audio.osdVisible, true);
        // The one-shot timer's handler, run at the deadline.
        tc.clock += 1000;
        audio.osd._arm();
        compare(audio.osdVisible, false);
    }

    function test_osdTimerFiresAtTheDeadline() {
        const audio = createTemporaryObject(audioComponent, tc);
        frame(audio, "ok", audioData({
            osd_at: Date.now()
        }));
        compare(audio.osdVisible, true);
        tryCompare(audio, "osdVisible", false, 3000);
    }

    function test_passedDeadlineHidesAtOnce() {
        const audio = makeAudio();
        frame(audio, "ok", audioData({
            osd_at: tc.clock - 1000
        }));
        compare(audio.osdVisible, false);
        frame(audio, "ok", audioData({
            osd_at: tc.clock - 1001
        }));
        compare(audio.osdVisible, false);
    }

    // hideOsd() hides the run and its trigger message: a later re-arm keeps it hidden, and the next
    // change inside the old timeout starts a new run without the dismissed message.
    function test_hideOsdHidesUntilNextChange() {
        const audio = makeAudio();
        const trigger = {
            enabled: true,
            last_trigger: {
                reason: "max_step",
                at: tc.clock
            }
        };
        frame(audio, "ok", audioData({
            protection: trigger,
            osd_at: tc.clock
        }));
        compare(audio.osdVisible, true);
        compare(audio.osdMessage, "Illegal increment");
        audio.hideOsd();
        compare(audio.osdVisible, false);
        // The one-shot timer's handler, still inside the old timeout.
        tc.clock += 100;
        audio.osd._arm();
        compare(audio.osdVisible, false);
        tc.clock += 100;
        frame(audio, "ok", audioData({
            protection: trigger,
            osd_at: tc.clock
        }));
        compare(audio.osdVisible, true);
        compare(audio.osdMessage, "");
    }

    function test_osdMessageFromLastTrigger() {
        const audio = makeAudio();
        frame(audio, "ok", audioData({
            osd_at: tc.clock
        }));
        compare(audio.osdReason, "");
        compare(audio.osdMessage, "");
        tc.clock += 200;
        frame(audio, "ok", audioData({
            protection: {
                enabled: true,
                last_trigger: {
                    reason: "max_step",
                    at: tc.clock
                }
            },
            osd_at: tc.clock
        }));
        compare(audio.osdVisible, true);
        compare(audio.osdReason, "max_step");
        compare(audio.osdMessage, "Illegal increment");
        // A plain change later in the same run keeps the message.
        tc.clock += 300;
        frame(audio, "ok", audioData({
            protection: {
                enabled: true,
                last_trigger: {
                    reason: "max_step",
                    at: tc.clock - 300
                }
            },
            osd_at: tc.clock
        }));
        compare(audio.osdMessage, "Illegal increment");
        // After the run ends, a new run without a trigger has no message.
        const triggerAt = tc.clock - 300;
        tc.clock += 1000;
        audio.osd._arm();
        compare(audio.osdVisible, false);
        compare(audio.osdMessage, "");
        tc.clock += 500;
        frame(audio, "ok", audioData({
            protection: {
                enabled: true,
                last_trigger: {
                    reason: "max_step",
                    at: triggerAt
                }
            },
            osd_at: tc.clock
        }));
        compare(audio.osdVisible, true);
        compare(audio.osdMessage, "");
        tc.clock += 100;
        frame(audio, "ok", audioData({
            protection: {
                enabled: true,
                last_trigger: {
                    reason: "max_volume",
                    at: tc.clock
                }
            },
            osd_at: tc.clock
        }));
        compare(audio.osdMessage, "Exceeded max allowed");
    }

    // The timer did not fire at the deadline (a suspend): the next osd_at still starts a new run, so
    // the old run's trigger message does not come back.
    function test_newRunAfterAMissedTimerFire() {
        const audio = makeAudio();
        const trigger = {
            enabled: true,
            last_trigger: {
                reason: "max_step",
                at: tc.clock
            }
        };
        frame(audio, "ok", audioData({
            protection: trigger,
            osd_at: tc.clock
        }));
        compare(audio.osdMessage, "Illegal increment");
        tc.clock += 5000;
        frame(audio, "ok", audioData({
            protection: trigger,
            osd_at: tc.clock
        }));
        compare(audio.osdVisible, true);
        compare(audio.osdMessage, "");
    }

    function test_rowsReuseUnchangedObjects() {
        const audio = makeAudio();
        frame(audio, "ok", audioData());
        const sink = audio.sinks[0];
        const hdmi = audio.sinks[1];
        const mic = audio.sources[0];
        const stream = audio.streams[0];
        const changedHdmi = Object.assign({}, audioData().sinks[1], {
            rev: 2,
            default: true
        });
        const changedStream = Object.assign({}, audioData().streams[0], {
            rev: 10,
            volume_percent: 30
        });
        frame(audio, "ok", audioData({
            sinks: [audioData().sinks[0], changedHdmi],
            streams: [changedStream]
        }));
        verify(audio.sinks[0] === sink, "same rev keeps the identical sink row");
        verify(audio.sources[0] === mic, "same rev keeps the identical source row");
        verify(audio.sinks[1] !== hdmi, "a changed rev gives a new sink row");
        compare(audio.sinks[1].isDefault, true);
        verify(audio.streams[0] !== stream, "a changed rev gives a new stream row");
        compare(audio.streams[0].audio.volume, 0.3);
    }

    function test_nullSinkKeepsLastValue() {
        const audio = makeAudio();
        frame(audio, "ok", audioData({
            sink: Object.assign(audioData().sink, {
                volume_percent: 42,
                muted: true
            }),
            osd_at: tc.clock - 5000
        }));
        compare(audio.isMuted, true);
        frame(audio, "ok", audioData({
            sink: null,
            source: null,
            osd_at: tc.clock - 5000
        }));
        compare(audio.volumePercent, 42);
        compare(audio.sink.audio.volume, 0.42);
        compare(audio.sink.audio.muted, true);
        compare(audio.sink.description, "Meteor Lake-P HD Audio Controller Speaker");
        compare(audio.ready, false);
        compare(audio.sink.ready, false);
        compare(audio.materialSymbol, "volume_off");
        compare(audio.isMuted, false);
        compare(audio.osdVisible, false);
        compare(audio.source.ready, false);
        compare(audio.source.audio.volume, 1);
        compare(audio.source.description, "Meteor Lake-P HD Audio Controller Digital Microphone");
        // The sink comes back: ready again with its new value.
        frame(audio, "ok", audioData({
            osd_at: tc.clock - 5000
        }));
        compare(audio.ready, true);
        compare(audio.volumePercent, 50);
        compare(audio.source.ready, true);
    }
}
