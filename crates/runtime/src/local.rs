//! Managed local inference: starting, switching and stopping llama-server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::format;
use common::proto::{DownloadPhase, DownloadProgress, Event, InferenceState, NoticeLevel};
use inference::server::{LlamaServer, ServerOptions};
use inference::{Backend, BackendSource};
use providers::openai::OpenAiCompatible;
use tokio_util::sync::CancellationToken;

use crate::{Internal, PendingAction, Runtime};

impl Runtime {
    pub(crate) async fn load_model(&mut self, query: &str) {
        let model = match self.store.find(query) {
            Ok(m) => m.clone(),
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        if !model.path.exists() {
            return self
                .notice(NoticeLevel::Error, format!("{} is missing its file ({}). Re-import or remove it.", model.id, model.path.display()))
                .await;
        }
        let Some(backend) = self.backend.clone() else {
            self.choice(
                PendingAction::OfferInstallBackend,
                "Inference backend needed".into(),
                vec![
                    "212 runs local models with llama.cpp, but no llama-server was found.".into(),
                    "It can download the official prebuilt release from github.com/ggml-org/llama.cpp".into(),
                    "and verify its SHA-256 before use. Alternatively set inference.server_path.".into(),
                ],
                vec!["Find and install llama.cpp".into(), "Not now".into()],
            )
            .await;
            return;
        };

        // Cancel whatever is in flight; a new model replaces it.
        if let Some(c) = self.generation.take() {
            c.cancel();
        }
        if let Some((_, c)) = self.loading.take() {
            c.cancel();
        }
        let old = self.server.take();
        self.backend_pid.store(0, std::sync::atomic::Ordering::Relaxed);
        if self.provider_name == "local" {
            self.provider = None;
            self.model_name = None;
        }
        self.active_local = None;

        let ctx = self.config.inference.context_size.min(model.context_length.unwrap_or(u64::MAX).min(u32::MAX as u64) as u32);
        let gpu_layers = self.config.inference.gpu_layers.parse::<u32>().ok();
        let est =
            models::GgufInfo::read_path(&model.path).ok().map(|info| models::estimate::estimate(&info, model.size, ctx as u64).total());
        let detail = format!(
            "{} · {}{} · {}",
            model.quant,
            format::bytes(model.size),
            est.map(|e| format!(" · ~{} RAM", format::bytes(e))).unwrap_or_default(),
            self.hardware.accel.label()
        );
        self.model_detail = Some(format!("{} · {}", model.quant, format::bytes(model.size)));
        self.emit(Event::Inference(InferenceState::Loading { model: model.name.clone(), detail })).await;
        if let Some(e) = est
            && e > self.hardware.model_memory()
        {
            self.notice(
                NoticeLevel::Warn,
                format!(
                    "Estimated memory ({}) exceeds this machine's {} — loading may fail or be very slow.",
                    format::bytes(e),
                    format::bytes(self.hardware.model_memory())
                ),
            )
            .await;
        }

        let opts = ServerOptions {
            binary: backend.path.clone(),
            model: model.path.clone(),
            alias: model.id.clone(),
            ctx_size: ctx,
            gpu_layers,
            threads: self.config.inference.threads,
            log_file: self.paths.log_dir().join("llama-server.log"),
            run_dir: self.paths.run_dir(),
            extra_args: Vec::new(),
        };
        let cancel = CancellationToken::new();
        self.loading = Some((model.id.clone(), cancel.clone()));
        let tx = self.internal_tx.clone();
        let timeout = Duration::from_secs(self.config.inference.load_timeout_secs);
        let id = model.id.clone();
        tokio::spawn(async move {
            if let Some(old) = old {
                old.shutdown().await;
            }
            let result = async {
                let mut server = LlamaServer::spawn(&opts).map_err(|e| e.to_string())?;
                match server.wait_ready(timeout, &cancel).await {
                    Ok(()) => Ok(server),
                    Err(e) => {
                        server.shutdown().await;
                        Err(e.to_string())
                    }
                }
            }
            .await;
            let _ = tx.send(Internal::Loaded { id, result }).await;
        });
    }

    pub(crate) async fn on_loaded(&mut self, id: String, result: Result<LlamaServer, String>) {
        // Ignore results from a load that was superseded.
        let current = matches!(&self.loading, Some((l, _)) if *l == id);
        if !current {
            if let Ok(s) = result {
                s.shutdown().await;
            }
            if self.loading.is_none() && self.server.is_none() {
                self.emit(Event::Inference(InferenceState::Offline)).await;
            }
            return;
        }
        self.loading = None;
        match result {
            Ok(server) => {
                let provider = OpenAiCompatible::new("local", &server.api_base(), Some(server.api_key.clone()), "the local backend");
                self.backend_pid.store(server.pid().unwrap_or(0), std::sync::atomic::Ordering::Relaxed);
                self.server = Some(server);
                self.active_local = Some(id.clone());
                if self.provider_name == "local" {
                    self.provider = Some(Arc::new(provider));
                    self.model_name = Some(id.clone());
                }
                self.sync_session_settings();
                self.save_setting("inference", "default_model", id.clone().into());
                self.config.inference.default_model = Some(id.clone());
                self.emit(Event::Inference(InferenceState::Ready)).await;
                self.notice(NoticeLevel::Success, format!("{id} loaded. Say hello!")).await;
                self.snapshot().await;
                if let Some(text) = self.queued_send.take() {
                    self.send(text).await;
                }
            }
            Err(e) => {
                self.model_detail = None;
                self.queued_send = None;
                self.emit(Event::Inference(InferenceState::Error(e.lines().next().unwrap_or("load failed").to_string()))).await;
                self.notice(NoticeLevel::Error, format!("Could not load {id}: {e}")).await;
                self.snapshot().await;
            }
        }
    }

    pub(crate) async fn unload(&mut self) {
        if let Some((_, c)) = self.loading.take() {
            c.cancel();
        }
        if let Some(c) = self.generation.take() {
            c.cancel();
        }
        match self.server.take() {
            Some(s) => {
                s.shutdown().await;
                self.backend_pid.store(0, std::sync::atomic::Ordering::Relaxed);
                self.active_local = None;
                if self.provider_name == "local" {
                    self.provider = None;
                    self.model_name = None;
                    self.model_detail = None;
                }
                self.emit(Event::Inference(InferenceState::Offline)).await;
                self.notice(NoticeLevel::Info, "Model unloaded; memory released.").await;
                self.snapshot().await;
            }
            None => self.notice(NoticeLevel::Info, "No local model is loaded.").await,
        }
    }

    pub(crate) async fn offer_backend_install(&mut self) {
        self.notice(NoticeLevel::Info, "Looking up the latest llama.cpp release…").await;
        let client = self.http.clone();
        let tx = self.internal_tx.clone();
        let events = self.events.clone();
        tokio::spawn(async move {
            match inference::install::find_release(&client).await {
                Ok((release, asset)) => {
                    let sha = asset.sha256().map(|s| format!("{}…", &s[..12.min(s.len())])).unwrap_or_else(|| "none published".into());
                    let body = vec![
                        format!("Release   {}", release.tag_name),
                        format!("File      {}", asset.name),
                        format!("Size      {}", format::bytes(asset.size)),
                        format!("SHA-256   {sha} (verified before use)"),
                        "Source    github.com/ggml-org/llama.cpp (official releases)".into(),
                    ];
                    let _ = tx
                        .send(Internal::Choice {
                            action: PendingAction::InstallBackend(Box::new(release), asset),
                            title: "Install llama.cpp backend?".into(),
                            body,
                            options: vec!["Download and install".into(), "Cancel".into()],
                        })
                        .await;
                }
                Err(e) => {
                    let _ = events
                        .send(Event::Notice {
                            level: NoticeLevel::Error,
                            text: format!(
                                "{e}. You can install llama.cpp yourself (e.g. `brew install llama.cpp`) — 212 finds llama-server on PATH."
                            ),
                        })
                        .await;
                }
            }
        });
    }

    pub(crate) async fn install_backend(&mut self, release: inference::install::Release, asset: inference::install::Asset) {
        if self.download.is_some() {
            return self.notice(NoticeLevel::Warn, "Another download is in progress.").await;
        }
        let cancel = CancellationToken::new();
        self.download = Some(cancel.clone());
        let (client, tx, events, dir) = (self.http.clone(), self.internal_tx.clone(), self.events.clone(), self.paths.backends_dir());
        let label = format!("llama.cpp {}", release.tag_name);
        tokio::spawn(async move {
            let ev2 = events.clone();
            let l2 = label.clone();
            let result = inference::install::install(&client, &release, &asset, &dir, &cancel, move |p| {
                let _ = ev2.try_send(Event::Download(DownloadProgress {
                    label: l2.clone(),
                    downloaded: p.downloaded,
                    total: p.total,
                    bytes_per_sec: p.bytes_per_sec,
                    phase: if p.verifying { DownloadPhase::Verifying } else { DownloadPhase::Downloading },
                }));
            })
            .await;
            let phase = if result.is_ok() { DownloadPhase::Finished } else { DownloadPhase::Failed };
            let _ = events.send(Event::Download(DownloadProgress { label, downloaded: 0, total: None, bytes_per_sec: 0.0, phase })).await;
            let _ = tx.send(Internal::BackendInstalled { result: result.map_err(|e| e.to_string()) }).await;
        });
    }

    pub(crate) async fn on_backend_installed(&mut self, result: Result<PathBuf, String>) {
        self.download = None;
        match result {
            Ok(path) => {
                let version = inference::backend::probe_version(&path).await;
                if version.is_none() {
                    self.notice(
                        NoticeLevel::Warn,
                        format!("Installed {}, but it did not report a version; it may not run on this system.", path.display()),
                    )
                    .await;
                }
                self.backend = Some(Backend { path, source: BackendSource::Managed, version });
                self.notice(NoticeLevel::Success, "llama.cpp backend installed and verified.").await;
                self.snapshot().await;
                if let Some(id) = self.config.inference.default_model.clone().filter(|id| self.store.find(id).is_ok()) {
                    self.load_model(&id).await;
                }
            }
            Err(e) => self.notice(NoticeLevel::Error, format!("Backend install failed: {e}")).await,
        }
    }
}
