//! Shared vocabulary for all 212 subsystems.
//!
//! This crate deliberately has almost no dependencies. It defines the chat
//! message model, the streaming event model, the UI <-> runtime protocol and
//! platform paths, so that the TUI, runtime and providers can evolve
//! independently.

pub mod chat;
pub mod format;
pub mod paths;
pub mod proto;

pub use chat::{Message, Role, StreamEvent, ToolCall, Usage};
pub use paths::AppPaths;

/// Application version, shared by every crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
