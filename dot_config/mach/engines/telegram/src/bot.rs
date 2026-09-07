//! Pure dispatch logic: sender allowlisting, message classification, and
//! reply-text formatting. Kept free of any HTTP/subprocess/DB I/O so all of
//! it is unit-testable directly -- `pipeline` is what wires this to the
//! real `TelegramApi`, `kb` store, and `Transcriber`.
//!
//! SECURITY (hard sender allowlist): `is_allowed` is the ONE gate standing
//! between the public internet and the user's memory store -- every update
//! from any other sender must be dropped before any of its content ever
//! reaches the note pipeline, the whisper transcriber, or a reply. Nothing
//! in a message's TEXT is ever interpreted as a command to this process
//! either: `/start` and `/status` are recognized structurally (exact
//! command tokens, matched in `classify_message` below, allowlisted-sender
//! only) -- everything else, however imperative-sounding, is note DATA that
//! flows into the classification pipeline as content, never as an
//! instruction to `mach`, the shell, or this bot's own config.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kb::note::Worth;

use crate::api::Message;

/// The one allowlist check. `sender_id` is `None` for a message with no
/// `from` field at all (Telegram sends this for channel posts, which this
/// bridge has no business processing) -- that also fails the check.
pub fn is_allowed(sender_id: Option<i64>, allowed_user_id: i64) -> bool {
    sender_id == Some(allowed_user_id)
}

/// What one allowlisted message carries, decided once so callers don't
/// re-inspect `Message`'s optional fields themselves. `Photo` always
/// resolves to the largest available size (Telegram sends several).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageKind {
    StartCommand,
    StatusCommand,
    Text(String),
    Voice { file_id: String, file_size: Option<u64> },
    Audio { file_id: String, file_size: Option<u64> },
    Photo { file_id: String, caption: String },
    /// No text, no voice/audio, no photo (or an empty/whitespace-only
    /// text) -- nothing to do with this message.
    Empty,
}

pub fn classify_message(msg: &Message) -> MessageKind {
    if let Some(text) = &msg.text {
        let t = text.trim();
        if is_command(t, "/start") {
            return MessageKind::StartCommand;
        }
        if is_command(t, "/status") {
            return MessageKind::StatusCommand;
        }
        if !t.is_empty() {
            return MessageKind::Text(t.to_string());
        }
    }
    if let Some(voice) = &msg.voice {
        return MessageKind::Voice { file_id: voice.file_id.clone(), file_size: voice.file_size };
    }
    if let Some(audio) = &msg.audio {
        return MessageKind::Audio { file_id: audio.file_id.clone(), file_size: audio.file_size };
    }
    if let Some(photos) = &msg.photo {
        if let Some(largest) = photos.iter().max_by_key(|p| p.width.saturating_mul(p.height)) {
            return MessageKind::Photo { file_id: largest.file_id.clone(), caption: msg.caption.clone().unwrap_or_default() };
        }
    }
    MessageKind::Empty
}

/// Matches an exact bot command token, tolerating the `@botusername` suffix
/// Telegram appends in group chats (irrelevant here -- this bridge is a
/// private one-user bot -- but cheap to handle correctly) and any trailing
/// argument text.
fn is_command(text: &str, command: &str) -> bool {
    let first_word = text.split_whitespace().next().unwrap_or("");
    first_word == command || first_word.starts_with(&format!("{}@", command))
}

/// A running count of updates dropped for failing the sender allowlist.
/// Only the COUNT is ever logged (see `mach-telegramd`'s poll loop) -- never
/// the sender id or any message content, per the hard-allowlist invariant.
#[derive(Default)]
pub struct DropCounter(AtomicU64);

impl DropCounter {
    pub fn record_drop(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn total(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

// --- reply formatting ---

/// The exact reply for a message that failed to transcribe (missing
/// whisper.cpp binary/model, ffmpeg missing, spawn failure, timeout, empty
/// transcript) -- the audio itself is still saved and filed as a plain
/// memory either way, this is just what the user sees back.
pub const VOICE_TRANSCRIBE_FAILURE_REPLY: &str = "couldn't transcribe — saved raw audio reference";

/// Builds the confirmation reply for a filed note. `worth` (the classifier's
/// triage verdict — see `kb::note::Worth`) is `Durable` in the ordinary case
/// and leaves the reply exactly as before; `Dubious`/`Noise` append a plain
/// statement that the note was demoted to the review queue instead, so the
/// user isn't left thinking it landed as a regular reviewed memory.
pub fn format_note_confirmation(topic: &str, title: &str, fact_count: usize, worth: Worth) -> String {
    let noun = if fact_count == 1 { "memory" } else { "memories" };
    let base = format!("filed {} {} under \"{}\" — refer to it as: \"{}\"", fact_count, noun, topic, title);
    match worth {
        Worth::Durable => base,
        Worth::Dubious => format!("{} — filed to review queue (looked dubious) — `mach kb review` to promote", base),
        Worth::Noise => format!("{} — filed to review queue (looked like noise) — `mach kb review` to promote", base),
    }
}

pub fn format_note_failure(reason: &str) -> String {
    format!("couldn't file that note: {}", reason)
}

pub fn format_start_reply(allowed_user_id: i64) -> String {
    format!("mach-telegramd ready. Allowlisted sender id: {}.", allowed_user_id)
}

pub fn format_status_reply(uptime: Duration, kb_row_count: i64) -> String {
    let noun = if kb_row_count == 1 { "memory" } else { "memories" };
    format!("mach-telegramd up {} — {} {} in the knowledge bank", format_duration(uptime), kb_row_count, noun)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{}h{}m{}s", h, m, s)
    } else if m > 0 {
        format!("{}m{}s", m, s)
    } else {
        format!("{}s", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Audio, Chat, PhotoSize, Voice};

    fn base_msg() -> Message {
        Message { message_id: 1, from: None, chat: Chat { id: 1 }, text: None, caption: None, voice: None, audio: None, photo: None }
    }

    // ---------- is_allowed: the hard sender allowlist ----------

    #[test]
    fn allowlisted_sender_passes() {
        assert!(is_allowed(Some(42), 42));
    }

    #[test]
    fn any_other_sender_is_dropped() {
        assert!(!is_allowed(Some(1), 42));
        assert!(!is_allowed(Some(-1), 42));
        assert!(!is_allowed(Some(0), 42));
    }

    #[test]
    fn a_message_with_no_sender_at_all_is_dropped() {
        assert!(!is_allowed(None, 42));
    }

    // ---------- classify_message ----------

    #[test]
    fn plain_text_classifies_as_text_data_never_a_command() {
        let mut m = base_msg();
        m.text = Some("remember to buy milk tomorrow".to_string());
        assert_eq!(classify_message(&m), MessageKind::Text("remember to buy milk tomorrow".to_string()));
    }

    #[test]
    fn text_that_merely_contains_slash_start_is_still_plain_text() {
        // Only an exact leading command token is a command -- "note data
        // that happens to mention /start" must never be treated as one.
        let mut m = base_msg();
        m.text = Some("I typed /start by accident in my journal entry".to_string());
        assert_eq!(classify_message(&m), MessageKind::Text("I typed /start by accident in my journal entry".to_string()));
    }

    #[test]
    fn exact_start_command_is_recognized() {
        let mut m = base_msg();
        m.text = Some("/start".to_string());
        assert_eq!(classify_message(&m), MessageKind::StartCommand);
    }

    #[test]
    fn start_command_with_bot_username_suffix_is_recognized() {
        let mut m = base_msg();
        m.text = Some("/start@my_notes_bot".to_string());
        assert_eq!(classify_message(&m), MessageKind::StartCommand);
    }

    #[test]
    fn exact_status_command_is_recognized() {
        let mut m = base_msg();
        m.text = Some("  /status  ".to_string());
        assert_eq!(classify_message(&m), MessageKind::StatusCommand);
    }

    #[test]
    fn empty_or_whitespace_text_with_nothing_else_is_empty() {
        let mut m = base_msg();
        m.text = Some("   ".to_string());
        assert_eq!(classify_message(&m), MessageKind::Empty);
    }

    #[test]
    fn voice_message_classifies_with_file_id_and_size() {
        let mut m = base_msg();
        m.voice = Some(Voice { file_id: "v1".to_string(), file_size: Some(1000) });
        assert_eq!(classify_message(&m), MessageKind::Voice { file_id: "v1".to_string(), file_size: Some(1000) });
    }

    #[test]
    fn audio_message_classifies_with_file_id_and_size() {
        let mut m = base_msg();
        m.audio = Some(Audio { file_id: "a1".to_string(), file_size: Some(2000) });
        assert_eq!(classify_message(&m), MessageKind::Audio { file_id: "a1".to_string(), file_size: Some(2000) });
    }

    #[test]
    fn photo_message_picks_the_largest_size_and_keeps_the_caption() {
        let mut m = base_msg();
        m.caption = Some("my new desk setup".to_string());
        m.photo = Some(vec![
            PhotoSize { file_id: "small".to_string(), file_size: Some(500), width: 90, height: 90 },
            PhotoSize { file_id: "large".to_string(), file_size: Some(50000), width: 1280, height: 960 },
            PhotoSize { file_id: "medium".to_string(), file_size: Some(5000), width: 320, height: 240 },
        ]);
        assert_eq!(
            classify_message(&m),
            MessageKind::Photo { file_id: "large".to_string(), caption: "my new desk setup".to_string() }
        );
    }

    #[test]
    fn photo_message_without_a_caption_gets_an_empty_one() {
        let mut m = base_msg();
        m.photo = Some(vec![PhotoSize { file_id: "only".to_string(), file_size: None, width: 100, height: 100 }]);
        assert_eq!(classify_message(&m), MessageKind::Photo { file_id: "only".to_string(), caption: String::new() });
    }

    #[test]
    fn a_message_with_nothing_recognizable_is_empty() {
        assert_eq!(classify_message(&base_msg()), MessageKind::Empty);
    }

    // ---------- DropCounter ----------

    #[test]
    fn drop_counter_counts_up_and_reports_the_running_total() {
        let counter = DropCounter::default();
        assert_eq!(counter.record_drop(), 1);
        assert_eq!(counter.record_drop(), 2);
        assert_eq!(counter.record_drop(), 3);
        assert_eq!(counter.total(), 3);
    }

    // ---------- reply formatting ----------

    #[test]
    fn note_confirmation_singular_vs_plural() {
        assert_eq!(
            format_note_confirmation("helios-rendering", "Helios RHI descriptor design", 1, Worth::Durable),
            "filed 1 memory under \"helios-rendering\" — refer to it as: \"Helios RHI descriptor design\""
        );
        assert_eq!(
            format_note_confirmation("dotfiles", "chezmoi migration notes", 3, Worth::Durable),
            "filed 3 memories under \"dotfiles\" — refer to it as: \"chezmoi migration notes\""
        );
    }

    #[test]
    fn note_confirmation_states_review_queue_demotion_plainly_when_dubious() {
        assert_eq!(
            format_note_confirmation("notes", "a vague fragment", 1, Worth::Dubious),
            "filed 1 memory under \"notes\" — refer to it as: \"a vague fragment\" — filed to review queue (looked dubious) — `mach kb review` to promote"
        );
    }

    #[test]
    fn note_confirmation_states_review_queue_demotion_plainly_when_noise() {
        assert_eq!(
            format_note_confirmation("notes", "asdf test", 1, Worth::Noise),
            "filed 1 memory under \"notes\" — refer to it as: \"asdf test\" — filed to review queue (looked like noise) — `mach kb review` to promote"
        );
    }

    #[test]
    fn note_failure_includes_the_reason() {
        assert_eq!(format_note_failure("database error: disk full"), "couldn't file that note: database error: disk full");
    }

    #[test]
    fn voice_failure_reply_is_the_exact_specified_text() {
        assert_eq!(VOICE_TRANSCRIBE_FAILURE_REPLY, "couldn't transcribe — saved raw audio reference");
    }

    #[test]
    fn start_reply_includes_the_configured_user_id() {
        assert_eq!(format_start_reply(987654321), "mach-telegramd ready. Allowlisted sender id: 987654321.");
    }

    #[test]
    fn status_reply_formats_uptime_and_row_count() {
        assert_eq!(format_status_reply(Duration::from_secs(5), 0), "mach-telegramd up 5s — 0 memories in the knowledge bank");
        assert_eq!(format_status_reply(Duration::from_secs(65), 1), "mach-telegramd up 1m5s — 1 memory in the knowledge bank");
        assert_eq!(format_status_reply(Duration::from_secs(3725), 42), "mach-telegramd up 1h2m5s — 42 memories in the knowledge bank");
    }
}
