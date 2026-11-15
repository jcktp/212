//! /doctor, /status and /context reports.

use std::time::Duration;

use common::format;
use common::proto::{Event, ReportLine, ReportStatus};

use crate::Runtime;

fn line(status: ReportStatus, label: &str, detail: impl Into<String>) -> ReportLine {
    ReportLine { status, label: label.into(), detail: detail.into() }
}

impl Runtime {
    pub(crate) async fn doctor(&mut self, terminal: Vec<ReportLine>) {
        use ReportStatus::*;
        let h = &self.hardware;
        let mut lines = vec![line(
            if matches!(h.os.as_str(), "macos" | "linux" | "windows") { Ok } else { Warn },
            "System",
            format!("{} {} · {} cores", h.os_version.clone().unwrap_or_else(|| h.os.clone()), h.arch, h.cpu_cores),
        )];
        lines.push(line(Info, "Memory", format!("{} total · {} available", format::bytes(h.total_mem), format::bytes(h.available_mem))));
        lines.push(line(
            if h.accel == inference::Accel::Cpu { Warn } else { Ok },
            "Acceleration",
            match &h.accel {
                inference::Accel::Metal => "Metal (Apple Silicon, unified memory)".to_string(),
                inference::Accel::Cuda { name, vram } => {
                    format!("CUDA · {name}{}", vram.map(|v| format!(" · {}", format::bytes(v))).unwrap_or_default())
                }
                inference::Accel::Cpu => "none detected — CPU inference works but is slower".into(),
            },
        ));
        lines.extend(terminal);
        match &self.backend {
            Some(b) => {
                let version = inference::backend::probe_version(&b.path).await;
                lines.push(line(
                    if version.is_some() { Ok } else { Fail },
                    "Backend",
                    format!("llama-server {} ({}) · {}", version.as_deref().unwrap_or("did not run"), b.source.label(), b.path.display()),
                ));
            }
            None => lines.push(line(
                Fail,
                "Backend",
                "llama-server not found — /models switch offers to install it, or set inference.server_path",
            )),
        }
        match &mut self.server {
            Some(s) => {
                let running = s.is_running();
                lines.push(line(
                    if running { Ok } else { Fail },
                    "Inference",
                    if running {
                        format!("serving {} on {} (loopback only)", s.alias, s.base_url)
                    } else {
                        "backend process exited".into()
                    },
                ));
            }
            None => lines.push(line(Info, "Inference", "no local model loaded")),
        }
        let dir = self.store.dir().to_path_buf();
        let probe = dir.join(".write-test");
        let writable = std::fs::write(&probe, b"ok").is_ok();
        let _ = std::fs::remove_file(&probe);
        let free = inference::hardware::free_space(&dir).map(format::bytes).unwrap_or_else(|| "unknown".into());
        lines.push(line(
            if writable { Ok } else { Fail },
            "Model storage",
            format!("{} · {free} free{}", dir.display(), if writable { "" } else { " · NOT WRITABLE" }),
        ));
        let checks = self.store.check();
        if checks.is_empty() {
            lines.push(line(Info, "Models", "none installed"));
        }
        for (id, r) in checks {
            match r {
                Result::Ok(()) => lines.push(line(Ok, "Model", id)),
                Err(e) => lines.push(line(Fail, "Model", format!("{id}: {e}"))),
            }
        }
        match config::Config::load(&self.config_path) {
            Result::Ok(l) if l.warnings.is_empty() => lines.push(line(Ok, "Config", self.config_path.display().to_string())),
            Result::Ok(l) => {
                for w in l.warnings {
                    lines.push(line(Warn, "Config", w));
                }
            }
            Err(e) => lines.push(line(Fail, "Config", e.to_string())),
        }
        if let Some(p) = &self.provider
            && self.provider_name != "local"
        {
            let r = tokio::time::timeout(Duration::from_secs(5), p.list_models()).await;
            lines.push(match r {
                Result::Ok(Result::Ok(m)) => line(Ok, "Provider", format!("{} reachable · {} models", p.describe(), m.len())),
                Result::Ok(Err(e)) => line(Fail, "Provider", e.to_string()),
                Err(_) => line(Fail, "Provider", format!("{} timed out", p.describe())),
            });
        }
        if let Some(d) = models::ollama::local_models_dir().filter(|d| d.exists()) {
            let n = models::ollama::scan_local(&d).len();
            lines.push(line(Info, "Ollama store", format!("{n} model(s) importable from {} (Ollama itself is not required)", d.display())));
        }
        lines.push(line(Info, "Logs", self.paths.log_dir().display().to_string()));
        self.emit(Event::Report { title: "Diagnostics".into(), art: Some("doctor"), lines }).await;
    }

    pub(crate) async fn status(&mut self) {
        use ReportStatus::*;
        let mut lines = vec![
            line(Info, "Version", common::VERSION),
            line(Info, "Provider", self.provider_name.clone()),
            line(Info, "Model", self.model_name.clone().unwrap_or_else(|| "none".into())),
            line(Info, "Mode", self.mode.as_str()),
            line(Info, "Context", format!("{} / {} tokens", self.context_used, self.context_size())),
            line(Info, "Messages", self.messages.len().to_string()),
        ];
        if let Some(s) = &self.server {
            lines.push(line(Info, "Backend", format!("pid {} · {} · ctx {}", s.pid().unwrap_or(0), s.base_url, s.ctx_size)));
            lines.push(line(Info, "Backend log", s.log_file.display().to_string()));
        }
        lines.push(line(Info, "Config", self.config_path.display().to_string()));
        lines.push(line(Info, "Data", self.paths.data_dir.display().to_string()));
        self.emit(Event::Report { title: "Status".into(), art: None, lines }).await;
    }

    pub(crate) async fn context_report(&mut self) {
        use ReportStatus::*;
        let size = self.context_size();
        let est: u32 = self.messages.iter().map(crate::context::message_tokens).sum();
        let used = if self.context_used > 0 { self.context_used } else { est };
        let pct = (used as f64 / size.max(1) as f64 * 100.0).round();
        let status = if pct > 90.0 {
            Fail
        } else if pct > 70.0 {
            Warn
        } else {
            Ok
        };
        let lines = vec![
            line(
                status,
                "Used",
                format!(
                    "{used} of {size} tokens ({pct}%){}",
                    if self.context_used == 0 { " · estimated" } else { " · reported by backend" }
                ),
            ),
            line(Info, "Messages", format!("{} in history", self.messages.len())),
            line(Info, "Policy", "oldest turns are left out when the window fills; you are told when that happens"),
            line(Info, "Tip", "/new starts a fresh conversation"),
        ];
        self.emit(Event::Report { title: "Context".into(), art: None, lines }).await;
    }
}
