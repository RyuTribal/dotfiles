//! meet -- phase A (capture only) of the meeting recorder. `mach meet
//! start`/`stop`/`status` produce clean dual-track (mic + system-audio
//! monitor) or solo-mic WAV recordings plus a `meeting.json` sidecar under
//! `~/.local/share/mach/meetings/<dir>/`. Transcription/summarization is
//! phase B and lives elsewhere -- nothing in this crate reads the audio it
//! produces, only records it and reports where it landed.
pub mod active;
pub mod capture;
pub mod cli;
pub mod disk;
pub mod meeting;
pub mod time;
