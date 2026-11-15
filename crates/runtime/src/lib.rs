//! The 212 runtime.
//!
//! A single task owns all mutable state (conversation, model store, backend
//! process, active provider) and processes [`Request`]s from the UI one at a
//! time. Anything slow — model loading, generation, downloads — runs in a
//! spawned task that streams [`Event`]s to the UI and reports completion back
//! to the runtime through an internal channel. The runtime therefore never
//! blocks on I/O for long, and Cancel is always handled promptly.

mod attach;
mod chat;
mod context;
mod doctor;
mod local;
mod modelops;
mod providers_setup;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use common::proto::{AgentMode, Event, InferenceState, NoticeLevel, Request, SettingValue, Snapshot};
use common::{AppPaths, Message};
use config::Config;
use inference::{Backend, Hardware, LlamaServer};
use models::ModelStore;
use providers::Provider;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use context::estimate_tokens;

/// Work that finished in a background task and must update runtime state.
enum Internal {
    Loaded { id: String, result: Result<LlamaServer, String> },
    Generated { message: Option<Message>, prompt_tokens: Option<u64>, completion_tokens: u64, failed_empty: bool },
    Downloaded { result: Result<modelops::Fetched, String> },
    BackendInstalled { result: Result<PathBuf, String> },
    BackendProbed { backend: Backend },
    Choice { action: PendingAction, title: String, body: Vec<String>, options: Vec<String> },
}

/// Something waiting for the user's answer to an [`Event::Choice`].
enum PendingAction {
    Download(modelops::DownloadPlan),
    InstallBackend(Box<inference::install::Release>, inference::install::Asset),
    OfferInstallBackend,
    ImportOllamaLocal(models::ollama::LocalOllamaModel),
    LoadModel(String),
}

pub struct Runtime {
    paths: AppPaths,
    config: Config,
    config_path: PathBuf,
    hardware: Hardware,
    store: ModelStore,
    events: mpsc::Sender<Event>,
    internal_tx: mpsc::Sender<Internal>,
    internal_rx: Option<mpsc::Receiver<Internal>>,
    http: reqwest::Client,

    provider: Option<Arc<dyn Provider>>,
    provider_name: String,
    model_name: Option<String>,
    model_detail: Option<String>,

    backend: Option<Backend>,
    server: Option<LlamaServer>,
    active_local: Option<String>,
    loading: Option<(String, CancellationToken)>,

    messages: Vec<Message>,
    mode: AgentMode,
    generation: Option<CancellationToken>,
    queued_send: Option<String>,
    download: Option<CancellationToken>,
    pending: HashMap<u64, PendingAction>,
    next_choice: u64,
    context_used: u32,
    cwd: PathBuf,
    /// Interactive sessions load models automatically (on start, after import).
    autoload: bool,
}

pub struct RuntimeOptions {
    pub paths: AppPaths,
    pub config: Config,
    pub config_path: PathBuf,
    pub cwd: PathBuf,
    /// Load the configured default model automatically.
    pub autoload: bool,
}

impl Runtime {
    pub fn new(opts: RuntimeOptions, events: mpsc::Sender<Event>) -> Result<Self, String> {
        let models_dir = match opts.config.models.directory.as_str() {
            "auto" => opts.paths.default_models_dir(),
            p => PathBuf::from(p),
        };
        let store = ModelStore::open(&models_dir).map_err(|e| format!("model store at {}: {e}", models_dir.display()))?;
        let (internal_tx, internal_rx) = mpsc::channel(64);
        let mode = AgentMode::parse(&opts.config.agent.default_mode).unwrap_or_default();
        let backend = inference::backend::discover(opts.config.inference.server_path.as_deref(), &opts.paths.backends_dir());
        Ok(Self {
            provider_name: opts.config.inference.provider.clone(),
            paths: opts.paths,
            config: opts.config,
            config_path: opts.config_path,
            hardware: inference::hardware::detect(),
            store,
            events,
            internal_tx,
            internal_rx: Some(internal_rx),
            http: models::http_client(),
            provider: None,
            model_name: None,
            model_detail: None,
            backend,
            server: None,
            active_local: None,
            loading: None,
            messages: Vec::new(),
            mode,
            generation: None,
            queued_send: None,
            download: None,
            pending: HashMap::new(),
            next_choice: 1,
            context_used: 0,
            cwd: opts.cwd,
            autoload: opts.autoload,
        })
    }

    /// Use a specific provider (tests and embedding). Bypasses the local backend.
    #[doc(hidden)]
    pub fn with_provider(mut self, provider: Arc<dyn Provider>, model: &str) -> Self {
        self.provider = Some(provider);
        self.model_name = Some(model.to_string());
        self
    }

    /// Run until a [`Request::Shutdown`] arrives or the UI goes away.
    pub async fn run(mut self, mut requests: mpsc::Receiver<Request>, autoload: bool) {
        let mut internal = self.internal_rx.take().expect("run called once");
        self.startup(autoload).await;
        loop {
            tokio::select! {
                req = requests.recv() => match req {
                    Some(Request::Shutdown) | None => break,
                    Some(r) => self.handle(r).await,
                },
                Some(msg) = internal.recv() => self.on_internal(msg).await,
            }
        }
        self.shutdown().await;
        let _ = self.events.send(Event::ShutdownComplete).await;
    }

    async fn startup(&mut self, autoload: bool) {
        let reaped = inference::server::reap_stale(&self.paths.run_dir());
        if reaped > 0 {
            self.notice(NoticeLevel::Info, format!("Stopped {reaped} inference process(es) left behind by a previous session.")).await;
        }
        if let Some(b) = self.backend.clone() {
            let tx = self.internal_tx.clone();
            tokio::spawn(async move {
                let version = inference::backend::probe_version(&b.path).await;
                let _ = tx.send(Internal::BackendProbed { backend: Backend { version, ..b } }).await;
            });
        }
        if self.provider_name == "local" {
            let default = self.config.inference.default_model.clone();
            match default {
                Some(id) if autoload && self.store.find(&id).is_ok() => self.load_model(&id).await,
                _ => {
                    self.emit(Event::Inference(InferenceState::Offline)).await;
                }
            }
        } else {
            let name = self.provider_name.clone();
            self.set_provider(&name, None).await;
        }
        self.snapshot().await;
    }

    async fn shutdown(&mut self) {
        if let Some(c) = self.generation.take() {
            c.cancel();
        }
        if let Some(c) = self.download.take() {
            c.cancel();
        }
        if let Some((_, c)) = self.loading.take() {
            c.cancel();
        }
        if let Some(s) = self.server.take() {
            s.shutdown().await;
        }
    }

    async fn emit(&self, e: Event) {
        let _ = self.events.send(e).await;
    }

    async fn notice(&self, level: NoticeLevel, text: impl Into<String>) {
        self.emit(Event::Notice { level, text: text.into() }).await;
    }

    async fn snapshot(&self) {
        let s = Snapshot {
            provider: self.provider_name.clone(),
            model: self.model_name.clone(),
            model_detail: self.model_detail.clone(),
            backend: self.backend.as_ref().map(|b| {
                let v = b.version.clone().unwrap_or_else(|| "llama.cpp".into());
                format!("llama.cpp {v} · {}", self.hardware.accel.label()).replace("llama.cpp llama.cpp", "llama.cpp")
            }),
            mode: self.mode,
            context_size: self.context_size(),
            context_used: self.context_used,
            cwd: self.cwd.display().to_string(),
        };
        self.emit(Event::Snapshot(s)).await;
    }

    fn context_size(&self) -> u32 {
        match (&self.server, self.provider_name.as_str()) {
            (Some(s), "local") => s.ctx_size,
            _ => self.config.inference.context_size,
        }
    }

    fn ask(&mut self, action: PendingAction) -> u64 {
        let id = self.next_choice;
        self.next_choice += 1;
        self.pending.insert(id, action);
        id
    }

    async fn choice(&mut self, action: PendingAction, title: String, body: Vec<String>, options: Vec<String>) {
        let id = self.ask(action);
        self.emit(Event::Choice { id, title, body, options }).await;
    }

    fn save_setting(&mut self, section: &str, key: &str, value: toml_edit::Value) {
        if let Err(e) = config::set_value(&self.config_path, section, key, value) {
            tracing::warn!("could not persist {section}.{key}: {e}");
        }
    }

    async fn handle(&mut self, req: Request) {
        match req {
            Request::Send { text } => self.send(text).await,
            Request::Cancel => {
                if let Some(c) = self.generation.take() {
                    c.cancel();
                } else if let Some((_, c)) = self.loading.take() {
                    c.cancel();
                } else if let Some(c) = self.download.take() {
                    c.cancel();
                }
                self.queued_send = None;
            }
            Request::NewConversation => {
                if let Some(c) = self.generation.take() {
                    c.cancel();
                }
                self.messages.clear();
                self.context_used = 0;
                self.emit(Event::Cleared).await;
                self.snapshot().await;
            }
            Request::LoadModel { id } => self.load_model(&id).await,
            Request::UnloadModel => self.unload().await,
            Request::ListModels => self.list_models().await,
            Request::ModelInfo { id } => self.model_info(&id).await,
            Request::RemoveModel { id } => self.remove_model(&id).await,
            Request::ImportModel { source } => self.import(&source).await,
            Request::Pull { spec } => self.pull(&spec).await,
            Request::Search { query } => self.search(&query).await,
            Request::Recommend => self.recommend().await,
            Request::CancelDownload => {
                if let Some(c) = self.download.take() {
                    c.cancel();
                } else {
                    self.notice(NoticeLevel::Info, "No download in progress.").await;
                }
            }
            Request::SetProvider { provider, model } => self.set_provider(&provider, model).await,
            Request::SetMode(m) => {
                self.mode = m;
                self.snapshot().await;
                if m != AgentMode::Chat {
                    self.notice(
                        NoticeLevel::Info,
                        format!(
                            "Mode: {}. Tool use (file access, commands) arrives in the agent milestone; for now the model can only chat.",
                            m.as_str()
                        ),
                    )
                    .await;
                }
            }
            Request::SaveSetting { section, key, value } => {
                let v: toml_edit::Value = match value {
                    SettingValue::Bool(b) => b.into(),
                    SettingValue::Int(i) => i.into(),
                    SettingValue::Str(s) => s.into(),
                };
                self.save_setting(&section, &key, v);
            }
            Request::Answer { id, choice } => self.answer(id, choice).await,
            Request::Doctor { terminal } => self.doctor(terminal).await,
            Request::Status => self.status().await,
            Request::Context => self.context_report().await,
            Request::InstallBackend => self.offer_backend_install().await,
            Request::Shutdown => {}
        }
    }

    async fn answer(&mut self, id: u64, choice: Option<usize>) {
        let Some(action) = self.pending.remove(&id) else { return };
        let accepted = choice == Some(0);
        match action {
            PendingAction::Download(plan) if accepted => self.start_download(plan).await,
            PendingAction::InstallBackend(release, asset) if accepted => self.install_backend(*release, asset).await,
            PendingAction::OfferInstallBackend if accepted => self.offer_backend_install().await,
            PendingAction::ImportOllamaLocal(m) if accepted => self.import_ollama_local(m).await,
            PendingAction::LoadModel(id) if accepted => self.load_model(&id).await,
            PendingAction::Download(_) => self.notice(NoticeLevel::Info, "Download skipped.").await,
            _ => {}
        }
    }

    async fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Loaded { id, result } => self.on_loaded(id, result).await,
            Internal::Generated { message, prompt_tokens, completion_tokens, failed_empty } => {
                self.generation = None;
                if failed_empty && self.messages.last().is_some_and(|m| m.role == common::Role::User) {
                    // Nothing came back: drop the user turn so a retry doesn't duplicate it.
                    self.messages.pop();
                }
                if let Some(m) = message {
                    self.messages.push(m);
                }
                if let Some(p) = prompt_tokens {
                    self.context_used = (p + completion_tokens) as u32;
                }
                self.snapshot().await;
                if self.provider.is_some() && self.loading.is_none() {
                    self.emit(Event::Inference(InferenceState::Ready)).await;
                }
            }
            Internal::Downloaded { result } => self.on_downloaded(result).await,
            Internal::BackendInstalled { result } => self.on_backend_installed(result).await,
            Internal::BackendProbed { backend } => {
                self.backend = Some(backend);
                self.snapshot().await;
            }
            Internal::Choice { action, title, body, options } => self.choice(action, title, body, options).await,
        }
    }
}
