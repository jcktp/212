//! Hugging Face Hub client (documented public REST API).

use serde::Deserialize;

pub const BASE: &str = "https://huggingface.co";

#[derive(Debug, thiserror::Error)]
pub enum HfError {
    #[error("Hugging Face request failed: {0}")]
    Network(String),
    #[error("Hugging Face returned HTTP {0}{1}")]
    Http(u16, &'static str),
    #[error("no GGUF files found in {0}")]
    NoGguf(String),
    #[error("no GGUF file in {repo} matches '{hint}'. Available: {available}")]
    NoMatch { repo: String, hint: String, available: String },
    #[error("invalid model spec '{0}' — expected owner/repo[:QUANT] or owner/repo/file.gguf")]
    BadSpec(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct HfModel {
    pub id: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub likes: u64,
    #[serde(default)]
    pub gated: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Debug, Clone, Deserialize)]
struct Lfs {
    oid: String,
    size: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HfFile {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoInfo {
    #[serde(default, rename = "cardData")]
    pub card_data: Option<serde_json::Value>,
    #[serde(default)]
    pub gated: serde_json::Value,
}

impl RepoInfo {
    pub fn license(&self) -> Option<String> {
        self.card_data.as_ref()?.get("license")?.as_str().map(str::to_string)
    }
    pub fn is_gated(&self) -> bool {
        !matches!(self.gated, serde_json::Value::Bool(false) | serde_json::Value::Null)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HfSpec {
    pub repo: String,
    /// A quantization name (`Q4_K_M`) or exact file name.
    pub hint: Option<String>,
}

impl HfSpec {
    pub fn parse(spec: &str) -> Result<Self, HfError> {
        let s = spec.trim().trim_start_matches("hf:").trim_start_matches("https://huggingface.co/");
        let (repo_part, hint) = match s.split_once(':') {
            Some((r, h)) => (r, Some(h.to_string())),
            None => (s, None),
        };
        let parts: Vec<&str> = repo_part.split('/').filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [owner, name] => Ok(Self { repo: format!("{owner}/{name}"), hint }),
            [owner, name, rest @ ..] if rest.last().is_some_and(|f| f.ends_with(".gguf")) => {
                Ok(Self { repo: format!("{owner}/{name}"), hint: Some(rest.join("/")) })
            }
            _ => Err(HfError::BadSpec(spec.into())),
        }
    }
}

pub struct HfClient {
    client: reqwest::Client,
    token: Option<String>,
    base: String,
}

impl HfClient {
    pub fn new(client: reqwest::Client, token: Option<String>) -> Self {
        Self { client, token, base: BASE.into() }
    }
    pub fn with_base(mut self, base: impl Into<String>) -> Self {
        self.base = base.into();
        self
    }
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str) -> Result<T, HfError> {
        let mut req = self.client.get(url);
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.map_err(|e| HfError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            let hint = match status.as_u16() {
                401 | 403 => " (gated or private: accept the license on huggingface.co and set HF_TOKEN)",
                404 => " (not found)",
                429 => " (rate limited; try again shortly)",
                _ => "",
            };
            return Err(HfError::Http(status.as_u16(), hint));
        }
        resp.json().await.map_err(|e| HfError::Network(e.to_string()))
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<HfModel>, HfError> {
        let url = reqwest::Url::parse_with_params(
            &format!("{}/api/models", self.base),
            &[("search", query), ("filter", "gguf"), ("sort", "downloads"), ("direction", "-1"), ("limit", &limit.to_string())],
        )
        .map_err(|e| HfError::Network(e.to_string()))?;
        self.get_json(url.as_str()).await
    }

    pub async fn repo_info(&self, repo: &str) -> Result<RepoInfo, HfError> {
        self.get_json(&format!("{}/api/models/{repo}", self.base)).await
    }

    pub async fn gguf_files(&self, repo: &str) -> Result<Vec<HfFile>, HfError> {
        let entries: Vec<TreeEntry> = self.get_json(&format!("{}/api/models/{repo}/tree/main?recursive=true", self.base)).await?;
        Ok(entries
            .into_iter()
            .filter(|e| e.kind == "file" && e.path.ends_with(".gguf"))
            .map(|e| HfFile { size: e.lfs.as_ref().map_or(e.size, |l| l.size), sha256: e.lfs.map(|l| l.oid), path: e.path })
            .collect())
    }

    pub fn download_url(&self, repo: &str, path: &str) -> String {
        format!("{}/{repo}/resolve/main/{path}", self.base)
    }
}

/// Is this the n-th part of a split GGUF (`-00001-of-00003.gguf`)?
pub fn split_info(path: &str) -> Option<(u32, u32, &str)> {
    let stem = path.strip_suffix(".gguf")?;
    let (head, of) = stem.rsplit_once("-of-")?;
    let (prefix, n) = head.rsplit_once('-')?;
    Some((n.parse().ok()?, of.parse().ok()?, prefix))
}

fn is_projector(path: &str) -> bool {
    let p = path.to_lowercase();
    p.contains("mmproj") || p.contains("projector")
}

/// Pick the file(s) to download for a hint. Returns all parts of a split model.
/// Without a hint we prefer Q4_K_M, then other common 4–8 bit quants.
pub fn pick(repo: &str, files: &[HfFile], hint: Option<&str>) -> Result<Vec<HfFile>, HfError> {
    let candidates: Vec<&HfFile> = files.iter().filter(|f| !is_projector(&f.path)).collect();
    if candidates.is_empty() {
        return Err(HfError::NoGguf(repo.into()));
    }
    let first = match hint {
        Some(h) if h.ends_with(".gguf") => candidates.iter().find(|f| f.path == h || f.path.ends_with(&format!("/{h}"))),
        Some(h) => {
            let h = h.to_uppercase();
            candidates.iter().find(|f| {
                let up = f.path.to_uppercase();
                let base = up.rsplit('/').next().unwrap_or(&up);
                base.contains(&format!("-{h}."))
                    || base.contains(&format!(".{h}."))
                    || base.contains(&format!("-{h}-"))
                    || base.contains(&format!("_{h}."))
            })
        }
        None => ["Q4_K_M", "Q4_K_S", "Q5_K_M", "Q4_0", "Q8_0", "MXFP4"]
            .iter()
            .find_map(|q| candidates.iter().find(|f| f.path.to_uppercase().contains(q)))
            .or_else(|| candidates.iter().min_by_key(|f| f.size)),
    };
    let Some(first) = first else {
        let mut quants: Vec<String> = candidates.iter().map(|f| f.path.rsplit('/').next().unwrap_or(&f.path).to_string()).collect();
        quants.truncate(12);
        return Err(HfError::NoMatch { repo: repo.into(), hint: hint.unwrap_or("").into(), available: quants.join(", ") });
    };
    match split_info(&first.path) {
        Some((_, total, prefix)) => {
            let mut parts: Vec<HfFile> = candidates
                .iter()
                .filter(|f| split_info(&f.path).is_some_and(|(_, t, p)| t == total && p == prefix))
                .map(|f| (*f).clone())
                .collect();
            parts.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(parts)
        }
        None => Ok(vec![(*first).clone()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(p: &str, size: u64) -> HfFile {
        HfFile { path: p.into(), size, sha256: None }
    }

    #[test]
    fn parses_specs() {
        assert_eq!(HfSpec::parse("Qwen/Qwen3-4B-GGUF").unwrap(), HfSpec { repo: "Qwen/Qwen3-4B-GGUF".into(), hint: None });
        assert_eq!(HfSpec::parse("hf:Qwen/Qwen3-4B-GGUF:Q8_0").unwrap().hint.as_deref(), Some("Q8_0"));
        assert_eq!(HfSpec::parse("Qwen/Qwen3-4B-GGUF/Qwen3-4B-Q8_0.gguf").unwrap().hint.as_deref(), Some("Qwen3-4B-Q8_0.gguf"));
        assert!(HfSpec::parse("qwen").is_err());
    }

    #[test]
    fn picks_default_and_hinted_quant() {
        let files = vec![f("m-Q8_0.gguf", 8), f("m-Q4_K_M.gguf", 4), f("m-Q2_K.gguf", 2), f("mmproj-f16.gguf", 1)];
        assert_eq!(pick("r", &files, None).unwrap()[0].path, "m-Q4_K_M.gguf");
        assert_eq!(pick("r", &files, Some("q8_0")).unwrap()[0].path, "m-Q8_0.gguf");
        assert!(matches!(pick("r", &files, Some("Q6_K")), Err(HfError::NoMatch { .. })));
        assert!(matches!(pick("r", &[f("mmproj.gguf", 1)], None), Err(HfError::NoGguf(_))));
    }

    #[test]
    fn collects_split_parts() {
        let files = vec![f("Q4_K_M/m-Q4_K_M-00002-of-00002.gguf", 5), f("Q4_K_M/m-Q4_K_M-00001-of-00002.gguf", 5), f("m-Q8_0.gguf", 9)];
        let parts = pick("r", &files, Some("Q4_K_M")).unwrap();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].path.ends_with("00001-of-00002.gguf"));
        assert_eq!(split_info("a-b-00003-of-00010.gguf"), Some((3, 10, "a-b")));
    }
}
