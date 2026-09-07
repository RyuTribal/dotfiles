//! whisper.cpp integration for transcribing voice/audio notes.
//!
//! Telegram voice messages arrive as OGG/Opus (audio messages can be
//! mp3/m4a/etc.), which whisper.cpp's CLI does not read directly -- it
//! wants 16kHz mono WAV. `convert_to_wav` shells out to `ffmpeg` first;
//! `WhisperCliTranscriber` then shells out to the whisper.cpp CLI. Neither
//! binary is a hard dependency of this crate: per the user's package
//! install discipline (pacman/AUR first, nothing pip/npm'd by tooling here),
//! `install.sh` only ever checks for and reports on these binaries -- it
//! never installs them itself -- and every failure in this module (missing
//! binary, conversion failure, transcription failure, timeout) degrades to
//! a plain `Err(String)` the caller turns into "voice note (untranscribed)"
//! rather than losing the note.
//!
//! Behind the `Transcriber` trait (mirrors `kb::note::NoteLlm` /
//! `TelegramApi`) so the degradation path is unit-testable without a real
//! whisper.cpp binary or model file.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Multilingual base model (~142MB) -- see install.sh's size-warning echo.
pub const MODEL_NAME: &str = "ggml-base.bin";
pub const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

pub trait Transcriber {
    fn transcribe(&self, audio_path: &Path) -> Result<String, String>;
}

/// The wav-conversion step, behind a trait for the same reason `Transcriber`
/// is: so `pipeline`'s "transcriber configured but the chain still fails"
/// degradation path is testable without a real `ffmpeg` binary.
pub trait Converter {
    fn convert(&self, input: &Path) -> Result<PathBuf, String>;
}

/// Shells out to `ffmpeg`, mirroring `convert_to_wav`.
pub struct FfmpegConverter;

impl Converter for FfmpegConverter {
    fn convert(&self, input: &Path) -> Result<PathBuf, String> {
        convert_to_wav(input)
    }
}

/// Locates a whisper.cpp CLI binary on `PATH` -- `whisper-cli` (current
/// upstream binary name) preferred, `whisper-cpp` / legacy `main`-renamed
/// `whisper` as fallbacks some distro/AUR packages still ship under.
pub fn find_binary() -> Option<String> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    find_binary_in(&path_env.to_string_lossy())
}

fn find_binary_in(path_env: &str) -> Option<String> {
    for name in ["whisper-cli", "whisper-cpp", "whisper"] {
        if std::env::split_paths(path_env).any(|dir| dir.join(name).is_file()) {
            return Some(name.to_string());
        }
    }
    None
}

/// `~/.local/share/mach/whisper/`, where the auto-downloaded model lives.
pub fn model_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mach/whisper"))
}

pub fn model_path() -> Result<PathBuf, String> {
    Ok(model_dir()?.join(MODEL_NAME))
}

/// Downloads the model to `model_path()` via `curl` if it isn't already
/// there. Best-effort: any failure (no curl, offline, disk full) is
/// reported as a plain error string -- the caller treats it exactly like
/// "transcription is pending tooling", never a panic.
pub fn ensure_model() -> Result<PathBuf, String> {
    let dest = model_path()?;
    if dest.exists() {
        return Ok(dest);
    }
    let dir = model_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let status = Command::new("curl")
        .arg("-fsSL")
        .arg("-o")
        .arg(&dest)
        .arg(MODEL_URL)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("could not spawn curl to download whisper model: {}", e))?;
    if !status.success() {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("curl exited with {:?} downloading the whisper model", status.code()));
    }
    Ok(dest)
}

/// Converts `input_path` to a 16kHz mono WAV alongside it (same stem, a
/// `.wav` extension) via `ffmpeg`. A missing/failing `ffmpeg` degrades like
/// every other step here: a clear error, the original downloaded audio
/// bytes untouched.
pub fn convert_to_wav(input_path: &Path) -> Result<PathBuf, String> {
    convert_to_wav_with_bin("ffmpeg", input_path)
}

fn convert_to_wav_with_bin(ffmpeg_bin: &str, input_path: &Path) -> Result<PathBuf, String> {
    let wav_path = input_path.with_extension("wav");
    let status = Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(input_path)
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg(&wav_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("failed to spawn '{}': {}", ffmpeg_bin, e))?;
    if !status.success() {
        return Err(format!("'{}' exited with {:?} converting audio to wav", ffmpeg_bin, status.code()));
    }
    Ok(wav_path)
}

const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(120);

/// Shells out to a whisper.cpp CLI binary: `<bin> -m <model> -f <wav> -nt -np`
/// (`-nt` no timestamps, `-np` no progress/debug noise), reading stdout as
/// the transcript.
pub struct WhisperCliTranscriber {
    binary: String,
    model: PathBuf,
}

impl WhisperCliTranscriber {
    pub fn new(binary: String, model: PathBuf) -> Self {
        WhisperCliTranscriber { binary, model }
    }
}

impl Transcriber for WhisperCliTranscriber {
    fn transcribe(&self, audio_path: &Path) -> Result<String, String> {
        let mut child = Command::new(&self.binary)
            .arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(audio_path)
            .arg("-nt")
            .arg("-np")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("failed to spawn '{}': {}", self.binary, e))?;

        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut out = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        use std::io::Read;
                        let _ = stdout.read_to_string(&mut out);
                    }
                    return if status.success() {
                        let text = out.trim().to_string();
                        if text.is_empty() {
                            Err("whisper produced an empty transcript".to_string())
                        } else {
                            Ok(text)
                        }
                    } else {
                        Err(format!("'{}' exited with {:?}", self.binary, status.code()))
                    };
                }
                Ok(None) => {
                    if start.elapsed() >= TRANSCRIBE_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(format!("'{}' timed out after {:?}", self.binary, TRANSCRIBE_TIMEOUT));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(format!("error waiting on '{}': {}", self.binary, e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_binary_in_locates_the_preferred_name_first() {
        let dir = std::env::temp_dir().join(format!("mach-whisper-find-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("whisper-cli"), b"#!/bin/sh\n").unwrap();
        std::fs::write(dir.join("whisper-cpp"), b"#!/bin/sh\n").unwrap();

        let found = find_binary_in(&dir.to_string_lossy());
        assert_eq!(found, Some("whisper-cli".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_in_falls_back_through_known_names() {
        let dir = std::env::temp_dir().join(format!("mach-whisper-find-test-fallback-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("whisper-cpp"), b"#!/bin/sh\n").unwrap();

        let found = find_binary_in(&dir.to_string_lossy());
        assert_eq!(found, Some("whisper-cpp".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_binary_in_returns_none_when_nothing_is_present() {
        let dir = std::env::temp_dir().join(format!("mach-whisper-find-test-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_binary_in(&dir.to_string_lossy()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_path_lands_under_the_expected_whisper_directory() {
        let path = model_path().unwrap();
        assert!(path.ends_with(".local/share/mach/whisper/ggml-base.bin"));
    }

    #[test]
    fn transcriber_reports_a_clear_error_on_a_missing_binary() {
        let t = WhisperCliTranscriber::new("/nonexistent/definitely-not-whisper-xyz".to_string(), PathBuf::from("/nonexistent/model.bin"));
        let err = t.transcribe(Path::new("/nonexistent/audio.wav")).unwrap_err();
        assert!(err.contains("failed to spawn"));
    }

    #[test]
    fn convert_to_wav_reports_a_clear_error_on_a_missing_ffmpeg() {
        let err = convert_to_wav_with_bin("/nonexistent/definitely-not-ffmpeg-xyz", Path::new("/nonexistent/audio.oga")).unwrap_err();
        assert!(err.contains("failed to spawn"));
    }
}
