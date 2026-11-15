//! The protocol between the terminal UI and the runtime.
//!
//! The UI never touches providers, processes or files directly: it sends
//! [`Request`]s and renders [`Event`]s. This keeps rendering decoupled from
//! inference and makes both sides testable in isolation.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::chat::StreamEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    #[default]
    Chat,
    Plan,
    Build,
}

impl AgentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentMode::Chat => "chat",
            AgentMode::Plan => "plan",
            AgentMode::Build => "build",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Self::Chat),
            "plan" => Some(Self::Plan),
            "build" => Some(Self::Build),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// UI -> runtime.
#[derive(Debug, Clone)]
pub enum Request {
    /// Send a user message and stream a reply.
    Send {
        text: String,
    },
    /// Cancel the in-flight generation (or the active download if no generation runs).
    Cancel,
    NewConversation,
    /// Start (or switch) the local backend with an installed model.
    LoadModel {
        id: String,
    },
    UnloadModel,
    ListModels,
    ModelInfo {
        id: String,
    },
    RemoveModel {
        id: String,
    },
    /// A GGUF path, or `ollama:<name>[:tag]` to import from a local Ollama install.
    ImportModel {
        source: String,
    },
    /// `ollama:<name>[:tag]` pulls from the registry; other specs resolve on Hugging Face.
    Pull {
        spec: String,
    },
    Search {
        query: String,
    },
    CancelDownload,
    /// Switch provider: `local`, `ollama`, `openai`, `anthropic`, `compat`.
    SetProvider {
        provider: String,
        model: Option<String>,
    },
    SetMode(AgentMode),
    SaveSetting {
        section: String,
        key: String,
        value: SettingValue,
    },
    /// Answer to an [`Event::Choice`]. `None` means dismissed.
    Answer {
        id: u64,
        choice: Option<usize>,
    },
    /// Diagnostics. The UI passes its own terminal findings to include.
    Doctor {
        terminal: Vec<ReportLine>,
    },
    Status,
    Context,
    Recommend,
    InstallBackend,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum InferenceState {
    /// No backend running / nothing selected.
    Offline,
    /// Spawning backend and loading weights. Progress is not measurable, so the
    /// UI shows an indeterminate animation.
    Loading {
        model: String,
        detail: String,
    },
    Ready,
    Generating,
    Error(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    pub provider: String,
    pub model: Option<String>,
    pub model_detail: Option<String>,
    pub backend: Option<String>,
    pub mode: AgentMode,
    pub context_size: u32,
    pub context_used: u32,
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelSummary {
    pub id: String,
    pub name: String,
    pub quant: String,
    pub arch: String,
    pub size: u64,
    pub context_length: Option<u64>,
    pub est_memory: Option<u64>,
    pub fit: Fit,
    pub source: String,
    pub path: PathBuf,
    pub capabilities: Vec<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    Comfortable,
    Tight,
    TooLarge,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// Spec usable with `/models pull`.
    pub spec: String,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadProgress {
    pub label: String,
    pub downloaded: u64,
    pub total: Option<u64>,
    pub bytes_per_sec: f64,
    pub phase: DownloadPhase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadPhase {
    Connecting,
    Downloading,
    Verifying,
    Finished,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReportStatus {
    Ok,
    Warn,
    Fail,
    Info,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportLine {
    pub status: ReportStatus,
    pub label: String,
    pub detail: String,
}

/// Runtime -> UI.
#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Snapshot),
    Inference(InferenceState),
    /// A new assistant turn begins.
    TurnStart,
    Delta(StreamEvent),
    TurnEnd {
        completion_tokens: u64,
        elapsed_ms: u64,
        tokens_per_second: Option<f64>,
        cancelled: bool,
    },
    Notice {
        level: NoticeLevel,
        text: String,
    },
    Models(Vec<ModelSummary>),
    SearchResults {
        query: String,
        hits: Vec<SearchHit>,
    },
    Download(DownloadProgress),
    Report {
        title: String,
        art: Option<&'static str>,
        lines: Vec<ReportLine>,
    },
    /// Ask the user to choose. The UI answers with [`Request::Answer`].
    /// `preview` is a diff or command shown verbatim (diff lines colored).
    Choice {
        id: u64,
        title: String,
        body: Vec<String>,
        options: Vec<String>,
        preview: Option<String>,
    },
    /// A pending choice is no longer relevant (e.g. the turn was cancelled).
    Dismiss {
        id: u64,
    },
    /// The agent started a tool call.
    ToolStart {
        id: String,
        tool: String,
        title: String,
    },
    /// The tool call finished. `detail` is a diff or output excerpt for the UI.
    ToolEnd {
        id: String,
        ok: bool,
        summary: String,
        detail: Option<String>,
    },
    /// The conversation was reset.
    Cleared,
    /// Runtime finished shutting down; the UI may exit.
    ShutdownComplete,
}
