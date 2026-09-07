//! Long-poll offset persisted to `~/.local/share/mach/telegram-state.json`,
//! so a `machd` restart (crash, `systemctl restart`, a laptop reboot) never
//! re-delivers updates the bot already processed. Telegram's own
//! `getUpdates` semantics: passing `offset = last_update_id + 1` both fetches
//! only newer updates and acks (drops server-side) everything before it, so
//! persisting "the next offset to request" here is exactly what's needed --
//! never the raw `update_id` of the last-seen update itself.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub offset: i64,
}

/// `~/.local/share/mach/telegram-state.json`.
pub fn state_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mach/telegram-state.json"))
}

/// Loads persisted state from `path`. Missing file, unreadable file, or
/// malformed JSON all fall back to the zero-value default (offset 0, i.e.
/// "start from whatever Telegram currently has buffered") -- state loss here
/// costs at most a handful of re-delivered updates on the very first poll
/// after the file goes missing/corrupt, never a crash.
pub fn load(path: &Path) -> State {
    fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Persists `state` to `path`, creating parent directories as needed.
pub fn save(path: &Path, state: State) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let json = serde_json::to_string(&state).map_err(io::Error::other)?;
    fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("mach-telegram-state-test-{}-{}.json", std::process::id(), name));
        p
    }

    #[test]
    fn load_of_missing_file_defaults_to_zero_offset() {
        let path = scratch_path("missing");
        let _ = fs::remove_file(&path);
        assert_eq!(load(&path), State { offset: 0 });
    }

    #[test]
    fn load_of_malformed_json_defaults_to_zero_offset() {
        let path = scratch_path("malformed");
        fs::write(&path, b"not json at all").unwrap();
        assert_eq!(load(&path), State { offset: 0 });
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let path = scratch_path("roundtrip");
        let _ = fs::remove_file(&path);
        save(&path, State { offset: 4242 }).unwrap();
        assert_eq!(load(&path), State { offset: 4242 });
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_creates_parent_directories() {
        let mut path = std::env::temp_dir();
        path.push(format!("mach-telegram-state-test-{}-nested-dir", std::process::id()));
        let path = path.join("deep").join("telegram-state.json");
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        save(&path, State { offset: 7 }).unwrap();
        assert_eq!(load(&path), State { offset: 7 });
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}
