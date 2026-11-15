//! Local inference: hardware detection, llama.cpp backend discovery and
//! installation, and the managed `llama-server` child process.
//!
//! Design: llama.cpp runs as a supervised child process bound to loopback with
//! a random port and a random API key. A crash in the inference engine can
//! never take down the UI, GPU backends can be swapped without rebuilding 212,
//! and the process is reaped on exit (and on next start if 212 was killed).

pub mod backend;
pub mod hardware;
pub mod install;
pub mod server;

pub use backend::{Backend, BackendSource};
pub use hardware::{Accel, Hardware};
pub use server::{LlamaServer, ServerError, ServerOptions};
