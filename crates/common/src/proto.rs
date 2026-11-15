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
    /// Switch to a model by id or spec, importing or downloading it first if
    /// needed (downloads still ask for confirmation).
    UseModel {
        spec: String,
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
    /// Sessions in this project (`all` = every project).
    ListSessions {
        all: bool,
    },
    /// Resume a session by id (prefix); `None` = most recent in this project.
    ResumeSession {
        id: Option<String>,
    },
    RenameSession {
        title: String,
    },
    DeleteSession {
        id: String,
    },
    SearchHistory {
        query: String,
    },
    /// `md` or `json`; optional output path.
    Export {
        format: String,
        path: Option<String>,
    },
    /// Summarize older history to free context.
    Compact,
    /// Store an API key in the OS keychain.
    SetCredential {
        provider: String,
        secret: String,
    },
    RemoveCredential {
        provider: String,
    },
    /// Enable the header weather for a place name, or disable it.
    SetWeather {
        location: Option<String>,
    },
    ListSkills,
    /// Activate a skill for this conversation.
    UseSkill {
        name: String,
    },
    /// Install skills from a git URL or a local folder.
    InstallSkill {
        source: String,
    },
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
    /// False for models that are available but not in the store yet.
    pub installed: bool,
    /// How to get it when not installed: `ollama:<name>` (on disk) or a download spec.
    pub install_spec: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionItem {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub current: bool,
}

/// A restored conversation entry for display.
#[derive(Debug, Clone, PartialEq)]
pub enum TranscriptItem {
    User(String),
    Assistant { text: String, reasoning: Option<String> },
    Tool { title: String, ok: bool, summary: String },
    Summary(String),
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
    /// A picker of sessions (from /sessions or /history).
    SessionList {
        title: String,
        items: Vec<SessionItem>,
    },
    /// Replace the visible conversation with a restored transcript.
    Transcript {
        title: String,
        items: Vec<TranscriptItem>,
    },
    /// System load for the header (sent every few seconds when enabled).
    Stats {
        cpu_percent: f32,
        mem_used: u64,
        mem_total: u64,
        model_mem: Option<u64>,
    },
    /// Header weather text, or `None` when disabled/unavailable.
    Weather(Option<String>),
    /// Runtime finished shutting down; the UI may exit.
    ShutdownComplete,
}
