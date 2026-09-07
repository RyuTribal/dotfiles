//! Wires `api`/`bot`/`voice` to the real `kb` note pipeline: one update in,
//! zero-or-one reply out. Every I/O boundary (Telegram HTTP, the classifier/
//! embedder LLM calls, whisper transcription) is a trait object supplied by
//! the caller, so `Pipeline::handle_update` is exercised in tests against
//! fakes for all of them plus a real (`:memory:`) SQLite connection.
use std::path::{Path, PathBuf};
use std::time::Instant;

use kb::embed::Embedder;
use kb::note::{self, NoteLlm};
use kb::store;
use rusqlite::Connection;

use crate::api::{Message, TelegramApi, Update, MAX_FILE_BYTES};
use crate::bot::{self, classify_message, is_allowed, DropCounter, MessageKind};
use crate::voice::Transcriber;

/// Where downloaded voice/audio bytes land before (attempted) conversion
/// and transcription, and where an untranscribable file is kept permanently
/// as the "voice note (untranscribed): <path>" fallback's evidence.
pub fn audio_store_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".local/share/mach/kb-audio"))
}

fn save_audio_bytes(bytes: &[u8], update_id: i64, ext: &str) -> Result<PathBuf, String> {
    let dir = audio_store_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{}-{}.{}", ts, update_id, ext));
    std::fs::write(&path, bytes).map_err(|e| format!("could not write {}: {}", path.display(), e))?;
    Ok(path)
}

/// Extension inferred from a Telegram `file_path` (e.g. `voice/file_0.oga`
/// -> `oga`), falling back to `bin` if there isn't one — mirrors
/// `kb::note::store_image`'s own fallback.
fn ext_from_file_path(file_path: &str) -> &str {
    Path::new(file_path).extension().and_then(|e| e.to_str()).unwrap_or("bin")
}

pub struct Pipeline<'a> {
    pub api: &'a dyn TelegramApi,
    pub allowed_user_id: i64,
    pub drop_counter: &'a DropCounter,
    pub conn: &'a Connection,
    pub llm: &'a dyn NoteLlm,
    pub embedder: &'a dyn Embedder,
    /// `None` when no whisper.cpp binary/model could be resolved at
    /// startup -- every voice/audio message then goes straight to the
    /// untranscribed fallback without even trying.
    pub transcriber: Option<&'a dyn Transcriber>,
    pub converter: &'a dyn crate::voice::Converter,
    pub claude_bin: String,
    pub started_at: Instant,
}

impl<'a> Pipeline<'a> {
    /// Processes one update end to end: allowlist check, dispatch, at most
    /// one reply sent back. Never panics or propagates an error -- a failure
    /// sending the reply itself is logged (stderr) and otherwise swallowed,
    /// since the next long-poll cycle must keep going regardless.
    pub fn handle_update(&self, update: &Update) {
        let Some(msg) = &update.message else { return };
        let sender_id = msg.from.as_ref().map(|u| u.id);
        if !is_allowed(sender_id, self.allowed_user_id) {
            let n = self.drop_counter.record_drop();
            eprintln!("mach-telegramd: dropped {} update(s) from disallowed sender(s) so far", n);
            return;
        }

        let chat_id = msg.chat.id;
        let reply = self.dispatch(update.update_id, msg);
        if let Some(text) = reply {
            if let Err(e) = self.api.send_message(chat_id, &text) {
                eprintln!("mach-telegramd: failed to send reply: {}", e);
            }
        }
    }

    fn dispatch(&self, update_id: i64, msg: &Message) -> Option<String> {
        match classify_message(msg) {
            MessageKind::StartCommand => Some(bot::format_start_reply(self.allowed_user_id)),
            MessageKind::StatusCommand => {
                let uptime = self.started_at.elapsed();
                let count = store::all_memories(self.conn).map(|v| v.len() as i64).unwrap_or(-1);
                Some(bot::format_status_reply(uptime, count))
            }
            MessageKind::Text(text) => Some(self.file_text_note(&text)),
            MessageKind::Voice { file_id, file_size } => Some(self.handle_audio(update_id, &file_id, file_size)),
            MessageKind::Audio { file_id, file_size } => Some(self.handle_audio(update_id, &file_id, file_size)),
            MessageKind::Photo { file_id, caption } => Some(self.handle_photo(update_id, &file_id, &caption)),
            MessageKind::Empty => None,
        }
    }

    fn file_text_note(&self, text: &str) -> String {
        match note::file_note(self.conn, self.llm, self.embedder, text, None, note::DEFAULT_IMPORTANCE) {
            Ok(filed) => bot::format_note_confirmation(&filed.topic, &filed.title, filed.facts.len(), filed.worth),
            Err(e) => bot::format_note_failure(&e.to_string()),
        }
    }

    /// Downloads `file_id` (size-capped) and either transcribes it and files
    /// a note from the transcript, or — on any failure along that chain, or
    /// when no transcriber is configured at all — stores a plain "voice
    /// note (untranscribed): <path>" memory and reports the fixed failure
    /// reply. The downloaded audio is never lost either way.
    fn handle_audio(&self, update_id: i64, file_id: &str, declared_size: Option<u64>) -> String {
        if let Some(size) = declared_size {
            if size > MAX_FILE_BYTES {
                return bot::format_note_failure(&format!("audio file too large ({} bytes, cap {})", size, MAX_FILE_BYTES));
            }
        }
        let (file_path, size) = match self.api.get_file(file_id) {
            Ok(v) => v,
            Err(e) => return bot::format_note_failure(&format!("could not resolve audio file: {}", e)),
        };
        if let Some(size) = size {
            if size > MAX_FILE_BYTES {
                return bot::format_note_failure(&format!("audio file too large ({} bytes, cap {})", size, MAX_FILE_BYTES));
            }
        }
        let bytes = match self.api.download_file(&file_path) {
            Ok(b) => b,
            Err(e) => return bot::format_note_failure(&format!("could not download audio: {}", e)),
        };
        let ext = ext_from_file_path(&file_path).to_string();
        let saved = match save_audio_bytes(&bytes, update_id, &ext) {
            Ok(p) => p,
            Err(e) => return bot::format_note_failure(&format!("could not save audio: {}", e)),
        };

        match self.try_transcribe(&saved) {
            Ok(transcript) => {
                let note_text = format!("Voice note transcript: {}", transcript);
                self.file_text_note(&note_text)
            }
            Err(_reason) => {
                let content = format!("voice note (untranscribed): {}", saved.display());
                if let Err(e) = store::insert(self.conn, &content, Some("telegram:voice"), None, true, None, note::DEFAULT_IMPORTANCE) {
                    return bot::format_note_failure(&e.to_string());
                }
                bot::VOICE_TRANSCRIBE_FAILURE_REPLY.to_string()
            }
        }
    }

    fn try_transcribe(&self, saved: &Path) -> Result<String, String> {
        let transcriber = self.transcriber.ok_or_else(|| "no whisper.cpp binary/model configured".to_string())?;
        let wav = self.converter.convert(saved)?;
        let result = transcriber.transcribe(&wav);
        let _ = std::fs::remove_file(&wav); // scratch conversion artifact, not the permanent evidence copy
        result
    }

    fn handle_photo(&self, update_id: i64, file_id: &str, caption: &str) -> String {
        let (file_path, size) = match self.api.get_file(file_id) {
            Ok(v) => v,
            Err(e) => return bot::format_note_failure(&format!("could not resolve photo file: {}", e)),
        };
        if let Some(size) = size {
            if size > MAX_FILE_BYTES {
                return bot::format_note_failure(&format!("photo file too large ({} bytes, cap {})", size, MAX_FILE_BYTES));
            }
        }
        let bytes = match self.api.download_file(&file_path) {
            Ok(b) => b,
            Err(e) => return bot::format_note_failure(&format!("could not download photo: {}", e)),
        };
        let ext = ext_from_file_path(&file_path).to_string();
        let temp = match save_audio_bytes(&bytes, update_id, &ext) {
            // Reuses the same "drop it under a mach-owned data dir with a
            // unique name" helper as audio -- the file only needs to live
            // long enough for `store_image` to copy it into the permanent,
            // content-addressed image store below.
            Ok(p) => p,
            Err(e) => return bot::format_note_failure(&format!("could not save photo: {}", e)),
        };
        let stored = match note::store_image(&temp) {
            Ok(p) => p,
            Err(e) => return bot::format_note_failure(&format!("could not store photo: {}", e)),
        };
        let _ = std::fs::remove_file(&temp);

        match note::file_note_with_image(self.conn, self.llm, self.embedder, &self.claude_bin, &stored, caption, note::DEFAULT_IMPORTANCE) {
            Ok(filed) => bot::format_note_confirmation(&filed.topic, &filed.title, filed.facts.len(), filed.worth),
            Err(e) => bot::format_note_failure(&e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Chat, PhotoSize, User, Voice};
    use kb::store::KbError;
    use std::cell::RefCell;
    use std::sync::Mutex;

    struct FakeApi {
        get_file_result: Result<(String, Option<u64>), String>,
        download_result: Result<Vec<u8>, String>,
        sent: Mutex<Vec<(i64, String)>>,
    }

    impl Default for FakeApi {
        fn default() -> Self {
            FakeApi { get_file_result: Ok(("voice/f.oga".to_string(), Some(10))), download_result: Ok(vec![1, 2, 3]), sent: Mutex::new(Vec::new()) }
        }
    }

    impl TelegramApi for FakeApi {
        fn get_updates(&self, _offset: i64, _timeout_secs: u64) -> Result<Vec<Update>, String> {
            Ok(Vec::new())
        }
        fn get_file(&self, _file_id: &str) -> Result<(String, Option<u64>), String> {
            self.get_file_result.clone()
        }
        fn download_file(&self, _file_path: &str) -> Result<Vec<u8>, String> {
            self.download_result.clone()
        }
        fn send_message(&self, chat_id: i64, text: &str) -> Result<(), String> {
            self.sent.lock().unwrap().push((chat_id, text.to_string()));
            Ok(())
        }
    }

    struct FakeLlm {
        reply: String,
    }

    impl NoteLlm for FakeLlm {
        fn call(&self, _prompt: &str) -> Result<String, String> {
            Ok(self.reply.clone())
        }
    }

    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().map(|b| b as f32).collect())
        }
    }

    struct FailingTranscriber;

    impl Transcriber for FailingTranscriber {
        fn transcribe(&self, _path: &Path) -> Result<String, String> {
            Err("no model".to_string())
        }
    }

    struct WorkingTranscriber(RefCell<String>);

    impl Transcriber for WorkingTranscriber {
        fn transcribe(&self, _path: &Path) -> Result<String, String> {
            Ok(self.0.borrow().clone())
        }
    }

    /// Pass-through: no real `ffmpeg` invocation, just hands the input path
    /// straight back -- lets tests exercise the transcriber itself (working
    /// or failing) without needing a real conversion to succeed first.
    struct FakeConverter;

    impl crate::voice::Converter for FakeConverter {
        fn convert(&self, input: &Path) -> Result<PathBuf, String> {
            Ok(input.to_path_buf())
        }
    }

    fn msg_from(sender: Option<i64>) -> Message {
        Message { message_id: 1, from: sender.map(|id| User { id }), chat: Chat { id: 999 }, text: None, caption: None, voice: None, audio: None, photo: None }
    }

    fn scratch_conn() -> Connection {
        store::open_with_path(Path::new(":memory:")).unwrap()
    }

    // ---------- allowlist drop ----------

    #[test]
    fn disallowed_sender_is_dropped_silently_no_reply_sent() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: "TOPIC: t\nTITLE: a title\nFACT: fact\n".to_string() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(999)); // not the allowed id
        msg.text = Some("some note text".to_string());
        pipeline.handle_update(&Update { update_id: 1, message: Some(msg) });

        assert!(api.sent.lock().unwrap().is_empty());
        assert_eq!(counter.total(), 1);
        assert!(store::all_memories(&conn).unwrap().is_empty(), "a dropped sender's content must never reach the note pipeline");
    }

    #[test]
    fn allowlisted_text_message_files_a_note_and_replies_with_confirmation() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: "TOPIC: helios\nTITLE: a title\nFACT: fact one\n".to_string() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.text = Some("remember this".to_string());
        pipeline.handle_update(&Update { update_id: 2, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0], (999, "filed 1 memory under \"helios\" — refer to it as: \"a title\"".to_string()));
        assert_eq!(store::all_memories(&conn).unwrap().len(), 1);
    }

    #[test]
    fn text_message_the_classifier_judges_as_noise_files_unreviewed_and_replies_with_the_review_queue_notice() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: "TOPIC: notes\nTITLE: a title\nWORTH: noise\nFACT: fact one\n".to_string() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.text = Some("asdf test qwerty".to_string());
        pipeline.handle_update(&Update { update_id: 11, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0],
            (
                999,
                "filed 1 memory under \"notes\" — refer to it as: \"a title\" — filed to review queue (looked like noise) — `mach kb review` to promote"
                    .to_string()
            )
        );
        let stored = store::all_memories(&conn).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].reviewed);
        assert_eq!(stored[0].importance, 2);
    }

    #[test]
    fn start_command_replies_with_the_configured_allowed_user_id() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 555,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(555));
        msg.text = Some("/start".to_string());
        pipeline.handle_update(&Update { update_id: 3, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("555"));
    }

    #[test]
    fn status_command_reports_row_count() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        store::insert(&conn, "a memory", None, None, true, None, 5).unwrap();
        store::insert(&conn, "another memory", None, None, true, None, 5).unwrap();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 555,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(555));
        msg.text = Some("/status".to_string());
        pipeline.handle_update(&Update { update_id: 4, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert!(sent[0].1.contains("2 memories"));
    }

    // ---------- voice degradation path ----------

    #[test]
    fn voice_message_with_no_transcriber_configured_degrades_to_untranscribed_fallback() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None, // no whisper.cpp binary/model resolved
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.voice = Some(Voice { file_id: "v1".to_string(), file_size: Some(1000) });
        pipeline.handle_update(&Update { update_id: 5, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "couldn't transcribe — saved raw audio reference");

        let stored = store::all_memories(&conn).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.starts_with("voice note (untranscribed): "));
        let _ = std::fs::remove_dir_all(audio_store_dir().unwrap());
    }

    #[test]
    fn voice_message_whose_transcription_fails_still_degrades_to_untranscribed_fallback() {
        // Distinct from the "no transcriber configured" case above: here a
        // transcriber IS configured (whisper.cpp resolved fine), but the
        // transcription attempt itself fails -- must degrade exactly the
        // same way, never lose the note.
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let transcriber = FailingTranscriber;
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: Some(&transcriber),
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.voice = Some(Voice { file_id: "v1".to_string(), file_size: Some(1000) });
        pipeline.handle_update(&Update { update_id: 9, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent[0].1, "couldn't transcribe — saved raw audio reference");
        let stored = store::all_memories(&conn).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.starts_with("voice note (untranscribed): "));
        let _ = std::fs::remove_dir_all(audio_store_dir().unwrap());
    }

    #[test]
    fn voice_message_that_transcribes_successfully_files_a_note_from_the_transcript() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: "TOPIC: voice-notes\nTITLE: a voice title\nFACT: the transcribed fact\n".to_string() };
        let counter = DropCounter::default();
        let transcriber = WorkingTranscriber(RefCell::new("this is what I said".to_string()));
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: Some(&transcriber),
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.voice = Some(Voice { file_id: "v1".to_string(), file_size: Some(1000) });
        pipeline.handle_update(&Update { update_id: 10, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent[0].1, "filed 1 memory under \"voice-notes\" — refer to it as: \"a voice title\"");
        let stored = store::all_memories(&conn).unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].content.contains("the transcribed fact"));
        assert!(!stored[0].content.starts_with("voice note (untranscribed)"));
        let _ = std::fs::remove_dir_all(audio_store_dir().unwrap());
    }

    #[test]
    fn voice_message_whose_declared_size_exceeds_the_cap_is_rejected_before_downloading() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.voice = Some(Voice { file_id: "v1".to_string(), file_size: Some(MAX_FILE_BYTES + 1) });
        pipeline.handle_update(&Update { update_id: 6, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].1.contains("too large"));
        assert!(store::all_memories(&conn).unwrap().is_empty(), "an oversized file must never be downloaded or stored");
    }

    #[test]
    fn empty_message_produces_no_reply() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        let llm = FakeLlm { reply: String::new() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "claude".to_string(),
            started_at: Instant::now(),
        };
        let msg = msg_from(Some(42));
        pipeline.handle_update(&Update { update_id: 7, message: Some(msg) });
        assert!(api.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn photo_message_files_an_image_note_and_replies_with_confirmation() {
        let api = FakeApi::default();
        let conn = scratch_conn();
        // describe_image will fail to spawn (no real `claude` at this
        // path), which is exactly the "never lose the note" fallback --
        // the combined note text still reaches the classifier below.
        let llm = FakeLlm { reply: "TOPIC: photos\nTITLE: a photo title\nFACT: a fact about the photo\n".to_string() };
        let counter = DropCounter::default();
        let pipeline = Pipeline {
            api: &api,
            allowed_user_id: 42,
            drop_counter: &counter,
            conn: &conn,
            llm: &llm,
            embedder: &FakeEmbedder,
            transcriber: None,
            converter: &FakeConverter,
            claude_bin: "/nonexistent/definitely-not-claude-xyz".to_string(),
            started_at: Instant::now(),
        };
        let mut msg = msg_from(Some(42));
        msg.caption = Some("my new desk setup".to_string());
        msg.photo = Some(vec![PhotoSize { file_id: "p1".to_string(), file_size: Some(100), width: 800, height: 600 }]);
        pipeline.handle_update(&Update { update_id: 8, message: Some(msg) });

        let sent = api.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, "filed 1 memory under \"photos\" — refer to it as: \"a photo title\"");
        assert_eq!(store::all_memories(&conn).unwrap().len(), 1);
    }
}
