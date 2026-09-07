//! Telegram Bot HTTP API client, behind the `TelegramApi` trait so `bot`'s
//! dispatch/allowlist/reply-formatting logic is unit-testable against a
//! fake instead of ever making a real network call -- mirrors
//! `kb::classify::Classifier` / `kb::reflect::ReflectLlm`'s shape.
//! `HttpTelegramApi` is the real implementation, built on `ureq` (already a
//! workspace dependency via `kb-engine`'s ollama client).
//!
//! SECURITY: every error path here goes through `redact`, which strips the
//! literal bot token out of whatever `ureq`/`io` handed back before it's
//! ever returned, logged, or replied with -- `ureq::Error`'s `Display`
//! includes the request URL, which for this API always embeds the token
//! (`https://api.telegram.org/bot<TOKEN>/...`), so this is not optional.
use std::io::Read as _;
use std::time::Duration;

use serde::Deserialize;

/// Telegram enforces this itself for bot API downloads; checked here too so
/// a lying/misbehaving response doesn't buffer an unbounded amount of data
/// before the size is noticed.
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Audio {
    pub file_id: String,
    #[serde(default)]
    pub file_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Message {
    pub message_id: i64,
    #[serde(default)]
    pub from: Option<User>,
    pub chat: Chat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub audio: Option<Audio>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
}

impl Default for Chat {
    fn default() -> Self {
        Chat { id: 0 }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    #[serde(default)]
    result: Option<T>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileInfo {
    #[serde(default)]
    file_path: Option<String>,
    #[serde(default)]
    file_size: Option<u64>,
}

pub trait TelegramApi {
    /// Long-polls `getUpdates`. `timeout_secs` is passed straight through as
    /// Telegram's own long-poll `timeout` parameter (the HTTP client's own
    /// timeout is set somewhat higher than this so the poll itself has room
    /// to legitimately take that long).
    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, String>;
    /// Resolves a `file_id` to Telegram's own `file_path`, and reports the
    /// server-declared size if any -- callers can reject on size before
    /// ever downloading.
    fn get_file(&self, file_id: &str) -> Result<(String, Option<u64>), String>;
    /// Downloads a previously-resolved `file_path`'s bytes, capped at
    /// `MAX_FILE_BYTES` regardless of what any header claims.
    fn download_file(&self, file_path: &str) -> Result<Vec<u8>, String>;
    fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String>;
}

pub struct HttpTelegramApi {
    token: String,
}

impl HttpTelegramApi {
    pub fn new(token: String) -> Self {
        HttpTelegramApi { token }
    }

    fn api_base(&self) -> String {
        format!("https://api.telegram.org/bot{}", self.token)
    }

    fn file_base(&self) -> String {
        format!("https://api.telegram.org/file/bot{}", self.token)
    }

    /// Strips every occurrence of the literal token out of an error string
    /// before it's ever allowed to propagate -- see the module doc comment.
    fn redact(&self, msg: impl std::fmt::Display) -> String {
        let s = msg.to_string();
        if self.token.is_empty() {
            s
        } else {
            s.replace(&self.token, "<redacted>")
        }
    }
}

impl TelegramApi for HttpTelegramApi {
    fn get_updates(&self, offset: i64, timeout_secs: u64) -> Result<Vec<Update>, String> {
        let url = format!("{}/getUpdates", self.api_base());
        let resp = ureq::get(&url)
            .query("offset", &offset.to_string())
            .query("timeout", &timeout_secs.to_string())
            .timeout(Duration::from_secs(timeout_secs + 10))
            .call()
            .map_err(|e| self.redact(e))?;
        let parsed: ApiResponse<Vec<Update>> = resp.into_json().map_err(|e| self.redact(e))?;
        if !parsed.ok {
            return Err(format!("getUpdates: {}", parsed.description.unwrap_or_else(|| "ok=false".to_string())));
        }
        Ok(parsed.result.unwrap_or_default())
    }

    fn get_file(&self, file_id: &str) -> Result<(String, Option<u64>), String> {
        let url = format!("{}/getFile", self.api_base());
        let resp = ureq::get(&url).query("file_id", file_id).timeout(Duration::from_secs(30)).call().map_err(|e| self.redact(e))?;
        let parsed: ApiResponse<FileInfo> = resp.into_json().map_err(|e| self.redact(e))?;
        if !parsed.ok {
            return Err(format!("getFile: {}", parsed.description.unwrap_or_else(|| "ok=false".to_string())));
        }
        let info = parsed.result.ok_or_else(|| "getFile: no result".to_string())?;
        let path = info.file_path.ok_or_else(|| "getFile: no file_path in response".to_string())?;
        Ok((path, info.file_size))
    }

    fn download_file(&self, file_path: &str) -> Result<Vec<u8>, String> {
        let url = format!("{}/{}", self.file_base(), file_path);
        let resp = ureq::get(&url).timeout(Duration::from_secs(120)).call().map_err(|e| self.redact(e))?;
        if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) {
            if len > MAX_FILE_BYTES {
                return Err(format!("file too large ({} bytes, cap {})", len, MAX_FILE_BYTES));
            }
        }
        let mut buf = Vec::new();
        resp.into_reader().take(MAX_FILE_BYTES + 1).read_to_end(&mut buf).map_err(|e| self.redact(e))?;
        if buf.len() as u64 > MAX_FILE_BYTES {
            return Err(format!("file too large (>= {} bytes, cap {})", buf.len(), MAX_FILE_BYTES));
        }
        Ok(buf)
    }

    fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String> {
        let url = format!("{}/sendMessage", self.api_base());
        ureq::post(&url)
            .timeout(Duration::from_secs(30))
            .send_json(serde_json::json!({ "chat_id": chat_id, "text": text }))
            .map_err(|e| self.redact(e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_strips_the_token_from_any_error_string() {
        let api = HttpTelegramApi::new("123456:SUPER-SECRET-TOKEN".to_string());
        let msg = "error hitting https://api.telegram.org/bot123456:SUPER-SECRET-TOKEN/getUpdates: connection refused";
        let redacted = api.redact(msg);
        assert!(!redacted.contains("SUPER-SECRET-TOKEN"));
        assert!(redacted.contains("<redacted>"));
    }

    #[test]
    fn redact_is_a_noop_on_a_message_without_the_token() {
        let api = HttpTelegramApi::new("123456:SUPER-SECRET-TOKEN".to_string());
        assert_eq!(api.redact("plain message"), "plain message");
    }

    #[test]
    fn api_base_and_file_base_embed_the_token_as_expected_by_telegrams_own_url_scheme() {
        // Not a network test -- just locking in the URL shape so a future
        // edit doesn't silently drop the "bot"/"file/bot" prefix Telegram
        // requires.
        let api = HttpTelegramApi::new("T".to_string());
        assert_eq!(api.api_base(), "https://api.telegram.org/botT");
        assert_eq!(api.file_base(), "https://api.telegram.org/file/botT");
    }

    #[test]
    fn update_deserializes_a_text_message() {
        let json = r#"{"update_id": 5, "message": {"message_id": 1, "from": {"id": 42}, "chat": {"id": 42}, "text": "hello"}}"#;
        let u: Update = serde_json::from_str(json).unwrap();
        assert_eq!(u.update_id, 5);
        let m = u.message.unwrap();
        assert_eq!(m.from.unwrap().id, 42);
        assert_eq!(m.text.as_deref(), Some("hello"));
        assert!(m.voice.is_none());
        assert!(m.photo.is_none());
    }

    #[test]
    fn update_deserializes_a_voice_message() {
        let json = r#"{"update_id": 6, "message": {"message_id": 2, "from": {"id": 42}, "chat": {"id": 42}, "voice": {"file_id": "abc", "file_size": 1234}}}"#;
        let u: Update = serde_json::from_str(json).unwrap();
        let m = u.message.unwrap();
        assert_eq!(m.voice.unwrap().file_id, "abc");
    }

    #[test]
    fn update_deserializes_a_photo_message_with_caption() {
        let json = r#"{"update_id": 7, "message": {"message_id": 3, "from": {"id": 42}, "chat": {"id": 42}, "caption": "my desk", "photo": [{"file_id": "small", "width": 90, "height": 90}, {"file_id": "large", "width": 800, "height": 600, "file_size": 9999}]}}"#;
        let u: Update = serde_json::from_str(json).unwrap();
        let m = u.message.unwrap();
        assert_eq!(m.caption.as_deref(), Some("my desk"));
        let photos = m.photo.unwrap();
        assert_eq!(photos.len(), 2);
        assert_eq!(photos[1].file_id, "large");
    }

    #[test]
    fn update_without_a_message_deserializes_fine() {
        let json = r#"{"update_id": 8}"#;
        let u: Update = serde_json::from_str(json).unwrap();
        assert!(u.message.is_none());
    }
}
