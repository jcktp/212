//! Model management: GGUF inspection, the local model store, resumable
//! downloads and the Hugging Face / Ollama registry clients.

pub mod catalog;
pub mod download;
pub mod estimate;
pub mod gguf;
pub mod hf;
pub mod ollama;
pub mod store;

pub use gguf::GgufInfo;
pub use store::{InstalledModel, ModelSource, ModelStore};

/// User agent for all model-related HTTP traffic. Deliberately generic.
pub const USER_AGENT: &str = concat!("212/", env!("CARGO_PKG_VERSION"));

/// Install the pure-Rust TLS crypto provider (idempotent).
pub fn init_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn http_client() -> reqwest::Client {
    init_tls();
    reqwest::Client::builder().user_agent(USER_AGENT).connect_timeout(std::time::Duration::from_secs(15)).build().expect("reqwest client")
}
