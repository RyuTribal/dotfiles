//! whisper.cpp transcription for `mach meet process`, with segment-level
//! timestamps -- unlike `telegram::voice::Transcriber` (a single flattened
//! string, right for a short voice note), phase B needs each segment's
//! start offset to interleave the mic/system tracks into `transcript.md`.
//!
//! Reuses `telegram::voice::{find_binary, ensure_model}` verbatim for
//! locating/downloading the whisper.cpp binary and model -- no reason to
//! duplicate that (identical degradation rules: missing binary or model
//! download failure is a plain `Err`, never a panic) -- and reuses its
//! `Converter` trait for the same reason `pipeline.rs` keeps conversion and
//! transcription as two separate steps: testable independently, and a
//! meeting's own recorder captures 48kHz (`capture::SAMPLE_RATE`), which
//! whisper.cpp still wants downsampled to 16kHz mono first.
//!
//! `DownsampleConverter` is its own (small) `Converter` impl rather than
//! `telegram::voice::FfmpegConverter`: that type's `convert_to_wav` always
//! writes to `input.with_extension("wav")`, which for an already-`.wav`
//! meeting track (`mic.wav`, `system.wav`) is the *same path* as the input
//! -- ffmpeg reading and truncating-to-write the same file is not a
//! well-defined operation. `DownsampleConverter` runs the identical ffmpeg
//! invocation but writes to a distinct sibling path instead.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use telegram::voice::Converter;

use crate::segment::Segment;

pub use telegram::voice::{ensure_model, find_binary};

/// Full meeting audio is much longer than the single voice note
/// `telegram::voice`'s own 120s budget is sized for -- a solid hour of talk
/// on a CPU-bound `base` model can run well past that. Generous rather than
/// tight: a `mach meet process` invocation runs detached in the background
/// (see `cli::cmd_stop`), so nothing is waiting on it interactively.
pub const WHISPER_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub trait SegmentTranscriber {
    fn transcribe_segments(&self, wav_path: &Path) -> Result<Vec<Segment>, String>;
}

/// Shells out to `ffmpeg -i <input> -ar 16000 -ac 1 <input's dir>/<stem>.16k.wav`.
pub struct DownsampleConverter;

impl Converter for DownsampleConverter {
    fn convert(&self, input: &Path) -> Result<PathBuf, String> {
        convert_with_bin("ffmpeg", input)
    }
}

fn convert_with_bin(ffmpeg_bin: &str, input: &Path) -> Result<PathBuf, String> {
    let out = sixteen_khz_sibling(input);
    let status = Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg(&out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("failed to spawn '{}': {}", ffmpeg_bin, e))?;
    if !status.success() {
        return Err(format!("'{}' exited with {:?} downsampling {} to 16kHz mono", ffmpeg_bin, status.code(), input.display()));
    }
    Ok(out)
}

/// `mic.wav` -> `mic.16k.wav`, same directory.
fn sixteen_khz_sibling(input: &Path) -> PathBuf {
    let stem = input.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "audio".to_string());
    input.with_file_name(format!("{}.16k.wav", stem))
}

/// `whisper-cli -m <model> -f <wav> -oj -of <wav-without-ext> -np`, reading
/// back `<wav-without-ext>.json` -- whisper.cpp's own JSON output schema
/// (`{"transcription": [{"offsets": {"from": <ms>, "to": <ms>}, "text":
/// "..."}, ...]}`). The JSON file is a transcription artifact, not
/// something worth keeping alongside the meeting's actual output
/// (`transcript.md`), so it's removed again once parsed -- best-effort,
/// same as every other cleanup in this crate.
pub struct WhisperCliSegmentTranscriber {
    binary: String,
    model: PathBuf,
}

impl WhisperCliSegmentTranscriber {
    pub fn new(binary: String, model: PathBuf) -> Self {
        WhisperCliSegmentTranscriber { binary, model }
    }
}

/// `mic.16k.wav` -> (`-of` value `mic.16k`, JSON output path `mic.16k.json`).
/// NOT `out_stem.with_extension("json")`: `out_stem`'s own file name
/// (`mic.16k`, from the `DownsampleConverter` sibling this is always called
/// on) already contains a dot itself -- `Path`'s extension parsing only
/// ever looks at the LAST dot in a file name, so `with_extension` here
/// would see `out_stem`'s "16k" as its "extension" and replace it outright,
/// silently producing `mic.json` instead of `mic.16k.json` (whisper.cpp's
/// actual `-of <stem>` convention is a plain string append, not
/// path-aware). Caught live: whisper-cli wrote `mic.16k.json`, and the code
/// this replaced went looking for `mic.json` and failed to find it.
fn whisper_output_paths(wav_path: &Path) -> (PathBuf, PathBuf) {
    let out_stem = wav_path.with_extension("");
    let json_path = PathBuf::from(format!("{}.json", out_stem.display()));
    (out_stem, json_path)
}

impl SegmentTranscriber for WhisperCliSegmentTranscriber {
    fn transcribe_segments(&self, wav_path: &Path) -> Result<Vec<Segment>, String> {
        let (out_stem, json_path) = whisper_output_paths(wav_path);

        let mut child = Command::new(&self.binary)
            .arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(wav_path)
            .arg("-oj")
            .arg("-of")
            .arg(&out_stem)
            .arg("-np")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn '{}': {}", self.binary, e))?;

        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if start.elapsed() >= WHISPER_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!("'{}' timed out after {:?}", self.binary, WHISPER_TIMEOUT));
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => return Err(format!("error waiting on '{}': {}", self.binary, e)),
            }
        };
        if !status.success() {
            return Err(format!("'{}' exited with {:?} transcribing {}", self.binary, status.code(), wav_path.display()));
        }

        let mut raw = String::new();
        std::fs::File::open(&json_path)
            .map_err(|e| format!("could not open whisper output {}: {}", json_path.display(), e))?
            .read_to_string(&mut raw)
            .map_err(|e| format!("could not read whisper output {}: {}", json_path.display(), e))?;
        let _ = std::fs::remove_file(&json_path);

        parse_whisper_json(&raw)
    }
}

#[derive(Deserialize)]
struct WhisperJson {
    transcription: Vec<WhisperSegmentJson>,
}

#[derive(Deserialize)]
struct WhisperSegmentJson {
    offsets: WhisperOffsets,
    text: String,
}

#[derive(Deserialize)]
struct WhisperOffsets {
    from: u64,
}

/// Parses whisper.cpp's `-oj` JSON output into `Segment`s -- pure, so it's
/// unit-testable against a fixture string without a real whisper.cpp
/// binary. An empty `transcription` array (silence, or a track with no
/// speech) is not an error -- it yields an empty segment list, same as a
/// silent stretch of a real meeting would.
pub fn parse_whisper_json(raw: &str) -> Result<Vec<Segment>, String> {
    let parsed: WhisperJson = serde_json::from_str(raw).map_err(|e| format!("could not parse whisper JSON output: {}", e))?;
    Ok(parsed
        .transcription
        .into_iter()
        .map(|s| Segment { start_ms: s.offsets.from, text: s.text })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_whisper_json_extracts_offsets_and_text() {
        let raw = r#"{
            "transcription": [
                {"timestamps": {"from": "00:00:00,000", "to": "00:00:02,220"}, "offsets": {"from": 0, "to": 2220}, "text": " Hello there."},
                {"timestamps": {"from": "00:00:02,220", "to": "00:00:05,000"}, "offsets": {"from": 2220, "to": 5000}, "text": " How are you?"}
            ]
        }"#;
        let segs = parse_whisper_json(raw).unwrap();
        assert_eq!(segs, vec![
            Segment { start_ms: 0, text: " Hello there.".to_string() },
            Segment { start_ms: 2220, text: " How are you?".to_string() },
        ]);
    }

    #[test]
    fn parse_whisper_json_empty_transcription_is_not_an_error() {
        let raw = r#"{"transcription": []}"#;
        assert_eq!(parse_whisper_json(raw).unwrap(), vec![]);
    }

    #[test]
    fn parse_whisper_json_rejects_malformed_input() {
        assert!(parse_whisper_json("not json at all").is_err());
        assert!(parse_whisper_json(r#"{"nothing_useful": true}"#).is_err());
    }

    #[test]
    fn whisper_output_paths_handles_a_stem_with_an_embedded_dot() {
        // Regression test for a bug caught by live verification: `mic.16k`
        // (the downsampled sibling's stem) contains its own dot, which a
        // naive `.with_extension("json")` on it would misparse as an
        // "extension" to replace rather than a literal suffix to append.
        let (out_stem, json_path) = whisper_output_paths(Path::new("/tmp/meeting/mic.16k.wav"));
        assert_eq!(out_stem, Path::new("/tmp/meeting/mic.16k"));
        assert_eq!(json_path, Path::new("/tmp/meeting/mic.16k.json"));
    }

    #[test]
    fn whisper_output_paths_handles_a_plain_single_dot_stem_too() {
        let (out_stem, json_path) = whisper_output_paths(Path::new("/tmp/meeting/mic.wav"));
        assert_eq!(out_stem, Path::new("/tmp/meeting/mic"));
        assert_eq!(json_path, Path::new("/tmp/meeting/mic.json"));
    }

    #[test]
    fn sixteen_khz_sibling_does_not_collide_with_the_input_path() {
        let input = Path::new("/tmp/meeting/mic.wav");
        let out = sixteen_khz_sibling(input);
        assert_ne!(out, input);
        assert_eq!(out, Path::new("/tmp/meeting/mic.16k.wav"));
    }

    #[test]
    fn convert_reports_a_clear_error_on_a_missing_ffmpeg() {
        let err = convert_with_bin("/nonexistent/definitely-not-ffmpeg-xyz", Path::new("/nonexistent/mic.wav")).unwrap_err();
        assert!(err.contains("failed to spawn"));
    }

    #[test]
    fn whisper_segment_transcriber_reports_a_clear_error_on_a_missing_binary() {
        let t = WhisperCliSegmentTranscriber::new("/nonexistent/definitely-not-whisper-xyz".to_string(), PathBuf::from("/nonexistent/model.bin"));
        let err = t.transcribe_segments(Path::new("/nonexistent/audio.wav")).unwrap_err();
        assert!(err.contains("failed to spawn"));
    }
}
