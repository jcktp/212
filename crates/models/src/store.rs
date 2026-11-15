//! The local model store: a directory of GGUF files plus a JSON manifest.
//!
//! Imported files are hard-linked into the store when possible (zero extra
//! disk space, and the store copy survives if the original is deleted). If a
//! hard link is impossible (different volume), the original path is recorded.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::estimate::Capabilities;
use crate::gguf::{GgufError, GgufInfo};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ModelSource {
    Local { original: PathBuf },
    HuggingFace { repo: String, file: String },
    Ollama { name: String, tag: String },
}

impl ModelSource {
    pub fn label(&self) -> String {
        match self {
            ModelSource::Local { .. } => "local file".into(),
            ModelSource::HuggingFace { repo, .. } => format!("hf:{repo}"),
            ModelSource::Ollama { name, tag } => format!("ollama:{name}:{tag}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledModel {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    /// The file lives inside the store directory and is deleted on removal.
    pub managed: bool,
    pub source: ModelSource,
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    pub quant: String,
    pub arch: String,
    #[serde(default)]
    pub size_label: Option<String>,
    #[serde(default)]
    pub context_length: Option<u64>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub projector: Option<PathBuf>,
    /// Upstream defaults (e.g. Ollama `params`) preserved for reference.
    #[serde(default)]
    pub upstream_params: Option<serde_json::Value>,
    /// Upstream prompt template (Ollama Go template) preserved for reference.
    /// llama.cpp uses the Jinja template embedded in the GGUF instead.
    #[serde(default)]
    pub upstream_template: Option<String>,
    pub added: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}: no such file")]
    NotFound(PathBuf),
    #[error("{path} is not a usable GGUF model: {source}")]
    Gguf { path: PathBuf, source: GgufError },
    #[error("{0} is a vision projector (mmproj), not a language model")]
    Projector(PathBuf),
    #[error("no installed model matches '{0}'")]
    UnknownModel(String),
    #[error("'{0}' matches several models; be more specific")]
    Ambiguous(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("model manifest is corrupt: {0}")]
    Manifest(String),
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Manifest {
    version: u32,
    models: Vec<InstalledModel>,
}

#[derive(Debug)]
pub struct ModelStore {
    dir: PathBuf,
    models: Vec<InstalledModel>,
}

pub fn slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "model".into() } else { t }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

impl ModelStore {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        std::fs::create_dir_all(dir)?;
        let manifest = dir.join("models.json");
        let models = match std::fs::read_to_string(&manifest) {
            Ok(t) => serde_json::from_str::<Manifest>(&t).map_err(|e| StoreError::Manifest(e.to_string()))?.models,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self { dir: dir.to_path_buf(), models })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn list(&self) -> &[InstalledModel] {
        &self.models
    }

    /// Exact id, then unique id prefix, then case-insensitive name/id substring.
    pub fn find(&self, query: &str) -> Result<&InstalledModel, StoreError> {
        let q = query.trim().to_lowercase();
        if let Some(m) = self.models.iter().find(|m| m.id == q) {
            return Ok(m);
        }
        fn pick<'a>(v: Vec<&'a InstalledModel>, query: &str) -> Option<Result<&'a InstalledModel, StoreError>> {
            match v.len() {
                0 => None,
                1 => Some(Ok(v[0])),
                _ => Some(Err(StoreError::Ambiguous(query.to_string()))),
            }
        }
        if let Some(r) = pick(self.models.iter().filter(|m| m.id.starts_with(&q)).collect(), query) {
            return r;
        }
        if let Some(r) = pick(self.models.iter().filter(|m| m.id.contains(&q) || m.name.to_lowercase().contains(&q)).collect(), query) {
            return r;
        }
        Err(StoreError::UnknownModel(query.to_string()))
    }

    fn save(&self) -> Result<(), StoreError> {
        let m = Manifest { version: 1, models: self.models.clone() };
        let tmp = self.dir.join("models.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&m).map_err(|e| StoreError::Manifest(e.to_string()))?)?;
        std::fs::rename(tmp, self.dir.join("models.json"))?;
        Ok(())
    }

    fn unique_id(&self, base: &str) -> String {
        let base = slug(base);
        if !self.models.iter().any(|m| m.id == base) {
            return base;
        }
        (2..).map(|i| format!("{base}-{i}")).find(|c| !self.models.iter().any(|m| &m.id == c)).unwrap()
    }

    /// Directory where a new model with this id should place its files.
    pub fn model_dir(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// Inspect a GGUF file and build (but do not register) a model record.
    pub fn describe(&self, path: &Path, source: ModelSource, managed: bool) -> Result<InstalledModel, StoreError> {
        if !path.exists() {
            return Err(StoreError::NotFound(path.into()));
        }
        let info = GgufInfo::read_path(path).map_err(|source| StoreError::Gguf { path: path.into(), source })?;
        if info.is_vision_projector() {
            return Err(StoreError::Projector(path.into()));
        }
        let size = std::fs::metadata(path)?.len();
        let quant = info.quantization();
        let fallback = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "model".into());
        let name = match &source {
            ModelSource::Ollama { name, tag } => format!("{name}:{tag}"),
            _ => info.name().map(str::to_string).unwrap_or(fallback.clone()),
        };
        let id_base = match &source {
            ModelSource::Ollama { name, tag } => format!("{name}-{tag}"),
            ModelSource::HuggingFace { .. } | ModelSource::Local { .. } => {
                if fallback.to_lowercase().contains(&quant.to_lowercase()) {
                    fallback.clone()
                } else {
                    format!("{fallback}-{quant}")
                }
            }
        };
        Ok(InstalledModel {
            id: self.unique_id(&id_base),
            name,
            path: path.to_path_buf(),
            managed,
            source,
            size,
            sha256: None,
            quant,
            arch: info.architecture().unwrap_or("unknown").to_string(),
            size_label: info.size_label().map(str::to_string),
            context_length: info.context_length(),
            capabilities: Capabilities::detect(&info, false),
            license: info.license().map(str::to_string),
            projector: None,
            upstream_params: None,
            upstream_template: None,
            added: now_rfc3339(),
        })
    }

    pub fn register(&mut self, model: InstalledModel) -> Result<&InstalledModel, StoreError> {
        let mut model = model;
        if self.models.iter().any(|m| m.id == model.id) {
            model.id = self.unique_id(&model.id);
        }
        self.models.push(model);
        self.save()?;
        Ok(self.models.last().unwrap())
    }

    /// Import an existing GGUF file. Re-importing the same path returns the
    /// existing entry.
    pub fn import_file(&mut self, path: &Path) -> Result<InstalledModel, StoreError> {
        let path = std::fs::canonicalize(path).map_err(|_| StoreError::NotFound(path.into()))?;
        if let Some(existing) =
            self.models.iter().find(|m| m.path == path || matches!(&m.source, ModelSource::Local { original } if *original == path))
        {
            return Ok(existing.clone());
        }
        let mut model = self.describe(&path, ModelSource::Local { original: path.clone() }, false)?;
        let dir = self.model_dir(&model.id);
        std::fs::create_dir_all(&dir)?;
        let link = dir.join(path.file_name().unwrap_or_default());
        match std::fs::hard_link(&path, &link) {
            Ok(()) => {
                model.path = link;
                model.managed = true;
            }
            Err(e) => {
                tracing::info!("hard link failed ({e}); referencing {} in place", path.display());
                let _ = std::fs::remove_dir(&dir);
            }
        }
        Ok(self.register(model)?.clone())
    }

    /// Remove a model from the store. Deletes files only for managed models
    /// (hard links and downloads); externally referenced files are untouched.
    pub fn remove(&mut self, id: &str) -> Result<InstalledModel, StoreError> {
        let id = self.find(id)?.id.clone();
        let idx = self.models.iter().position(|m| m.id == id).unwrap();
        let m = self.models.remove(idx);
        if m.managed {
            let dir = self.model_dir(&m.id);
            if m.path.starts_with(&dir) {
                std::fs::remove_dir_all(&dir)?;
            } else {
                tracing::warn!("managed model outside its directory; not deleting {}", m.path.display());
            }
        }
        self.save()?;
        Ok(m)
    }

    /// Verify that each model's file still exists and looks like GGUF.
    pub fn check(&self) -> Vec<(String, Result<(), String>)> {
        self.models
            .iter()
            .map(|m| {
                let r = if !m.path.exists() {
                    Err(format!("file missing: {}", m.path.display()))
                } else if !crate::gguf::is_gguf(&m.path) {
                    Err("file is not GGUF".into())
                } else {
                    match std::fs::metadata(&m.path) {
                        Ok(md) if md.len() == m.size => Ok(()),
                        Ok(md) => Err(format!("size changed ({} -> {})", m.size, md.len())),
                        Err(e) => Err(e.to_string()),
                    }
                };
                (m.id.clone(), r)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::testutil::{W, build};

    fn write_model(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(
            &p,
            build(&[
                ("general.architecture", W::Str("qwen3")),
                ("general.name", W::Str("Qwen3 Tiny")),
                ("general.file_type", W::U32(7)),
                ("qwen3.context_length", W::U32(40960)),
            ]),
        )
        .unwrap();
        p
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Qwen3 0.6B / Q8_0!"), "qwen3-0.6b-q8_0");
        assert_eq!(slug("---"), "model");
    }

    #[test]
    fn import_find_remove_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let src = write_model(tmp.path(), "qwen3-tiny.gguf");
        let mut store = ModelStore::open(&tmp.path().join("store")).unwrap();
        let m = store.import_file(&src).unwrap();
        assert_eq!(m.id, "qwen3-tiny-q8_0");
        assert_eq!(m.quant, "Q8_0");
        assert_eq!(m.context_length, Some(40960));
        assert!(m.managed, "same volume should hard link");
        assert!(m.path.starts_with(store.dir()));
        // Re-import is idempotent.
        assert_eq!(store.import_file(&src).unwrap().id, m.id);
        assert_eq!(store.list().len(), 1);
        // Persisted.
        let store2 = ModelStore::open(&tmp.path().join("store")).unwrap();
        assert_eq!(store2.find("qwen3-tiny").unwrap().id, m.id);
        assert!(store2.check().iter().all(|(_, r)| r.is_ok()));
        // Remove deletes the link but never the original.
        let mut store2 = store2;
        store2.remove(&m.id).unwrap();
        assert!(!m.path.exists());
        assert!(src.exists());
        assert!(store2.list().is_empty());
    }

    #[test]
    fn rejects_non_gguf_and_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("bad.gguf");
        std::fs::write(&bad, b"not a model at all, definitely").unwrap();
        let mut store = ModelStore::open(&tmp.path().join("store")).unwrap();
        assert!(matches!(store.import_file(&bad), Err(StoreError::Gguf { .. })));
        assert!(matches!(store.import_file(&tmp.path().join("nope.gguf")), Err(StoreError::NotFound(_))));
    }

    #[test]
    fn ids_are_unique_and_find_is_ambiguity_aware() {
        let tmp = tempfile::tempdir().unwrap();
        let a = write_model(tmp.path(), "a.gguf");
        let mut store = ModelStore::open(&tmp.path().join("store")).unwrap();
        let mut m1 = store.describe(&a, ModelSource::Local { original: a.clone() }, false).unwrap();
        m1.id = "same".into();
        let m2 = m1.clone();
        store.register(m1).unwrap();
        let id2 = store.register(m2).unwrap().id.clone();
        assert_eq!(id2, "same-2");
        assert!(matches!(store.find("sam"), Err(StoreError::Ambiguous(_))));
        assert_eq!(store.find("same").unwrap().id, "same");
    }
}
