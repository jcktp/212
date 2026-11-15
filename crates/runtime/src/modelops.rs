//! Model management requests: list, info, import, search, pull, remove.

use std::path::PathBuf;

use common::format;
use common::proto::{DownloadPhase, DownloadProgress, Event, Fit, ModelSummary, NoticeLevel, ReportLine, ReportStatus, SearchHit};
use models::download::{DownloadSpec, download};
use models::hf::{HfClient, HfSpec};
use models::ollama::{self, OllamaRef, RegistryClient};
use models::store::{ModelSource, slug};
use models::{GgufInfo, InstalledModel, catalog, estimate};
use tokio_util::sync::CancellationToken;

use crate::{Internal, PendingAction, Runtime};

#[derive(Debug, Clone)]
pub(crate) struct PlannedFile {
    pub url: String,
    pub dest: PathBuf,
    pub sha256: Option<String>,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct DownloadPlan {
    pub label: String,
    pub dir_id: String,
    pub files: Vec<PlannedFile>,
    pub source: ModelSource,
    pub template: Option<String>,
    pub params: Option<serde_json::Value>,
    pub license: Option<String>,
    pub bearer: Option<String>,
}

impl DownloadPlan {
    fn total(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

#[derive(Debug)]
pub(crate) struct Fetched {
    plan: DownloadPlan,
}

fn rough_need(size: u64) -> u64 {
    // Before download we only know the file size: assume ~15% + 512 MiB for
    // KV cache and buffers at default context. Shown as a rough estimate.
    size + size / 7 + 512 * 1024 * 1024
}

fn fit_label(fit: Fit) -> &'static str {
    match fit {
        Fit::Comfortable => "should fit comfortably",
        Fit::Tight => "tight — close other apps or lower context_size",
        Fit::TooLarge => "likely too large for this machine",
        Fit::Unknown => "unknown",
    }
}

impl Runtime {
    fn hf(&self) -> HfClient {
        let token = crate::credentials::lookup("huggingface", &self.config.models.huggingface_token_env);
        HfClient::new(self.http.clone(), token)
    }

    pub(crate) fn summarize(&self, m: &InstalledModel) -> ModelSummary {
        let ctx = self.config.inference.context_size as u64;
        let est = GgufInfo::read_path(&m.path).ok().map(|info| estimate::estimate(&info, m.size, ctx).total());
        ModelSummary {
            id: m.id.clone(),
            name: m.name.clone(),
            quant: m.quant.clone(),
            arch: m.arch.clone(),
            size: m.size,
            context_length: m.context_length,
            est_memory: est,
            fit: est.map_or(Fit::Unknown, |e| estimate::fit(e, self.hardware.model_memory())),
            source: m.source.label(),
            path: m.path.clone(),
            capabilities: m.capabilities.labels(),
            active: self.active_local.as_deref() == Some(&m.id),
            installed: true,
            install_spec: None,
        }
    }

    /// Installed models first, then models already on disk in an Ollama
    /// store, then curated downloads that suit this machine.
    pub(crate) fn model_catalog(&self) -> Vec<ModelSummary> {
        let mut list: Vec<ModelSummary> = self.store.list().iter().map(|m| self.summarize(m)).collect();
        let mem = self.hardware.model_memory();
        let installed_sources: Vec<ModelSource> = self.store.list().iter().map(|m| m.source.clone()).collect();
        if let Some(dir) = ollama::local_models_dir() {
            for m in ollama::scan_local(&dir).into_iter().filter(|m| m.problem.is_none()) {
                let src = ModelSource::Ollama { name: m.reference.name.clone(), tag: m.reference.tag.clone() };
                if installed_sources.contains(&src) {
                    continue;
                }
                let need = rough_need(m.size);
                list.push(ModelSummary {
                    id: m.reference.display(),
                    name: m.reference.display(),
                    quant: String::new(),
                    arch: String::new(),
                    size: m.size,
                    context_length: None,
                    est_memory: Some(need),
                    fit: estimate::fit(need, mem),
                    source: "on disk (Ollama) · no download".into(),
                    path: m.model_blob.clone(),
                    capabilities: vec![],
                    active: false,
                    installed: false,
                    install_spec: Some(format!("ollama:{}", m.reference.display())),
                });
            }
        }
        for e in catalog::recommend(mem) {
            let repo = e.spec.split(':').next().unwrap_or(e.spec);
            if installed_sources.iter().any(|s| matches!(s, ModelSource::HuggingFace { repo: r, .. } if r == repo)) {
                continue;
            }
            let size = (e.approx_gib as f64 * (1u64 << 30) as f64) as u64;
            list.push(ModelSummary {
                id: e.title.to_string(),
                name: e.title.to_string(),
                quant: String::new(),
                arch: String::new(),
                size,
                context_length: None,
                est_memory: Some(rough_need(size)),
                fit: estimate::fit(rough_need(size), mem),
                source: format!("download ~{:.1} GB · {}", e.approx_gib, e.blurb),
                path: Default::default(),
                capabilities: if e.tools { vec!["tools".into()] } else { vec![] },
                active: false,
                installed: false,
                install_spec: Some(e.spec.to_string()),
            });
        }
        list
    }

    /// Switch to a model, installing it first if needed.
    pub(crate) async fn use_model(&mut self, spec: &str) {
        let spec = spec.trim();
        if let Ok(m) = self.store.find(spec) {
            let id = m.id.clone();
            if self.active_local.as_deref() == Some(&id) && self.provider_name == "local" {
                return self.notice(NoticeLevel::Info, format!("{id} is already loaded.")).await;
            }
            if self.provider_name != "local" {
                self.provider_name = "local".into();
                self.save_setting("inference", "provider", "local".into());
            }
            return self.load_model(&id).await;
        }
        // Anything installed from here on is loaded right away.
        self.load_next_install = true;
        if spec.starts_with("ollama:")
            && let Some(r) = OllamaRef::parse(spec)
            && let Some(local) =
                ollama::local_models_dir().map(|d| ollama::scan_local(&d)).unwrap_or_default().into_iter().find(|m| m.reference == r)
        {
            return self.import_ollama_local(local).await;
        }
        self.pull(spec).await;
    }

    pub(crate) async fn list_models(&mut self) {
        let list = self.model_catalog();
        self.emit(Event::Models(list)).await;
    }

    pub(crate) async fn model_info(&mut self, query: &str) {
        let query = if query.is_empty() { self.active_local.clone().unwrap_or_default() } else { query.to_string() };
        let m = match self.store.find(&query) {
            Ok(m) => m.clone(),
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        let s = self.summarize(&m);
        let info = |label: &str, detail: String| ReportLine { status: ReportStatus::Info, label: label.into(), detail };
        let mut lines = vec![
            info("Name", m.name.clone()),
            info("Id", m.id.clone()),
            info("Architecture", format!("{}{}", m.arch, m.size_label.as_ref().map(|l| format!(" · {l}")).unwrap_or_default())),
            info("Quantization", m.quant.clone()),
            info("File size", format::bytes(m.size)),
            info("Max context", m.context_length.map_or("unknown".into(), |c| format!("{c} tokens"))),
            info("Source", m.source.label()),
            info("License", m.license.clone().unwrap_or_else(|| "not declared in file".into())),
            info("Location", m.path.display().to_string()),
        ];
        if let Some(e) = s.est_memory {
            lines.push(ReportLine {
                status: match s.fit {
                    Fit::Comfortable => ReportStatus::Ok,
                    Fit::Tight => ReportStatus::Warn,
                    Fit::TooLarge => ReportStatus::Fail,
                    Fit::Unknown => ReportStatus::Info,
                },
                label: "Est. memory".into(),
                detail: format!("~{} at {} ctx — {}", format::bytes(e), self.config.inference.context_size, fit_label(s.fit)),
            });
        }
        let caps = &m.capabilities;
        lines.push(ReportLine {
            status: if caps.tool_calling { ReportStatus::Ok } else { ReportStatus::Warn },
            label: "Tool calling".into(),
            detail: if caps.tool_calling {
                "chat template declares tool support (reliability varies by model size)".into()
            } else {
                "no tool support in chat template — agent features will be limited".into()
            },
        });
        lines.push(info("Reasoning", if caps.reasoning { "emits separable reasoning".into() } else { "no".into() }));
        if !caps.has_chat_template {
            lines.push(ReportLine {
                status: ReportStatus::Warn,
                label: "Chat template".into(),
                detail: "missing — llama.cpp falls back to a generic format; output quality may suffer".into(),
            });
        }
        self.emit(Event::Report { title: format!("Model · {}", m.id), art: None, lines }).await;
    }

    /// Ask before removing; the answer calls [`Self::remove_model_confirmed`].
    pub(crate) async fn remove_model(&mut self, query: &str) {
        let m = match self.store.find(query) {
            Ok(m) => m.clone(),
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        let what = if m.managed {
            let original = match &m.source {
                ModelSource::Local { original } if m.path != *original => " (a hard link: the original file elsewhere is kept)",
                ModelSource::Ollama { .. } => " (a hard link: Ollama's own copy is kept)",
                _ => "",
            };
            format!("Deletes {} from 212's model store{original}.", format::bytes(m.size))
        } else {
            format!("Forgets the entry; the file at {} is left untouched.", m.path.display())
        };
        let active = if self.active_local.as_deref() == Some(&m.id) { " It is currently loaded and will be unloaded." } else { "" };
        self.choice(
            crate::PendingAction::RemoveModel(m.id.clone()),
            format!("Remove {}?", m.id),
            vec![format!("{what}{active}")],
            vec!["Remove".into(), "Keep".into()],
        )
        .await;
    }

    pub(crate) async fn remove_model_confirmed(&mut self, id: &str) {
        let id = id.to_string();
        if self.active_local.as_deref() == Some(&id) {
            self.unload().await;
        }
        match self.store.remove(&id) {
            Ok(m) => {
                let what = if m.managed { "files deleted" } else { "external file left untouched" };
                if self.config.inference.default_model.as_deref() == Some(&id) {
                    self.config.inference.default_model = None;
                }
                self.notice(NoticeLevel::Success, format!("Removed {id} ({what}).")).await;
            }
            Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
        }
    }

    pub(crate) async fn import(&mut self, source: &str) {
        let source = source.trim();
        if source.is_empty() || source == "ollama" {
            return self.list_ollama_local().await;
        }
        if source.starts_with("ollama:") {
            let Some(r) = OllamaRef::parse(source) else {
                return self.notice(NoticeLevel::Error, format!("'{source}' is not a valid Ollama model name.")).await;
            };
            let local = ollama::local_models_dir().map(|d| ollama::scan_local(&d)).unwrap_or_default();
            return match local.into_iter().find(|m| m.reference == r) {
                Some(m) => self.import_ollama_local(m).await,
                None => {
                    self.notice(
                        NoticeLevel::Warn,
                        format!("{} is not in the local Ollama store. Use /models pull {source} to download it.", r.display()),
                    )
                    .await
                }
            };
        }
        let expanded = expand_tilde(source);
        let path = if expanded.is_absolute() { expanded } else { self.cwd.join(expanded) };
        match self.store.import_file(&path) {
            Ok(m) => {
                let how = if m.managed { "hard-linked into the model store" } else { "referenced in place" };
                self.notice(
                    NoticeLevel::Success,
                    format!("Imported {} ({}, {}) — {how}. Load it with /models switch {}", m.id, m.quant, format::bytes(m.size), m.id),
                )
                .await;
                self.offer_load(&m.id).await;
            }
            Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
        }
    }

    async fn offer_load(&mut self, id: &str) {
        if std::mem::take(&mut self.load_next_install) {
            return self.load_model(id).await;
        }
        if !self.autoload {
            return;
        }
        if self.active_local.is_none() && self.loading.is_none() && self.provider_name == "local" {
            self.load_model(id).await;
        } else {
            self.choice(PendingAction::LoadModel(id.into()), format!("Load {id} now?"), vec![], vec!["Load".into(), "Later".into()]).await;
        }
    }

    async fn list_ollama_local(&mut self) {
        let Some(dir) = ollama::local_models_dir() else {
            return self.notice(NoticeLevel::Info, "No Ollama model directory found.").await;
        };
        let found = ollama::scan_local(&dir);
        if found.is_empty() {
            return self.notice(NoticeLevel::Info, format!("No Ollama models found in {}.", dir.display())).await;
        }
        let hits = found
            .iter()
            .map(|m| SearchHit {
                spec: format!("ollama:{}", m.reference.display()),
                title: m.reference.display(),
                detail: match &m.problem {
                    Some(p) => format!("cannot import: {p}"),
                    None => format!("{} · on disk, imports without downloading", format::bytes(m.size)),
                },
            })
            .collect();
        self.emit(Event::SearchResults { query: "local Ollama models".into(), hits }).await;
    }

    pub(crate) async fn import_ollama_local(&mut self, m: ollama::LocalOllamaModel) {
        if let Some(p) = &m.problem {
            return self.notice(NoticeLevel::Error, format!("Cannot import {}: {p}", m.reference.display())).await;
        }
        let source = ModelSource::Ollama { name: m.reference.name.clone(), tag: m.reference.tag.clone() };
        if let Some(existing) = self.store.list().iter().find(|x| x.source == source) {
            let id = existing.id.clone();
            return self.notice(NoticeLevel::Info, format!("Already imported as {id}.")).await;
        }
        let dir_id = slug(&format!("{}-{}", m.reference.name, m.reference.tag));
        let dir = self.store.model_dir(&dir_id);
        let link = dir.join("model.gguf");
        let (path, managed) = match std::fs::create_dir_all(&dir).and_then(|_| std::fs::hard_link(&m.model_blob, &link)) {
            Ok(()) => (link, true),
            Err(_) => {
                let _ = std::fs::remove_dir(&dir);
                (m.model_blob.clone(), false)
            }
        };
        match self.store.describe(&path, source, managed) {
            Ok(mut model) => {
                if managed {
                    model.id = dir_id.clone();
                }
                model.upstream_template = m.template.clone();
                model.upstream_params = m.params.clone();
                if model.capabilities.vision || m.projector.is_some() {
                    model.projector = m.projector.clone();
                }
                match self.store.register(model) {
                    Ok(model) => {
                        let id = model.id.clone();
                        self.notice(
                            NoticeLevel::Success,
                            format!("Imported {} from the local Ollama store as {id} (no download needed).", m.reference.display()),
                        )
                        .await;
                        self.offer_load(&id).await;
                    }
                    Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
                }
            }
            Err(e) => {
                if managed {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                self.notice(
                    NoticeLevel::Error,
                    format!(
                        "{} can't be used by llama.cpp: {e}. Some Ollama models use formats llama.cpp doesn't load.",
                        m.reference.display()
                    ),
                )
                .await
            }
        }
    }

    pub(crate) async fn search(&mut self, query: &str) {
        let query = query.trim().to_string();
        if query.is_empty() {
            return self.recommend().await;
        }
        let q = query.to_lowercase();
        let mut hits: Vec<SearchHit> = catalog::CATALOG
            .iter()
            .filter(|e| e.title.to_lowercase().contains(&q) || e.spec.to_lowercase().contains(&q))
            .map(|e| SearchHit {
                spec: e.spec.into(),
                title: format!("★ {}", e.title),
                detail: format!("~{:.1} GB · {}", e.approx_gib, e.blurb),
            })
            .collect();
        if let Some(dir) = ollama::local_models_dir() {
            for m in ollama::scan_local(&dir).into_iter().filter(|m| m.problem.is_none() && m.reference.display().contains(&q)) {
                hits.push(SearchHit {
                    spec: format!("ollama:{}", m.reference.display()),
                    title: m.reference.display(),
                    detail: format!("{} · already on disk (Ollama) — imports without downloading", format::bytes(m.size)),
                });
            }
        }
        if OllamaRef::parse(&query).is_some() {
            hits.push(SearchHit {
                spec: format!("ollama:{query}"),
                title: format!("ollama:{query}"),
                detail: "pull from the Ollama registry (if that name exists)".into(),
            });
        }
        self.notice(NoticeLevel::Info, format!("Searching Hugging Face for GGUF models matching '{query}'…")).await;
        let hf = self.hf();
        let events = self.events.clone();
        tokio::spawn(async move {
            match hf.search(&query, 15).await {
                Ok(models) => {
                    for m in models {
                        let gated = !matches!(m.gated, serde_json::Value::Bool(false) | serde_json::Value::Null);
                        hits.push(SearchHit {
                            spec: m.id.clone(),
                            title: m.id.clone(),
                            detail: format!("{} downloads · {} likes{}", m.downloads, m.likes, if gated { " · gated" } else { "" }),
                        });
                    }
                }
                Err(e) => {
                    let _ = events.send(Event::Notice { level: NoticeLevel::Warn, text: e.to_string() }).await;
                }
            }
            let _ = events.send(Event::SearchResults { query, hits }).await;
        });
    }

    pub(crate) async fn recommend(&mut self) {
        let mem = self.hardware.model_memory();
        let mut hits: Vec<SearchHit> = catalog::recommend(mem)
            .into_iter()
            .map(|e| SearchHit {
                spec: e.spec.into(),
                title: e.title.into(),
                detail: format!("~{:.1} GB · {}{}", e.approx_gib, e.blurb, if e.tools { " · tools" } else { "" }),
            })
            .collect();
        if let Some(dir) = ollama::local_models_dir() {
            for m in ollama::scan_local(&dir).into_iter().filter(|m| m.problem.is_none()) {
                if rough_need(m.size) < mem {
                    hits.insert(
                        0,
                        SearchHit {
                            spec: format!("ollama:{}", m.reference.display()),
                            title: m.reference.display(),
                            detail: format!("{} · already on disk (Ollama) — no download", format::bytes(m.size)),
                        },
                    );
                }
            }
        }
        let title = format!("recommended for {} {}", format::bytes(self.hardware.total_mem), self.hardware.accel.label());
        if hits.is_empty() {
            return self
                .notice(NoticeLevel::Warn, "This machine has too little memory for the curated models; try a cloud provider (/provider).")
                .await;
        }
        self.emit(Event::SearchResults { query: title, hits }).await;
    }

    pub(crate) async fn pull(&mut self, spec: &str) {
        let spec = spec.trim();
        if spec.is_empty() {
            return self.notice(NoticeLevel::Warn, "Usage: /models pull <owner/repo[:QUANT]> or ollama:<name[:tag]>").await;
        }
        if self.download.is_some() {
            return self.notice(NoticeLevel::Warn, "A download is already running (Esc or /models cancel to stop it).").await;
        }
        if spec.starts_with("ollama:") {
            return self.plan_ollama(spec).await;
        }
        let parsed = match HfSpec::parse(spec) {
            Ok(s) => s,
            Err(e) => return self.notice(NoticeLevel::Error, e.to_string()).await,
        };
        self.notice(NoticeLevel::Info, format!("Looking up {} on Hugging Face…", parsed.repo)).await;
        let hf = self.hf();
        let tx = self.internal_tx.clone();
        let events = self.events.clone();
        let store_dir = self.store.dir().to_path_buf();
        let mem = self.hardware.model_memory();
        let installed: Vec<ModelSource> = self.store.list().iter().map(|m| m.source.clone()).collect();
        tokio::spawn(async move {
            let result = async {
                let (files, info) = tokio::join!(hf.gguf_files(&parsed.repo), hf.repo_info(&parsed.repo));
                let files = files.map_err(|e| e.to_string())?;
                let info = info.unwrap_or_default();
                let picked = models::hf::pick(&parsed.repo, &files, parsed.hint.as_deref()).map_err(|e| e.to_string())?;
                let first = &picked[0];
                let source = ModelSource::HuggingFace { repo: parsed.repo.clone(), file: first.path.clone() };
                if installed.contains(&source) {
                    return Err(format!("{} is already installed.", first.path));
                }
                let stem = first.path.rsplit('/').next().unwrap_or(&first.path).trim_end_matches(".gguf");
                let stem =
                    models::hf::split_info(&first.path).map_or(stem.to_string(), |(_, _, p)| p.rsplit('/').next().unwrap_or(p).to_string());
                let dir_id = slug(&stem);
                let dir = store_dir.join(&dir_id);
                let plan = DownloadPlan {
                    label: stem.clone(),
                    dir_id,
                    files: picked
                        .iter()
                        .map(|f| PlannedFile {
                            url: hf.download_url(&parsed.repo, &f.path),
                            dest: dir.join(f.path.rsplit('/').next().unwrap_or(&f.path)),
                            sha256: f.sha256.clone(),
                            size: f.size,
                        })
                        .collect(),
                    source,
                    template: None,
                    params: None,
                    license: info.license(),
                    bearer: hf.token().map(str::to_string),
                };
                let total = plan.total();
                let fit = estimate::fit(rough_need(total), mem);
                let mut body = vec![
                    format!("Repository  {}", parsed.repo),
                    format!(
                        "File        {}{}",
                        picked[0].path,
                        if picked.len() > 1 { format!(" (+{} parts)", picked.len() - 1) } else { String::new() }
                    ),
                    format!("Size        {}", format::bytes(total)),
                    format!("License     {}", plan.license.clone().unwrap_or_else(|| "see model card".into())),
                    format!("Memory      ~{} (rough) — {}", format::bytes(rough_need(total)), fit_label(fit)),
                    format!("Saved to    {}", dir.display()),
                    format!(
                        "Checksum    {}",
                        if picked.iter().all(|f| f.sha256.is_some()) {
                            "SHA-256 (verified after download)"
                        } else {
                            "none published — integrity cannot be verified"
                        }
                    ),
                ];
                if info.is_gated() {
                    body.push("Gated model: accept its terms on huggingface.co and set HF_TOKEN.".into());
                }
                if let Some(free) = inference::hardware::free_space(&store_dir)
                    && free < total + (1 << 30)
                {
                    return Err(format!("Not enough disk space: need {}, {} free.", format::bytes(total), format::bytes(free)));
                }
                Ok((plan, body, total))
            }
            .await;
            match result {
                Ok((plan, body, total)) => {
                    let _ = tx
                        .send(Internal::Choice {
                            action: PendingAction::Download(plan),
                            title: format!("Download {}?", format::bytes(total)),
                            body,
                            options: vec!["Download".into(), "Cancel".into()],
                        })
                        .await;
                }
                Err(e) => {
                    let _ = events.send(Event::Notice { level: NoticeLevel::Error, text: e }).await;
                }
            }
        });
    }

    async fn plan_ollama(&mut self, spec: &str) {
        let Some(r) = OllamaRef::parse(spec) else {
            return self.notice(NoticeLevel::Error, format!("'{spec}' is not a valid Ollama model name.")).await;
        };
        // Prefer a copy already on disk.
        if let Some(local) = ollama::local_models_dir()
            .map(|d| ollama::scan_local(&d))
            .unwrap_or_default()
            .into_iter()
            .find(|m| m.reference == r && m.problem.is_none())
        {
            let size = format::bytes(local.size);
            return self
                .choice(
                    PendingAction::ImportOllamaLocal(local),
                    format!("{} is already on disk", r.display()),
                    vec![format!("Found in the local Ollama store ({size}). Importing links the file; nothing is downloaded.")],
                    vec!["Import from disk".into(), "Cancel".into()],
                )
                .await;
        }
        self.notice(NoticeLevel::Info, format!("Fetching manifest for {} from the Ollama registry…", r.display())).await;
        let client = RegistryClient::new(self.http.clone());
        let (tx, events) = (self.internal_tx.clone(), self.events.clone());
        let store_dir = self.store.dir().to_path_buf();
        let mem = self.hardware.model_memory();
        tokio::spawn(async move {
            let result = async {
                let manifest = client.manifest(&r).await.map_err(|e| e.to_string())?;
                let layer = manifest.compatibility()?.clone();
                let sha = ollama::digest_hex(&layer.digest).ok_or("manifest has an invalid digest")?.to_string();
                let template = match manifest.layer(ollama::MEDIA_TEMPLATE) {
                    Some(l) => client.small_blob(&r, l).await.ok(),
                    None => None,
                };
                let params = match manifest.layer(ollama::MEDIA_PARAMS) {
                    Some(l) => client.small_blob(&r, l).await.ok().and_then(|t| serde_json::from_str(&t).ok()),
                    None => None,
                };
                let license = match manifest.layer(ollama::MEDIA_LICENSE) {
                    Some(l) => client
                        .small_blob(&r, l)
                        .await
                        .ok()
                        .and_then(|t| t.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().chars().take(60).collect())),
                    None => None,
                };
                let dir_id = slug(&format!("{}-{}", r.name, r.tag));
                let plan = DownloadPlan {
                    label: r.display(),
                    dir_id: dir_id.clone(),
                    files: vec![PlannedFile {
                        url: client.blob_url(&r, &layer.digest),
                        dest: store_dir.join(&dir_id).join("model.gguf"),
                        sha256: Some(sha),
                        size: layer.size,
                    }],
                    source: ModelSource::Ollama { name: r.name.clone(), tag: r.tag.clone() },
                    template,
                    params,
                    license,
                    bearer: None,
                };
                let fit = estimate::fit(rough_need(layer.size), mem);
                let mut body = vec![
                    format!("Model       {}", r.display()),
                    format!("Size        {}", format::bytes(layer.size)),
                    format!("License     {}", plan.license.clone().unwrap_or_else(|| "see ollama.com".into())),
                    format!("Memory      ~{} (rough) — {}", format::bytes(rough_need(layer.size)), fit_label(fit)),
                    format!("Saved to    {}", store_dir.join(&dir_id).display()),
                    "Checksum    SHA-256 (content-addressed, verified after download)".into(),
                ];
                if manifest.layer(ollama::MEDIA_PROJECTOR).is_some() {
                    body.push("Note: the vision projector is not downloaded; text chat only.".into());
                }
                if let Some(free) = inference::hardware::free_space(&store_dir)
                    && free < layer.size + (1 << 30)
                {
                    return Err(format!("Not enough disk space: need {}, {} free.", format::bytes(layer.size), format::bytes(free)));
                }
                Ok::<_, String>((plan, body, layer.size))
            }
            .await;
            match result {
                Ok((plan, body, size)) => {
                    let _ = tx
                        .send(Internal::Choice {
                            action: PendingAction::Download(plan),
                            title: format!("Download {}?", format::bytes(size)),
                            body,
                            options: vec!["Download".into(), "Cancel".into()],
                        })
                        .await;
                }
                Err(e) => {
                    let _ = events.send(Event::Notice { level: NoticeLevel::Error, text: e }).await;
                }
            }
        });
    }

    pub(crate) async fn start_download(&mut self, plan: DownloadPlan) {
        if self.download.is_some() {
            return self.notice(NoticeLevel::Warn, "A download is already running.").await;
        }
        let cancel = CancellationToken::new();
        self.download = Some(cancel.clone());
        let (client, tx, events) = (self.http.clone(), self.internal_tx.clone(), self.events.clone());
        tokio::spawn(async move {
            let total: u64 = plan.total();
            let mut done_before = 0u64;
            let mut result = Ok(());
            for f in &plan.files {
                let spec = DownloadSpec {
                    url: f.url.clone(),
                    dest: f.dest.clone(),
                    sha256: f.sha256.clone(),
                    size: Some(f.size),
                    bearer: plan.bearer.clone(),
                };
                let ev = events.clone();
                let label = plan.label.clone();
                let base = done_before;
                let r = download(&client, &spec, &cancel, move |p| {
                    let _ = ev.try_send(Event::Download(DownloadProgress {
                        label: label.clone(),
                        downloaded: base + p.downloaded,
                        total: Some(total),
                        bytes_per_sec: p.bytes_per_sec,
                        phase: if p.verifying { DownloadPhase::Verifying } else { DownloadPhase::Downloading },
                    }));
                })
                .await;
                if let Err(e) = r {
                    result = Err(e);
                    break;
                }
                done_before += f.size;
            }
            let phase = match &result {
                Ok(()) => DownloadPhase::Finished,
                Err(models::download::DownloadError::Cancelled) => DownloadPhase::Cancelled,
                Err(_) => DownloadPhase::Failed,
            };
            let _ = events
                .send(Event::Download(DownloadProgress {
                    label: plan.label.clone(),
                    downloaded: done_before,
                    total: Some(total),
                    bytes_per_sec: 0.0,
                    phase,
                }))
                .await;
            let _ = tx.send(Internal::Downloaded { result: result.map(|_| Fetched { plan }).map_err(|e| e.to_string()) }).await;
        });
    }

    pub(crate) async fn on_downloaded(&mut self, result: Result<Fetched, String>) {
        self.download = None;
        let fetched = match result {
            Ok(f) => f,
            Err(e) => return self.notice(NoticeLevel::Error, format!("Download stopped: {e}. Run the same pull again to resume.")).await,
        };
        let plan = fetched.plan;
        let first = plan.files[0].dest.clone();
        match self.store.describe(&first, plan.source.clone(), true) {
            Ok(mut model) => {
                if !self.store.list().iter().any(|m| m.id == plan.dir_id) {
                    model.id = plan.dir_id.clone();
                }
                model.sha256 = plan.files[0].sha256.clone();
                model.upstream_template = plan.template;
                model.upstream_params = plan.params;
                if model.license.is_none() {
                    model.license = plan.license;
                }
                match self.store.register(model) {
                    Ok(m) => {
                        let (id, size) = (m.id.clone(), m.size);
                        self.notice(NoticeLevel::Success, format!("Installed {id} ({}).", format::bytes(size))).await;
                        self.offer_load(&id).await;
                    }
                    Err(e) => self.notice(NoticeLevel::Error, e.to_string()).await,
                }
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(self.store.model_dir(&plan.dir_id));
                self.notice(NoticeLevel::Error, format!("Downloaded file is not usable: {e}")).await;
            }
        }
    }
}

fn expand_tilde(p: &str) -> PathBuf {
    let unquoted = p.trim_matches(|c| c == '"' || c == '\'');
    if let Some(rest) = unquoted.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(unquoted.replace("\\ ", " "))
}
