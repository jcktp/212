//! Switching between providers at runtime.

use std::sync::Arc;
use std::time::Duration;

use common::proto::{Event, InferenceState, NoticeLevel};
use providers::Provider;
use providers::anthropic::Anthropic;
use providers::openai::OpenAiCompatible;

use crate::Runtime;

fn key_for(provider: &str, var: &str) -> Option<String> {
    crate::credentials::lookup(provider, var)
}

impl Runtime {
    pub(crate) async fn set_provider(&mut self, name: &str, model: Option<String>) {
        let p = self.config.providers.clone();
        let name = name.trim().to_lowercase();
        let built: Result<(Arc<dyn Provider>, Option<String>), String> = match name.as_str() {
            "local" => {
                self.provider_name = "local".into();
                match (&self.server, &self.active_local) {
                    (Some(s), Some(id)) => {
                        let prov = OpenAiCompatible::new("local", &s.api_base(), Some(s.api_key.clone()), "the local backend");
                        Ok((Arc::new(prov), Some(id.clone())))
                    }
                    _ => {
                        self.provider = None;
                        self.model_name = None;
                        self.model_detail = None;
                        self.save_setting("inference", "provider", "local".into());
                        self.emit(Event::Inference(if self.loading.is_some() { InferenceState::Ready } else { InferenceState::Offline }))
                            .await;
                        self.snapshot().await;
                        if let Some(id) = self.config.inference.default_model.clone().filter(|id| self.store.find(id).is_ok()) {
                            if self.loading.is_none() {
                                self.load_model(&id).await;
                            }
                        } else {
                            self.notice(NoticeLevel::Info, "Provider: local. Pick a model with /models.").await;
                        }
                        return;
                    }
                }
            }
            "ollama" => Ok((
                Arc::new(OpenAiCompatible::new("ollama", &format!("{}/v1", p.ollama_url.trim_end_matches('/')), None, "")),
                model.or(p.ollama_model),
            )),
            "openai" => match key_for("openai", &p.openai_key_env) {
                Some(k) => Ok((
                    Arc::new(OpenAiCompatible::new("openai", &p.openai_url, Some(k), &p.openai_key_env)),
                    model.or(Some(p.openai_model)),
                )),
                None => Err(format!("No OpenAI key: run /key openai, or set {}.", p.openai_key_env)),
            },
            "anthropic" => match key_for("anthropic", &p.anthropic_key_env) {
                Some(k) => Ok((Arc::new(Anthropic::new(&p.anthropic_url, k, &p.anthropic_key_env)), model.or(Some(p.anthropic_model)))),
                None => Err(format!("No Anthropic key: run /key anthropic, or set {}.", p.anthropic_key_env)),
            },
            "compat" => match &p.compat_url {
                Some(url) => {
                    let key = key_for("compat", p.compat_key_env.as_deref().unwrap_or("TWO_ONE_TWO_COMPAT_KEY"));
                    Ok((
                        Arc::new(OpenAiCompatible::new("compat", url, key, p.compat_key_env.as_deref().unwrap_or(""))),
                        model.or(p.compat_model),
                    ))
                }
                None => Err("Set providers.compat_url in config.toml to use an OpenAI-compatible endpoint.".into()),
            },
            other => Err(format!("Unknown provider '{other}'. Choose local, ollama, openai, anthropic or compat.")),
        };
        let (provider, mut model) = match built {
            Ok(x) => x,
            Err(e) => return self.notice(NoticeLevel::Error, e).await,
        };
        // Verify connectivity and resolve a model when none is configured.
        if name != "local" {
            match tokio::time::timeout(Duration::from_secs(5), provider.list_models()).await {
                Ok(Ok(list)) => {
                    if model.is_none() {
                        model = list.first().map(|m| m.id.clone());
                    }
                }
                Ok(Err(e)) => return self.notice(NoticeLevel::Error, format!("{e}")).await,
                Err(_) => return self.notice(NoticeLevel::Error, format!("{} did not respond within 5s.", provider.describe())).await,
            }
            if model.is_none() {
                return self.notice(NoticeLevel::Error, format!("{name} has no models available.")).await;
            }
        }
        self.provider_name = name.clone();
        self.model_detail = if name == "local" { self.model_detail.take() } else { Some(provider.describe()) };
        self.provider = Some(provider);
        self.model_name = model.clone();
        self.save_setting("inference", "provider", name.clone().into());
        self.emit(Event::Inference(InferenceState::Ready)).await;
        self.sync_session_settings();
        self.snapshot().await;
        self.notice(NoticeLevel::Success, format!("Provider: {name} · model {}", model.unwrap_or_default())).await;
    }
}
