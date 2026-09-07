//! `Meeting` -- the `meeting.json` schema written into each
//! `~/.local/share/mach/meetings/<dir>/` directory, plus the directory-name
//! and slug conventions used to create that directory in the first place.
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Recording mode: `Dual` captures `mic.wav` + `system.wav`; `Solo`
/// captures `mic.wav` only. Serialized lowercase (`"dual"`/`"solo"`) --
/// `meeting.json` is meant to be readable by phase B without needing this
/// crate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Dual,
    Solo,
}

impl Mode {
    /// Track names this mode records, in the order `mach meet start`
    /// spawns them and `mach meet stop`/`status` report them.
    pub fn track_names(self) -> &'static [&'static str] {
        match self {
            Mode::Dual => &["mic", "system"],
            Mode::Solo => &["mic"],
        }
    }

    pub fn file_for(track_name: &str) -> String {
        format!("{}.wav", track_name)
    }
}

/// Recorder pids, keyed by track -- `system` is only ever `Some` in `Dual`
/// mode. Kept as its own struct (rather than a `Vec`) so `meeting.json`
/// stays self-describing without a track-name lookup.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Pids {
    pub mic: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<u32>,
}

impl Pids {
    /// Every recorded pid, mic first -- the order `mach meet stop` signals
    /// them in and the `.active` pidfile stores them in.
    pub fn all(&self) -> Vec<u32> {
        let mut v = vec![self.mic];
        if let Some(s) = self.system {
            v.push(s);
        }
        v
    }
}

/// One track's on-disk outcome, filled in by `mach meet stop` (empty at
/// `start` time -- a track isn't "done" until the recorder has been
/// stopped and its final size read).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackInfo {
    pub name: String,
    pub file: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meeting {
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub sample_rate: u32,
    /// `"pw-record"` or `"parecord"` -- whichever `capture::detect_binary`
    /// found, recorded so phase B (or a human debugging a bad recording)
    /// knows which tool produced it without re-detecting anything.
    pub capture_binary: String,
    pub pids: Pids,
    /// Empty until `mach meet stop` fills it in.
    #[serde(default)]
    pub tracks: Vec<TrackInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
}

impl Meeting {
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    pub fn path(dir: &Path) -> std::path::PathBuf {
        dir.join("meeting.json")
    }

    pub fn write(&self, dir: &Path) -> io::Result<()> {
        let json = self.to_json().map_err(io::Error::other)?;
        fs::write(Self::path(dir), json)
    }

    pub fn read(dir: &Path) -> io::Result<Self> {
        let s = fs::read_to_string(Self::path(dir))?;
        Self::from_json(&s).map_err(io::Error::other)
    }
}

/// Kebab-cases a title for use in a meeting directory name: lowercase,
/// ASCII alphanumerics kept, every run of anything else collapsed to a
/// single `-`, no leading/trailing `-`. Same convention as
/// `kb::note::slugify`, duplicated here (a handful of lines) rather than
/// pulled in as a cross-engine dependency -- unlike that function, an
/// empty/punctuation-only input returns an empty string (not a fallback
/// word): `dir_name` below treats that as "no title given" and omits the
/// slug suffix entirely rather than appending a meaningless one.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true; // suppresses a leading dash
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Builds `<YYYY-MM-DD-HHMM>[-title-slug]`, the meeting directory's base
/// name (before any `disambiguate` collision suffix). `title` is the raw
/// user-supplied `--title` text, if any; a title that slugifies to nothing
/// (empty or punctuation-only) is treated the same as no title.
pub fn dir_name(started_at_secs: u64, title: Option<&str>) -> String {
    let ts = crate::time::dir_timestamp_from_secs(started_at_secs);
    match title.map(slugify).filter(|s| !s.is_empty()) {
        Some(slug) => format!("{}-{}", ts, slug),
        None => ts,
    }
}

/// Appends `-2`, `-3`, ... to `base` until `exists(candidate)` is false --
/// guards the vanishingly unlikely case of two meetings landing on the same
/// minute-resolution directory name (e.g. a crash-recovered stale meeting
/// immediately followed by a fresh start). Generic over `exists` so the
/// collision search is unit-testable without touching the real filesystem.
pub fn disambiguate<F: Fn(&str) -> bool>(base: &str, exists: F) -> String {
    if !exists(base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{}-{}", base, n);
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_lowercases_and_hyphenates() {
        assert_eq!(slugify("Weekly Standup"), "weekly-standup");
    }

    #[test]
    fn slugify_collapses_punctuation_and_trims_edges() {
        assert_eq!(slugify("  --Q3 Planning!! --"), "q3-planning");
    }

    #[test]
    fn slugify_of_punctuation_only_is_empty() {
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn dir_name_without_title_is_just_the_timestamp() {
        let secs = parse_test_secs("2026-09-07T17:33:00Z");
        assert_eq!(dir_name(secs, None), "2026-09-07-1733");
    }

    #[test]
    fn dir_name_with_title_appends_slug() {
        let secs = parse_test_secs("2026-09-07T17:33:00Z");
        assert_eq!(dir_name(secs, Some("Weekly Standup")), "2026-09-07-1733-weekly-standup");
    }

    #[test]
    fn dir_name_with_unslugifiable_title_omits_suffix() {
        let secs = parse_test_secs("2026-09-07T17:33:00Z");
        assert_eq!(dir_name(secs, Some("!!!")), "2026-09-07-1733");
        assert_eq!(dir_name(secs, Some("")), "2026-09-07-1733");
    }

    fn parse_test_secs(s: &str) -> u64 {
        crate::time::parse_rfc3339(s).unwrap() as u64
    }

    #[test]
    fn disambiguate_returns_base_when_free() {
        assert_eq!(disambiguate("2026-09-07-1733", |_| false), "2026-09-07-1733");
    }

    #[test]
    fn disambiguate_finds_the_first_free_numbered_suffix() {
        let taken = ["2026-09-07-1733", "2026-09-07-1733-2", "2026-09-07-1733-3"];
        let exists = |c: &str| taken.contains(&c);
        assert_eq!(disambiguate("2026-09-07-1733", exists), "2026-09-07-1733-4");
    }

    #[test]
    fn meeting_json_roundtrips_dual_mode_with_all_fields() {
        let m = Meeting {
            started_at: "2026-09-07T17:33:00Z".to_string(),
            ended_at: Some("2026-09-07T18:03:00Z".to_string()),
            mode: Mode::Dual,
            title: Some("Weekly Standup".to_string()),
            sample_rate: 48000,
            capture_binary: "pw-record".to_string(),
            pids: Pids { mic: 111, system: Some(112) },
            tracks: vec![
                TrackInfo { name: "mic".to_string(), file: "mic.wav".to_string(), bytes: 12345 },
                TrackInfo { name: "system".to_string(), file: "system.wav".to_string(), bytes: 67890 },
            ],
            duration_secs: Some(1800),
        };
        let json = m.to_json().unwrap();
        let back = Meeting::from_json(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn meeting_json_roundtrips_solo_mode_with_minimal_fields() {
        let m = Meeting {
            started_at: "2026-09-07T17:33:00Z".to_string(),
            ended_at: None,
            mode: Mode::Solo,
            title: None,
            sample_rate: 48000,
            capture_binary: "parecord".to_string(),
            pids: Pids { mic: 111, system: None },
            tracks: vec![],
            duration_secs: None,
        };
        let json = m.to_json().unwrap();
        let back = Meeting::from_json(&json).unwrap();
        assert_eq!(m, back);
        assert!(!json.contains("ended_at")); // skip_serializing_if actually elides it
        assert!(!json.contains("system"));
    }

    #[test]
    fn pids_all_includes_system_only_when_present() {
        assert_eq!(Pids { mic: 1, system: None }.all(), vec![1]);
        assert_eq!(Pids { mic: 1, system: Some(2) }.all(), vec![1, 2]);
    }

    #[test]
    fn mode_track_names() {
        assert_eq!(Mode::Dual.track_names(), &["mic", "system"]);
        assert_eq!(Mode::Solo.track_names(), &["mic"]);
    }
}
