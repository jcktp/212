//! Ollama model support without the Ollama daemon.
//!
//! * Registry: we speak the OCI-style registry protocol directly
//!   (`/v2/<ns>/<name>/manifests/<tag>`, `/v2/<ns>/<name>/blobs/<digest>`).
//!   Blobs are content-addressed, so every download is verified by its digest.
//! * Local store: models already pulled by an Ollama install are discovered in
//!   `~/.ollama/models` (or `$OLLAMA_MODELS`) and can be imported in place.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const REGISTRY: &str = "https://registry.ollama.ai";
pub const MEDIA_MODEL: &str = "application/vnd.ollama.image.model";
pub const MEDIA_TEMPLATE: &str = "application/vnd.ollama.image.template";
pub const MEDIA_PARAMS: &str = "application/vnd.ollama.image.params";
pub const MEDIA_LICENSE: &str = "application/vnd.ollama.image.license";
pub const MEDIA_PROJECTOR: &str = "application/vnd.ollama.image.projector";
pub const MEDIA_ADAPTER: &str = "application/vnd.ollama.image.adapter";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaRef {
    pub namespace: String,
    pub name: String,
    pub tag: String,
}

impl OllamaRef {
    /// Parse `ollama:qwen3`, `qwen3:0.6b`, `user/model:tag`.
    pub fn parse(spec: &str) -> Option<Self> {
        let s = spec.trim().trim_start_matches("ollama:").trim_start_matches("ollama.com/");
        if s.is_empty() || s.contains(char::is_whitespace) {
            return None;
        }
        let (path, tag) = match s.rsplit_once(':') {
            Some((p, t)) if !t.contains('/') => (p, t),
            _ => (s, "latest"),
        };
        let (namespace, name) = match path.split_once('/') {
            Some((ns, n)) if !n.contains('/') => (ns, n),
            None => ("library", path),
            _ => return None,
        };
        let ok = |x: &str| !x.is_empty() && !x.starts_with('.') && x.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
        (ok(namespace) && ok(name) && ok(tag)).then(|| Self { namespace: namespace.into(), name: name.into(), tag: tag.into() })
    }

    pub fn display(&self) -> String {
        if self.namespace == "library" {
            format!("{}:{}", self.name, self.tag)
        } else {
            format!("{}/{}:{}", self.namespace, self.name, self.tag)
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Layer {
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub digest: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub layers: Vec<Layer>,
}

impl Manifest {
    pub fn layer(&self, media: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.media_type == media)
    }
    /// Explain why this model can't be used, if it can't.
    pub fn compatibility(&self) -> Result<&Layer, String> {
        if self.layers.iter().any(|l| l.media_type == MEDIA_ADAPTER) {
            return Err("this model uses a LoRA adapter layer, which 212 does not support yet".into());
        }
        self.layer(MEDIA_MODEL).ok_or_else(|| "the manifest has no GGUF model layer (it may use a format llama.cpp can't load)".into())
    }
}

pub fn digest_hex(digest: &str) -> Option<&str> {
    let h = digest.strip_prefix("sha256:")?;
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then_some(h)
}

#[derive(Debug, thiserror::Error)]
pub enum OllamaError {
    #[error("registry request failed: {0}")]
    Network(String),
    #[error("model '{0}' was not found in the Ollama registry")]
    NotFound(String),
    #[error("registry returned HTTP {0}")]
    Http(u16),
}

pub struct RegistryClient {
    client: reqwest::Client,
    base: String,
}

impl RegistryClient {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client, base: REGISTRY.into() }
    }
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }

    pub async fn manifest(&self, r: &OllamaRef) -> Result<Manifest, OllamaError> {
        let url = format!("{}/v2/{}/{}/manifests/{}", self.base, r.namespace, r.name, r.tag);
        let resp = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/vnd.docker.distribution.manifest.v2+json")
            .send()
            .await
            .map_err(|e| OllamaError::Network(e.to_string()))?;
        match resp.status().as_u16() {
            200 => resp.json().await.map_err(|e| OllamaError::Network(e.to_string())),
            404 => Err(OllamaError::NotFound(r.display())),
            s => Err(OllamaError::Http(s)),
        }
    }

    pub fn blob_url(&self, r: &OllamaRef, digest: &str) -> String {
        format!("{}/v2/{}/{}/blobs/{digest}", self.base, r.namespace, r.name)
    }

    pub async fn small_blob(&self, r: &OllamaRef, layer: &Layer) -> Result<String, OllamaError> {
        if layer.size > 1 << 20 {
            return Err(OllamaError::Network("blob too large".into()));
        }
        let resp = self.client.get(self.blob_url(r, &layer.digest)).send().await.map_err(|e| OllamaError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(OllamaError::Http(resp.status().as_u16()));
        }
        resp.text().await.map_err(|e| OllamaError::Network(e.to_string()))
    }
}

/// A model found in a local Ollama installation's store.
#[derive(Debug, Clone)]
pub struct LocalOllamaModel {
    pub reference: OllamaRef,
    pub model_blob: PathBuf,
    pub size: u64,
    pub template: Option<String>,
    pub params: Option<serde_json::Value>,
    pub projector: Option<PathBuf>,
    pub problem: Option<String>,
}

pub fn local_models_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("OLLAMA_MODELS") {
        return Some(PathBuf::from(p));
    }
    directories::BaseDirs::new().map(|b| b.home_dir().join(".ollama").join("models"))
}

fn blob_path(root: &Path, digest: &str) -> PathBuf {
    root.join("blobs").join(digest.replace(':', "-"))
}

/// Scan a local Ollama store. Never fails hard: unreadable entries are skipped.
pub fn scan_local(root: &Path) -> Vec<LocalOllamaModel> {
    let manifests = root.join("manifests");
    let mut out = Vec::new();
    let Ok(hosts) = std::fs::read_dir(&manifests) else { return out };
    for host in hosts.flatten() {
        let Ok(namespaces) = std::fs::read_dir(host.path()) else { continue };
        for ns in namespaces.flatten() {
            let Ok(names) = std::fs::read_dir(ns.path()) else { continue };
            for name in names.flatten() {
                let Ok(tags) = std::fs::read_dir(name.path()) else { continue };
                for tag in tags.flatten() {
                    let Some(m) = std::fs::read_to_string(tag.path()).ok().and_then(|t| serde_json::from_str::<Manifest>(&t).ok()) else {
                        continue;
                    };
                    let reference = OllamaRef {
                        namespace: ns.file_name().to_string_lossy().into(),
                        name: name.file_name().to_string_lossy().into(),
                        tag: tag.file_name().to_string_lossy().into(),
                    };
                    let read_small = |media: &str| m.layer(media).and_then(|l| std::fs::read_to_string(blob_path(root, &l.digest)).ok());
                    let (model_blob, size, problem) = match m.compatibility() {
                        Ok(l) => {
                            let p = blob_path(root, &l.digest);
                            let problem = (!p.exists()).then(|| "model blob missing".to_string());
                            (p, l.size, problem)
                        }
                        Err(e) => (PathBuf::new(), 0, Some(e)),
                    };
                    out.push(LocalOllamaModel {
                        reference,
                        model_blob,
                        size,
                        template: read_small(MEDIA_TEMPLATE),
                        params: read_small(MEDIA_PARAMS).and_then(|t| serde_json::from_str(&t).ok()),
                        projector: m.layer(MEDIA_PROJECTOR).map(|l| blob_path(root, &l.digest)),
                        problem,
                    });
                }
            }
        }
    }
    out.sort_by_key(|a| a.reference.display());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_refs() {
        let r = OllamaRef::parse("ollama:qwen3").unwrap();
        assert_eq!((r.namespace.as_str(), r.name.as_str(), r.tag.as_str()), ("library", "qwen3", "latest"));
        let r = OllamaRef::parse("qwen3:0.6b").unwrap();
        assert_eq!(r.tag, "0.6b");
        let r = OllamaRef::parse("someone/coder:7b-q4").unwrap();
        assert_eq!((r.namespace.as_str(), r.display().as_str()), ("someone", "someone/coder:7b-q4"));
        assert!(OllamaRef::parse("a/b/c").is_none());
        assert!(OllamaRef::parse("bad name").is_none());
        assert!(OllamaRef::parse("../etc").is_none());
    }

    #[test]
    fn manifest_compatibility() {
        let m: Manifest = serde_json::from_str(
            r#"{"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:ab","size":5},
                          {"mediaType":"application/vnd.ollama.image.template","digest":"sha256:cd","size":1}]}"#,
        )
        .unwrap();
        assert_eq!(m.compatibility().unwrap().size, 5);
        let m: Manifest =
            serde_json::from_str(r#"{"layers":[{"mediaType":"application/vnd.ollama.image.adapter","digest":"sha256:ab","size":5}]}"#)
                .unwrap();
        assert!(m.compatibility().is_err());
        assert_eq!(digest_hex(&format!("sha256:{}", "a".repeat(64))), Some("a".repeat(64).as_str()));
        assert_eq!(digest_hex("sha256:zz"), None);
    }

    #[test]
    fn scans_local_store() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mdir = root.join("manifests/registry.ollama.ai/library/tiny");
        std::fs::create_dir_all(&mdir).unwrap();
        std::fs::create_dir_all(root.join("blobs")).unwrap();
        std::fs::write(root.join("blobs/sha256-aa"), b"GGUF").unwrap();
        std::fs::write(root.join("blobs/sha256-bb"), r#"{"stop":["<end>"]}"#).unwrap();
        std::fs::write(
            mdir.join("latest"),
            r#"{"layers":[{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:aa","size":4},
                          {"mediaType":"application/vnd.ollama.image.params","digest":"sha256:bb","size":10}]}"#,
        )
        .unwrap();
        let found = scan_local(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].reference.display(), "tiny:latest");
        assert!(found[0].problem.is_none());
        assert!(found[0].params.is_some());
    }
}
