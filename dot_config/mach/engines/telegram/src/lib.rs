//! telegram — the Telegram note bridge. Long-polls a private bot for
//! messages from one allowlisted sender and files them into the `kb` note
//! pipeline: text becomes a note directly, voice/audio is transcribed (via
//! whisper.cpp, when available) first, and photos reuse the image-note
//! path. Hosted by `machd` as its one registered subsystem.
//!
//! Module layout:
//!   config    — `~/.local/share/mach/telegram.toml` (secret bot token,
//!               OUTSIDE the chezmoi-managed tree; see its own doc comment)
//!   state     — the persisted `getUpdates` offset
//!   api       — the Telegram Bot HTTP client, behind `TelegramApi` for tests
//!   voice     — whisper.cpp transcription, behind `Transcriber` for tests
//!   bot       — pure allowlist/dispatch/reply-formatting logic
//!   pipeline  — wires the above to a real `kb` store connection
pub mod api;
pub mod bot;
pub mod config;
pub mod pipeline;
pub mod state;
pub mod voice;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use kb::embed::OllamaEmbedder;
use kb::note::ProcessNoteLlm;
use kb::store;

use api::{HttpTelegramApi, TelegramApi};
use bot::DropCounter;
use pipeline::Pipeline;

/// Telegram's own long-poll `timeout` parameter.
const POLL_TIMEOUT_SECS: u64 = 30;
/// Backoff floor/cap for a repeatedly-failing `getUpdates` (offline laptop,
/// DNS failure, Telegram outage) -- a laptop that's asleep or off-wifi must
/// degrade to an infrequent retry instead of crash-looping or busy-looping.
const BACKOFF_MIN: Duration = Duration::from_secs(30);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Resolves a whisper.cpp transcriber if a binary is on `PATH` and the
/// model can be found/downloaded; `None` (with a one-line log) means every
/// voice/audio message will go straight to the untranscribed fallback.
fn build_transcriber() -> Option<Box<dyn voice::Transcriber>> {
    let binary = voice::find_binary()?;
    match voice::ensure_model() {
        Ok(model) => Some(Box::new(voice::WhisperCliTranscriber::new(binary, model))),
        Err(e) => {
            eprintln!("mach-telegramd: whisper model unavailable ({}) — voice notes will be saved untranscribed", e);
            None
        }
    }
}

fn sleep_interruptible(d: Duration, shutdown: &AtomicBool) {
    let step = Duration::from_millis(200);
    let mut waited = Duration::ZERO;
    while waited < d && !shutdown.load(Ordering::Relaxed) {
        let remaining = d - waited;
        std::thread::sleep(step.min(remaining));
        waited += step;
    }
}

/// Runs the long-poll loop until `shutdown` is set (SIGTERM, via `machd`).
/// Returns `Ok(())` on a clean shutdown; `Err` only for a setup failure that
/// happens before the loop can even start (state path / kb store).
pub fn run(cfg: config::Config, shutdown: &AtomicBool) -> Result<(), String> {
    let api = HttpTelegramApi::new(cfg.bot_token.clone());
    let state_path = state::state_path()?;
    let mut st = state::load(&state_path);

    let conn = store::open().map_err(|e| e.to_string())?;
    let llm = ProcessNoteLlm::new();
    let embedder = OllamaEmbedder::new();
    let claude_bin = std::env::var("CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());

    let transcriber = build_transcriber();
    if transcriber.is_none() {
        eprintln!("mach-telegramd: no whisper.cpp binary found on PATH — voice notes will be saved untranscribed (see install.sh)");
    }

    let converter = voice::FfmpegConverter;
    let drop_counter = DropCounter::default();
    let started_at = Instant::now();
    let mut backoff = BACKOFF_MIN;

    eprintln!("mach-telegramd: ready, allowlisted sender id {}", cfg.allowed_user_id);

    while !shutdown.load(Ordering::Relaxed) {
        match api.get_updates(st.offset, POLL_TIMEOUT_SECS) {
            Ok(updates) => {
                backoff = BACKOFF_MIN;
                if updates.is_empty() {
                    continue;
                }
                let pipeline = Pipeline {
                    api: &api,
                    allowed_user_id: cfg.allowed_user_id,
                    drop_counter: &drop_counter,
                    conn: &conn,
                    llm: &llm,
                    embedder: &embedder,
                    transcriber: transcriber.as_deref(),
                    converter: &converter,
                    claude_bin: claude_bin.clone(),
                    started_at,
                };
                for u in &updates {
                    pipeline.handle_update(u);
                    st.offset = u.update_id + 1;
                }
                if let Err(e) = state::save(&state_path, st) {
                    eprintln!("mach-telegramd: warning: could not persist getUpdates offset: {}", e);
                }
            }
            Err(e) => {
                eprintln!("mach-telegramd: getUpdates failed ({}) — backing off {:?}", e, backoff);
                sleep_interruptible(backoff, shutdown);
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
    eprintln!("mach-telegramd: shutting down (signal received)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_interruptible_returns_early_when_shutdown_flips_mid_wait() {
        let shutdown = std::sync::Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        sleep_interruptible(Duration::from_secs(30), &shutdown);
        // Must return well before the full 30s once shutdown is flipped.
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn sleep_interruptible_waits_out_the_full_duration_when_never_interrupted() {
        let shutdown = AtomicBool::new(false);
        let start = Instant::now();
        sleep_interruptible(Duration::from_millis(100), &shutdown);
        assert!(start.elapsed() >= Duration::from_millis(90));
    }
}
