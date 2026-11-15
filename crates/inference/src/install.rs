//! Installing a verified llama.cpp build from the official GitHub releases.
//!
//! Every asset must carry a SHA-256 digest published by GitHub; downloads are
//! verified before extraction and nothing is executed before verification.

use std::path::{Path, PathBuf};

use models::download::{DownloadSpec, Progress, download};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::backend::{SERVER_BIN, managed_root};

pub const RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=10";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not query llama.cpp releases: {0}")]
    Network(String),
    #[error("no prebuilt llama.cpp release matches this platform ({0})")]
    NoAsset(String),
    #[error("release asset {0} has no published checksum; refusing to install")]
    NoChecksum(String),
    #[error(transparent)]
    Download(#[from] models::download::DownloadError),
    #[error("archive problem: {0}")]
    Archive(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
    #[serde(default)]
    pub digest: Option<String>,
}

impl Asset {
    pub fn sha256(&self) -> Option<&str> {
        self.digest.as_deref()?.strip_prefix("sha256:")
    }
}

/// Asset name fragments for this platform, best first. GPU variants other than
/// Metal need matching drivers, so the portable CPU build is the default
/// elsewhere; users with CUDA can point `inference.server_path` at a CUDA build.
pub fn platform_patterns(os: &str, arch: &str) -> Vec<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => vec!["-bin-macos-arm64."],
        ("macos", "x86_64") => vec!["-bin-macos-x64."],
        ("linux", "x86_64") => vec!["-bin-ubuntu-x64."],
        ("linux", "aarch64") => vec!["-bin-ubuntu-arm64."],
        ("windows", "x86_64") => vec!["-bin-win-cpu-x64."],
        ("windows", "aarch64") => vec!["-bin-win-cpu-arm64."],
        _ => vec![],
    }
}

pub fn pick_asset<'a>(releases: &'a [Release], os: &str, arch: &str) -> Option<(&'a Release, &'a Asset)> {
    let pats = platform_patterns(os, arch);
    releases.iter().find_map(|r| {
        pats.iter().find_map(|p| {
            r.assets
                .iter()
                .find(|a| a.name.starts_with("llama-") && a.name.contains(p) && (a.name.ends_with(".tar.gz") || a.name.ends_with(".zip")))
                .map(|a| (r, a))
        })
    })
}

pub async fn find_release(client: &reqwest::Client) -> Result<(Release, Asset), InstallError> {
    let releases: Vec<Release> = client
        .get(RELEASES_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| InstallError::Network(e.to_string()))?
        .json()
        .await
        .map_err(|e| InstallError::Network(e.to_string()))?;
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    pick_asset(&releases, os, arch).map(|(r, a)| (r.clone(), a.clone())).ok_or_else(|| InstallError::NoAsset(format!("{os}/{arch}")))
}

/// Download, verify, extract and activate a release. Returns the server path.
pub async fn install(
    client: &reqwest::Client,
    release: &Release,
    asset: &Asset,
    backends_dir: &Path,
    cancel: &CancellationToken,
    on_progress: impl FnMut(Progress),
) -> Result<PathBuf, InstallError> {
    let sha = asset.sha256().ok_or_else(|| InstallError::NoChecksum(asset.name.clone()))?;
    let root = managed_root(backends_dir);
    let archive = root.join("downloads").join(&asset.name);
    let spec = DownloadSpec {
        url: asset.browser_download_url.clone(),
        dest: archive.clone(),
        sha256: Some(sha.to_lowercase()),
        size: Some(asset.size),
        bearer: None,
    };
    download(client, &spec, cancel, on_progress).await?;

    let tag = sanitize(&release.tag_name);
    let staging = root.join(format!("{tag}.partial"));
    let final_dir = root.join(&tag);
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let (a, s) = (archive.clone(), staging.clone());
    tokio::task::spawn_blocking(move || extract(&a, &s)).await.map_err(|e| InstallError::Archive(e.to_string()))??;
    let _ = std::fs::remove_dir_all(&final_dir);
    std::fs::rename(&staging, &final_dir)?;
    let server = find_file(&final_dir, SERVER_BIN).ok_or_else(|| InstallError::Archive(format!("{SERVER_BIN} not found in archive")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))?;
    }
    let rel = server.strip_prefix(&root).map_err(|e| InstallError::Archive(e.to_string()))?;
    std::fs::write(root.join("current"), rel.to_string_lossy().as_bytes())?;
    let _ = std::fs::remove_file(&archive);
    Ok(server)
}

fn sanitize(tag: &str) -> String {
    tag.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

/// Extract `.tar.gz` or `.zip`, refusing entries that escape `dest`.
pub fn extract(archive: &Path, dest: &Path) -> Result<(), InstallError> {
    let name = archive.to_string_lossy();
    let f = std::fs::File::open(archive)?;
    if name.ends_with(".tar.gz") {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
        for entry in ar.entries()? {
            let mut entry = entry?;
            // `unpack_in` rejects absolute paths and `..` components.
            if !entry.unpack_in(dest)? {
                return Err(InstallError::Archive(format!("unsafe path in archive: {}", entry.path()?.display())));
            }
        }
        Ok(())
    } else if name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(f).map_err(|e| InstallError::Archive(e.to_string()))?;
        for i in 0..zip.len() {
            let mut file = zip.by_index(i).map_err(|e| InstallError::Archive(e.to_string()))?;
            let Some(rel) = file.enclosed_name() else {
                return Err(InstallError::Archive(format!("unsafe path in archive: {}", file.name())));
            };
            let out = dest.join(rel);
            if file.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut w = std::fs::File::create(&out)?;
                std::io::copy(&mut file, &mut w)?;
            }
        }
        Ok(())
    } else {
        Err(InstallError::Archive(format!("unsupported archive {name}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str, digest: Option<&str>) -> Asset {
        Asset { name: name.into(), browser_download_url: format!("https://x/{name}"), size: 1, digest: digest.map(str::to_string) }
    }

    #[test]
    fn picks_platform_asset_across_releases() {
        let releases = vec![
            Release { tag_name: "v1".into(), assets: vec![asset("nightly-tag.txt", None)] },
            Release {
                tag_name: "b2".into(),
                assets: vec![
                    asset("cudart-llama-b2-bin-ubuntu-cuda-12.8-x64.tar.gz", None),
                    asset("llama-b2-bin-macos-arm64.tar.gz", Some("sha256:ab")),
                    asset("llama-b2-bin-ubuntu-x64.tar.gz", Some("sha256:cd")),
                    asset("llama-b2-bin-win-cpu-x64.zip", Some("sha256:ef")),
                ],
            },
        ];
        let (r, a) = pick_asset(&releases, "macos", "aarch64").unwrap();
        assert_eq!((r.tag_name.as_str(), a.name.as_str(), a.sha256()), ("b2", "llama-b2-bin-macos-arm64.tar.gz", Some("ab")));
        assert_eq!(pick_asset(&releases, "linux", "x86_64").unwrap().1.name, "llama-b2-bin-ubuntu-x64.tar.gz");
        assert_eq!(pick_asset(&releases, "windows", "x86_64").unwrap().1.name, "llama-b2-bin-win-cpu-x64.zip");
        assert!(pick_asset(&releases, "freebsd", "riscv64").is_none());
    }

    #[test]
    fn extracts_tar_gz_and_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = tmp.path().join("a.tar.gz");
        {
            let f = std::fs::File::create(&archive).unwrap();
            let mut b = tar::Builder::new(flate2::write::GzEncoder::new(f, flate2::Compression::fast()));
            let data = b"#!/bin/sh\n";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "build/bin/llama-server", &data[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let out = tmp.path().join("out");
        std::fs::create_dir_all(&out).unwrap();
        extract(&archive, &out).unwrap();
        assert!(out.join("build/bin/llama-server").exists());
        assert!(find_file(&out, "llama-server").is_some());

        // A zip entry with `..` must be refused.
        let zpath = tmp.path().join("evil.zip");
        {
            let f = std::fs::File::create(&zpath).unwrap();
            let mut z = zip::ZipWriter::new(f);
            z.start_file("../evil", zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut z, b"x").unwrap();
            z.finish().unwrap();
        }
        assert!(extract(&zpath, &out).is_err());
        assert!(!tmp.path().join("evil").exists());
    }
}
