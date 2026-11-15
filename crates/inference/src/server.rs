//! The managed `llama-server` child process.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub binary: PathBuf,
    pub model: PathBuf,
    pub alias: String,
    pub ctx_size: u32,
    /// `None` lets llama.cpp decide (it offloads as much as fits).
    pub gpu_layers: Option<u32>,
    pub threads: Option<u32>,
    pub log_file: PathBuf,
    pub run_dir: PathBuf,
    pub extra_args: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("could not start {binary}: {source}")]
    Spawn { binary: String, source: std::io::Error },
    #[error("inference backend exited during startup ({status}). {hint}")]
    Exited { status: String, hint: String },
    #[error("model did not finish loading within {0}s")]
    Timeout(u64),
    #[error("model loading cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub struct LlamaServer {
    child: tokio::process::Child,
    pub base_url: String,
    pub api_key: String,
    pub alias: String,
    pub model: PathBuf,
    pub ctx_size: u32,
    pub log_file: PathBuf,
    pid_file: PathBuf,
    client: reqwest::Client,
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn random_key() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..32).map(|_| format!("{:x}", rng.random_range(0..16u8))).collect()
}

impl LlamaServer {
    /// Spawn the server. Returns immediately; call [`wait_ready`] next.
    pub fn spawn(opts: &ServerOptions) -> Result<Self, ServerError> {
        std::fs::create_dir_all(&opts.run_dir)?;
        reap_stale(&opts.run_dir);
        if let Some(parent) = opts.log_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let log = std::fs::File::create(&opts.log_file)?;
        let port = free_port()?;
        let api_key = random_key();

        let mut cmd = tokio::process::Command::new(&opts.binary);
        cmd.arg("--model")
            .arg(&opts.model)
            // Loopback only. Exposing the backend requires explicit user config.
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--api-key", &api_key])
            .args(["--ctx-size", &opts.ctx_size.to_string()])
            .args(["--alias", &opts.alias])
            // One slot: the whole context belongs to the conversation.
            .args(["--parallel", "1"])
            .arg("--no-webui")
            .arg("--jinja");
        if let Some(n) = opts.gpu_layers {
            cmd.args(["--n-gpu-layers", &n.to_string()]);
        }
        if let Some(t) = opts.threads {
            cmd.args(["--threads", &t.to_string()]);
        }
        cmd.args(&opts.extra_args).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).kill_on_drop(true);
        #[cfg(target_os = "linux")]
        unsafe {
            // Ask the kernel to terminate the server if 212 dies abruptly.
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|source| ServerError::Spawn { binary: opts.binary.display().to_string(), source })?;
        let pid_file = opts.run_dir.join(format!("llama-server-{}.pid", std::process::id()));
        if let Some(pid) = child.id() {
            let _ = std::fs::write(&pid_file, format!("{pid}\n"));
        }
        tracing::info!(port, model = %opts.model.display(), "spawned llama-server");
        Ok(Self {
            child,
            base_url: format!("http://127.0.0.1:{port}"),
            api_key,
            alias: opts.alias.clone(),
            model: opts.model.clone(),
            ctx_size: opts.ctx_size,
            log_file: opts.log_file.clone(),
            pid_file,
            client: {
                models::init_tls();
                reqwest::Client::builder()
            }
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("client"),
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// OpenAI-compatible API root.
    pub fn api_base(&self) -> String {
        format!("{}/v1", self.base_url)
    }

    /// Poll `/health` until the model is loaded, the process dies, the timeout
    /// expires or `cancel` fires.
    pub async fn wait_ready(&mut self, timeout: Duration, cancel: &CancellationToken) -> Result<(), ServerError> {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Err(ServerError::Exited { status: status.to_string(), hint: diagnose_log(&self.log_file) });
            }
            let health = self.client.get(format!("{}/health", self.base_url)).bearer_auth(&self.api_key).send().await;
            if let Ok(r) = health
                && r.status().is_success()
            {
                tracing::info!(elapsed_ms = start.elapsed().as_millis() as u64, "llama-server ready");
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(ServerError::Timeout(timeout.as_secs()));
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = cancel.cancelled() => return Err(ServerError::Cancelled),
            }
        }
    }

    /// True while the child process is running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Graceful stop: SIGTERM, then SIGKILL after a grace period.
    pub async fn shutdown(mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            if tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await.is_ok() {
                let _ = std::fs::remove_file(&self.pid_file);
                return;
            }
        }
        let _ = self.child.kill().await;
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

impl Drop for LlamaServer {
    fn drop(&mut self) {
        // Last line of defence; `shutdown` is the normal path.
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

/// Kill servers left behind by 212 processes that no longer exist (e.g. after
/// SIGKILL). Pid files are named after the owning 212 process.
pub fn reap_stale(run_dir: &Path) -> usize {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let Ok(entries) = std::fs::read_dir(run_dir) else { return 0 };
    let Ok(mut sys) = System::new() else { return 0 };
    let mut reaped = 0;
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(owner) = name.strip_prefix("llama-server-").and_then(|s| s.strip_suffix(".pid")).and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(child) = std::fs::read_to_string(e.path()).ok().and_then(|s| s.trim().parse::<u32>().ok()) else {
            let _ = std::fs::remove_file(e.path());
            continue;
        };
        let (owner_pid, child_pid) = (Pid::from_u32(owner), Pid::from_u32(child));
        sys.refresh_processes(ProcessesToUpdate::Some(&[owner_pid, child_pid]), true);
        let owner_alive = owner != std::process::id() && sys.process(owner_pid).is_some();
        if owner_alive {
            continue;
        }
        if let Some(p) = sys.process(child_pid)
            && p.name().to_string_lossy().contains("llama-server")
        {
            tracing::warn!(pid = child, "killing orphaned llama-server");
            p.kill();
            reaped += 1;
        }
        let _ = std::fs::remove_file(e.path());
    }
    reaped
}

/// Turn the tail of the server log into an actionable hint.
pub fn diagnose_log(log: &Path) -> String {
    let text =
        std::fs::read(log).map(|b| String::from_utf8_lossy(&b[b.len().saturating_sub(16 * 1024)..]).into_owned()).unwrap_or_default();
    let lower = text.to_lowercase();
    let hint = if lower.contains("out of memory") || lower.contains("failed to allocate") || lower.contains("insufficient memory") {
        "The model does not fit in memory. Try a smaller quantization, a smaller model, or a lower context_size."
    } else if lower.contains("unknown model architecture") {
        "This llama.cpp build does not support the model's architecture. Update the backend (/doctor)."
    } else if lower.contains("failed to load model") || lower.contains("error loading model") {
        "llama.cpp could not load the model file. It may be corrupt or use an unsupported format."
    } else if lower.contains("address already in use") {
        "The chosen port was taken; try again."
    } else {
        "See the backend log for details."
    };
    let last: Vec<&str> = text
        .lines()
        .filter(|l| {
            let l = l.to_lowercase();
            l.contains("error") || l.contains("failed")
        })
        .rev()
        .take(2)
        .collect();
    if last.is_empty() {
        format!("{hint} (log: {})", log.display())
    } else {
        format!("{hint}\n  {}\n(log: {})", last.into_iter().rev().collect::<Vec<_>>().join("\n  "), log.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnoses_oom() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("l.log");
        std::fs::write(&log, "loading...\nggml_metal: error: failed to allocate buffer\n").unwrap();
        let d = diagnose_log(&log);
        assert!(d.contains("does not fit"), "{d}");
        assert!(d.contains("failed to allocate"));
    }

    #[test]
    fn reaps_only_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        // Owner is this live process: must be left alone.
        let mine = tmp.path().join(format!("llama-server-{}.pid", std::process::id()));
        std::fs::write(&mine, "999999\n").unwrap();
        // Owner pid that cannot exist; child pid that isn't llama-server.
        let dead = tmp.path().join("llama-server-4000000.pid");
        std::fs::write(&dead, format!("{}\n", std::process::id())).unwrap();
        assert_eq!(reap_stale(tmp.path()), 0, "must never kill a non-llama-server process");
        assert!(!dead.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_early_exit_with_log_hint() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("llama-server");
        std::fs::write(&fake, "#!/bin/sh\necho 'llama_model_load: error loading model: bad magic' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opts = ServerOptions {
            binary: fake,
            model: tmp.path().join("m.gguf"),
            alias: "m".into(),
            ctx_size: 512,
            gpu_layers: None,
            threads: None,
            log_file: tmp.path().join("server.log"),
            run_dir: tmp.path().join("run"),
            extra_args: vec![],
        };
        let mut s = LlamaServer::spawn(&opts).unwrap();
        let err = s.wait_ready(Duration::from_secs(10), &CancellationToken::new()).await.unwrap_err();
        match err {
            ServerError::Exited { hint, .. } => assert!(hint.contains("could not load"), "{hint}"),
            e => panic!("{e:?}"),
        }
        s.shutdown().await;
        assert!(std::fs::read_dir(tmp.path().join("run")).unwrap().next().is_none(), "pid file removed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_and_shutdown_kill_the_process() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("llama-server");
        std::fs::write(&fake, "#!/bin/sh\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opts = ServerOptions {
            binary: fake,
            model: tmp.path().join("m.gguf"),
            alias: "m".into(),
            ctx_size: 512,
            gpu_layers: Some(0),
            threads: Some(1),
            log_file: tmp.path().join("server.log"),
            run_dir: tmp.path().join("run"),
            extra_args: vec![],
        };
        let mut s = LlamaServer::spawn(&opts).unwrap();
        let pid = s.pid().unwrap();
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            c2.cancel();
        });
        assert!(matches!(s.wait_ready(Duration::from_secs(30), &cancel).await, Err(ServerError::Cancelled)));
        assert!(s.is_running());
        s.shutdown().await;
        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
        assert!(!alive, "server process must be gone after shutdown");
    }
}
