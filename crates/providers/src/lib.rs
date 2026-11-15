//! Chat providers.
//!
//! Every provider turns a [`ChatRequest`] into a stream of
//! [`common::StreamEvent`]s. Cancellation is by dropping the stream: the HTTP
//! connection closes and servers (including llama.cpp) stop generating.

pub mod anthropic;
pub mod mock;
pub mod openai;
pub mod sse;

use common::{Message, StreamEvent};
use futures::stream::BoxStream;

#[derive(Debug, thiserror::Error, Clone)]
pub enum ProviderError {
    #[error("cannot reach {endpoint}: {message}")]
    Connect { endpoint: String, message: String },
    #[error("{provider} rejected the credentials (HTTP {status}). Check {hint}")]
    Auth { provider: String, status: u16, hint: String },
    #[error("{provider} returned HTTP {status}: {message}")]
    Http { provider: String, status: u16, message: String },
    #[error("malformed response from {provider}: {message}")]
    Protocol { provider: String, message: String },
    #[error("{0}")]
    Config(String),
}

#[derive(Debug, Clone, Default)]
pub struct Sampling {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub sampling: Sampling,
    /// JSON-schema tool definitions (OpenAI function format).
    pub tools: Vec<serde_json::Value>,
}

pub type EventStream = BoxStream<'static, Result<StreamEvent, ProviderError>>;

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteModel {
    pub id: String,
}

pub trait Provider: Send + Sync {
    /// Short identifier: `local`, `ollama`, `openai`, `anthropic`, `compat`.
    fn id(&self) -> &str;
    /// Human-readable endpoint description (never includes credentials).
    fn describe(&self) -> String;
    fn chat(&self, req: ChatRequest) -> EventStream;
    fn list_models(&self) -> futures::future::BoxFuture<'static, Result<Vec<RemoteModel>, ProviderError>>;
}

/// Install the pure-Rust TLS crypto provider (idempotent).
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub(crate) fn status_error(provider: &str, status: u16, body: &str, key_hint: &str) -> ProviderError {
    if status == 401 || status == 403 {
        return ProviderError::Auth { provider: provider.into(), status, hint: key_hint.into() };
    }
    // Pull a readable message out of common JSON error shapes.
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/error"))
                .or_else(|| v.pointer("/message"))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(300).collect());
    ProviderError::Http { provider: provider.into(), status, message }
}
