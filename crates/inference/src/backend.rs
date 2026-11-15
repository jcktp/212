//! Locating a `llama-server` executable.
//!
//! Order: explicit config path → 212-managed install → `PATH`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendSource {
    Configured,
    Managed,
    SystemPath,
}

impl BackendSource {
    pub fn label(self) -> &'static str {
        match self {
            BackendSource::Configured => "configured",
            BackendSource::Managed => "managed",
            BackendSource::SystemPath => "system",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub path: PathBuf,
    pub source: BackendSource,
    pub version: Option<String>,
}

pub const SERVER_BIN: &str = if cfg!(windows) { "llama-server.exe" } else { "llama-server" };

pub fn managed_root(backends_dir: &Path) -> PathBuf {
    backends_dir.join("llama.cpp")
}

/// The managed install records the active binary in `current`.
pub fn managed_binary(backends_dir: &Path) -> Option<PathBuf> {
    let root = managed_root(backends_dir);
    let rel = std::fs::read_to_string(root.join("current")).ok()?;
    let p = root.join(rel.trim());
    is_executable(&p).then_some(p)
}

pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.is_file() && md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        md.is_file()
    }
}

pub fn discover(configured: Option<&Path>, backends_dir: &Path) -> Option<Backend> {
    if let Some(p) = configured {
        if is_executable(p) {
            return Some(Backend { path: p.into(), source: BackendSource::Configured, version: None });
        }
        tracing::warn!("configured server_path {} is not executable", p.display());
    }
    if let Some(p) = managed_binary(backends_dir) {
        return Some(Backend { path: p, source: BackendSource::Managed, version: None });
    }
    find_in_path(SERVER_BIN).map(|p| Backend { path: p, source: BackendSource::SystemPath, version: None })
}

/// Run `llama-server --version` and extract e.g. `b12359` / `1.0.0 (build 12359)`.
pub async fn probe_version(path: &Path) -> Option<String> {
    let fut = tokio::process::Command::new(path).arg("--version").stdin(std::process::Stdio::null()).kill_on_drop(true).output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), fut).await.ok()?.ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    parse_version(&text)
}

pub fn parse_version(text: &str) -> Option<String> {
    let line = text.lines().find(|l| l.trim_start().starts_with("version:"))?;
    let v = line.trim_start().trim_start_matches("version:").trim();
    // "1.0.0 (build 12359, commit b1b770d90)" -> "1.0.0 b12359"
    match (v.split_whitespace().next(), v.split("build ").nth(1)) {
        (Some(ver), Some(rest)) => {
            let build: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if ver.chars().all(|c| c.is_ascii_digit()) { Some(format!("b{build}")) } else { Some(format!("{ver} b{build}")) }
        }
        (Some(ver), None) => Some(ver.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("version: 1.0.0 (build 12359, commit b1b770d90)\nbuilt with clang").as_deref(), Some("1.0.0 b12359"));
        assert_eq!(parse_version("version: 6543 (abc)\n").as_deref(), Some("6543"));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn configured_path_wins_and_missing_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("srv");
        std::fs::write(&fake, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let b = discover(Some(&fake), tmp.path()).unwrap();
        assert_eq!(b.source, BackendSource::Configured);
        // A managed install is preferred over PATH.
        let root = managed_root(tmp.path());
        std::fs::create_dir_all(root.join("b1/bin")).unwrap();
        std::fs::copy(&fake, root.join("b1/bin").join(SERVER_BIN)).unwrap();
        std::fs::write(root.join("current"), format!("b1/bin/{SERVER_BIN}")).unwrap();
        let b = discover(Some(&tmp.path().join("missing")), tmp.path()).unwrap();
        assert_eq!(b.source, BackendSource::Managed);
    }
}
