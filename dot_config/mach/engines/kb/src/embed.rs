
//! Ollama HTTP embedding client.
//!
//! Behind the `Embedder` trait so `cli` can be tested against a fake that
//! never touches the network; `OllamaEmbedder` is the real implementation
//! used at runtime.
use std::time::Duration;

use serde::Deserialize;

use crate::store::KbError;

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";
pub const DEFAULT_MODEL: &str = "nomic-embed-text";

pub trait Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, KbError>;
}

pub struct OllamaEmbedder {
    base_url: String,
    model: String,
}

impl OllamaEmbedder {
    /// Uses `MACH_KB_OLLAMA_URL` / `MACH_KB_OLLAMA_MODEL` if set, otherwise
    /// the defaults (`http://127.0.0.1:11434`, `nomic-embed-text`).
    pub fn new() -> Self {
        OllamaEmbedder {
            base_url: std::env::var("MACH_KB_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            model: std::env::var("MACH_KB_OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
        }
    }
}

impl Default for OllamaEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

impl Embedder for OllamaEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
        let url = format!("{}/api/embed", self.base_url);
        let resp = ureq::post(&url)
            .timeout(Duration::from_secs(30))
            .send_json(serde_json::json!({ "model": self.model, "input": text }))
            .map_err(|e| {
                KbError::Embed(format!(
                    "cannot reach ollama at {} with model '{}' (is `ollama serve` running, \
                     and has `ollama pull {}` been run? — {})",
                    self.base_url, self.model, self.model, e
                ))
            })?;
        let parsed: EmbedResponse = resp
            .into_json()
            .map_err(|e| KbError::Embed(format!("unexpected response from ollama: {}", e)))?;
        parsed
            .embeddings
            .into_iter()
            .next()
            .ok_or_else(|| KbError::Embed("ollama returned no embedding vector".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeEmbedder;

    impl Embedder for FakeEmbedder {
        fn embed(&self, text: &str) -> Result<Vec<f32>, KbError> {
            Ok(text.bytes().map(|b| b as f32).collect())
        }
    }

    struct FailingEmbedder;

    impl Embedder for FailingEmbedder {
        fn embed(&self, _text: &str) -> Result<Vec<f32>, KbError> {
            Err(KbError::Embed("ollama unreachable".into()))
        }
    }

    #[test]
    fn fake_embedder_is_deterministic() {
        let e = FakeEmbedder;
        assert_eq!(e.embed("hi").unwrap(), e.embed("hi").unwrap());
    }

    #[test]
    fn failing_embedder_reports_clear_error() {
        let e = FailingEmbedder;
        let err = e.embed("x").unwrap_err();
        assert!(err.to_string().contains("unreachable"));
    }
}
