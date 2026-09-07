//! meet -- the meeting recorder. Phase A (`mach meet start`/`stop`/
//! `status`) produces clean dual-track (mic + system-audio monitor) or
//! solo-mic WAV recordings plus a `meeting.json` sidecar under
//! `~/.local/share/mach/meetings/<dir>/`, marking each finished recording
//! `needs-processing`. Phase B (`mach meet process`) picks those up:
//! whisper.cpp transcription with per-segment timestamps (`transcribe`,
//! `segment`), one sonnet call to summarize/label/extract durable facts
//! (`summarize`), and the `needs-processing` -> `needs-summary` ->
//! `processed` marker state machine that lets the claude step defer
//! independently of transcription when offline (`markers`, `process`).
pub mod active;
pub mod capture;
pub mod cli;
pub mod disk;
pub mod markers;
pub mod meeting;
pub mod process;
pub mod segment;
pub mod summarize;
pub mod time;
pub mod transcribe;
