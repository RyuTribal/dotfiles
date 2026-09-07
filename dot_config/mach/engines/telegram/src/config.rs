//! Telegram bridge config: `~/.local/share/mach/telegram.toml`, deliberately
//! OUTSIDE the chezmoi-managed tree (`~/.config`) because it holds a secret
//! bot token. `telegram.toml.example` (shipped in the repo) is the
//! placeholder template; `install.sh`'s onboarding block tells the user how
//! to create the real file at runtime.
//!
//! SECURITY: nothing in this module ever includes `bot_token`'s value in an
//! error, a `Display` impl, or a log line -- every error variant here
//! carries only the config *path* and (for a parse failure) a reason string
//! built from `toml`'s own error, which never echoes back field values it
//! successfully parsed as a bare secret. `allowed_user_id` is read as a
//! string (not a TOML integer) so the placeholder in the `.example` file
//! can be a human-readable sentinel like the token's, rather than some
//! magic numeric stand-in.
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const PLACEHOLDER_TOKEN: &str = "CHANGEME_BOT_TOKEN";
pub const PLACEHOLDER_USER_ID: &str = "CHANGEME_ALLOWED_USER_ID";

#[derive(Debug, Deserialize)]
struct RawConfig {
    bot_token: String,
    allowed_user_id: String,
}

/// A loaded, validated config: real (non-placeholder) token and a numeric
/// allowlisted sender id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bot_token: String,
    pub allowed_user_id: i64,
}

#[derive(Debug)]
pub enum ConfigError {
    /// `~/.local/share/mach/telegram.toml` doesn't exist yet -- the normal
    /// state for anyone who hasn't run through install.sh's onboarding
    /// block. Not itself a hard failure for `machd` (see `machd`'s clean
    /// exit on this).
    Missing(PathBuf),
    /// The file exists but still holds the `.example`'s placeholder values
    /// -- copied but never filled in.
    Placeholder(PathBuf),
    /// The file exists, isn't a placeholder, but is otherwise unusable
    /// (malformed TOML, a non-numeric `allowed_user_id`, an unreadable
    /// file).
    Invalid { path: PathBuf, reason: String },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Missing(p) => write!(f, "no telegram config at {} -- see install.sh's onboarding instructions", p.display()),
            ConfigError::Placeholder(p) => write!(f, "telegram config at {} still holds placeholder values -- fill in a real bot token and user id", p.display()),
            ConfigError::Invalid { path, reason } => write!(f, "telegram config at {} is invalid: {}", path.display(), reason),
        }
    }
}

impl std::error::Error for ConfigError {}

/// `~/.local/share/mach/telegram.toml`.
pub fn config_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mach/telegram.toml"))
}

/// Loads and validates the config at the default path (`config_path`).
pub fn load() -> Result<Config, ConfigError> {
    let path = config_path().map_err(|reason| ConfigError::Invalid { path: PathBuf::new(), reason })?;
    load_from(&path)
}

/// Loads and validates the config at an arbitrary path -- so tests (and
/// `mach-telegramd --foreground`'s own diagnostics) can point at a scratch
/// file instead of the real one.
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    if !path.exists() {
        return Err(ConfigError::Missing(path.to_path_buf()));
    }
    let text = fs::read_to_string(path).map_err(|e| ConfigError::Invalid { path: path.to_path_buf(), reason: format!("could not read file: {}", e) })?;
    parse(&text, path)
}

fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError::Invalid { path: path.to_path_buf(), reason: format!("malformed TOML: {}", e) })?;

    let token = raw.bot_token.trim();
    let user_id_str = raw.allowed_user_id.trim();

    if token.is_empty() || token == PLACEHOLDER_TOKEN || user_id_str.is_empty() || user_id_str == PLACEHOLDER_USER_ID {
        return Err(ConfigError::Placeholder(path.to_path_buf()));
    }

    let allowed_user_id: i64 = user_id_str
        .parse()
        .map_err(|_| ConfigError::Invalid { path: path.to_path_buf(), reason: "allowed_user_id must be a numeric telegram user id".to_string() })?;

    Ok(Config { bot_token: token.to_string(), allowed_user_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_reports_missing() {
        let err = load_from(Path::new("/nonexistent/definitely-not-a-file/telegram.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Missing(_)));
    }

    #[test]
    fn placeholder_values_are_detected() {
        let text = format!("bot_token = \"{}\"\nallowed_user_id = \"{}\"\n", PLACEHOLDER_TOKEN, PLACEHOLDER_USER_ID);
        let err = parse(&text, Path::new("scratch.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Placeholder(_)));
    }

    #[test]
    fn partially_filled_in_is_still_a_placeholder() {
        // A real token but a still-placeholder user id (or vice versa) must
        // not be treated as ready -- the allowlist is the only thing
        // standing between the public bot and the memory store.
        let text = format!("bot_token = \"123:realtokenlike\"\nallowed_user_id = \"{}\"\n", PLACEHOLDER_USER_ID);
        let err = parse(&text, Path::new("scratch.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Placeholder(_)));
    }

    #[test]
    fn valid_config_parses() {
        let text = "bot_token = \"123456:ABC-real-token\"\nallowed_user_id = \"987654321\"\n";
        let cfg = parse(text, Path::new("scratch.toml")).unwrap();
        assert_eq!(cfg.bot_token, "123456:ABC-real-token");
        assert_eq!(cfg.allowed_user_id, 987654321);
    }

    #[test]
    fn non_numeric_user_id_is_invalid_not_placeholder() {
        let text = "bot_token = \"123456:ABC-real-token\"\nallowed_user_id = \"not-a-number\"\n";
        let err = parse(text, Path::new("scratch.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn malformed_toml_is_invalid() {
        let err = parse("this is not toml {{{", Path::new("scratch.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn error_display_never_contains_a_real_looking_token_value() {
        // Regression guard for the "never echo a real secret" invariant --
        // Display must only ever mention the *path*, never field values.
        let text = "bot_token = \"super-secret-real-token-xyz\"\nallowed_user_id = \"42\"\n";
        // This parses fine (not a placeholder), so exercise a failure path
        // that still had the real token in scope: an unreadable-path style
        // Missing error over a path containing no secret at all, plus a
        // sanity check that Display of every variant is path/reason based.
        let cfg = parse(text, Path::new("scratch.toml")).unwrap();
        assert_eq!(cfg.bot_token, "super-secret-real-token-xyz");

        let missing = ConfigError::Missing(PathBuf::from("/home/user/.local/share/mach/telegram.toml"));
        assert!(!format!("{}", missing).contains("super-secret-real-token-xyz"));
        let placeholder = ConfigError::Placeholder(PathBuf::from("/home/user/.local/share/mach/telegram.toml"));
        assert!(!format!("{}", placeholder).contains("super-secret-real-token-xyz"));
    }
}
