//! Resumable, verified HTTP downloads.
//!
//! Data is written to `<dest>.part`. If a partial file exists we resume with a
//! `Range` request (re-hashing the existing bytes first). The final file only
//! appears at `dest` after size and SHA-256 verification succeed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub url: String,
    pub dest: PathBuf,
    /// Lowercase hex SHA-256. When present, the file is verified before use.
    pub sha256: Option<String>,
    pub size: Option<u64>,
    pub bearer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub bytes_per_sec: f64,
    pub verifying: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download cancelled (partial data kept for resume)")]
    Cancelled,
    #[error("HTTP {status} from server{hint}")]
    Http { status: u16, hint: String },
    #[error("network error: {0}")]
    Network(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("size mismatch: expected {expected} bytes, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

async fn hash_existing(path: &Path, hasher: &mut Sha256) -> std::io::Result<u64> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    let mut n_total = 0;
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        n_total += n as u64;
    }
    Ok(n_total)
}

/// Download `spec`, calling `on_progress` at most ~10 times per second.
pub async fn download(
    client: &reqwest::Client,
    spec: &DownloadSpec,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, DownloadError> {
    if let Some(parent) = spec.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_path(&spec.dest);
    let mut hasher = Sha256::new();
    let mut have = match tokio::fs::metadata(&part).await {
        Ok(m) if m.len() > 0 => hash_existing(&part, &mut hasher).await?,
        _ => 0,
    };
    if let Some(size) = spec.size
        && have > size
    {
        // Partial file is larger than the target: start over.
        tokio::fs::remove_file(&part).await?;
        hasher = Sha256::new();
        have = 0;
    }

    let mut total = spec.size;
    let complete_already = spec.size.is_some_and(|s| s == have && s > 0);
    if !complete_already {
        let mut req = client.get(&spec.url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        if let Some(token) = &spec.bearer {
            req = req.bearer_auth(token);
        }
        let resp = tokio::select! {
            r = req.send() => r.map_err(|e| DownloadError::Network(e.to_string()))?,
            _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
        };
        let status = resp.status();
        let mut file = if status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0 {
            tokio::fs::OpenOptions::new().append(true).open(&part).await?
        } else if status.is_success() {
            // Server ignored the range: restart from zero.
            hasher = Sha256::new();
            have = 0;
            tokio::fs::File::create(&part).await?
        } else if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE && have > 0 {
            // Most likely already complete; verification below decides.
            tokio::fs::OpenOptions::new().append(true).open(&part).await?
        } else {
            let hint = match status.as_u16() {
                401 | 403 => ": this model may be gated — accept its license on huggingface.co and set HF_TOKEN".to_string(),
                404 => ": file not found".to_string(),
                _ => String::new(),
            };
            return Err(DownloadError::Http { status: status.as_u16(), hint });
        };
        if status.is_success()
            && let Some(len) = resp.content_length()
        {
            total = Some(have + len);
        }

        let mut stream = resp.bytes_stream();
        let start = Instant::now();
        let start_have = have;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut writer = tokio::io::BufWriter::with_capacity(1 << 20, &mut file);
        loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.cancelled() => {
                    writer.flush().await?;
                    return Err(DownloadError::Cancelled);
                }
            };
            let Some(chunk) = chunk else { break };
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    // Keep what we have so the next attempt can resume.
                    writer.flush().await?;
                    return Err(DownloadError::Network(e.to_string()));
                }
            };
            hasher.update(&chunk);
            writer.write_all(&chunk).await?;
            have += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(100) {
                last_emit = Instant::now();
                let secs = start.elapsed().as_secs_f64().max(0.001);
                on_progress(Progress { downloaded: have, total, bytes_per_sec: (have - start_have) as f64 / secs, verifying: false });
            }
        }
        writer.flush().await?;
        file.sync_all().await?;
    }

    on_progress(Progress { downloaded: have, total, bytes_per_sec: 0.0, verifying: true });
    if let Some(expected) = spec.size.or(total)
        && expected != have
    {
        // Keep the part file only if it is short (resumable); otherwise discard.
        if have > expected {
            let _ = tokio::fs::remove_file(&part).await;
        }
        return Err(DownloadError::Size { expected, actual: have });
    }
    let actual = hex::encode(hasher.finalize());
    if let Some(expected) = &spec.sha256
        && !expected.eq_ignore_ascii_case(&actual)
    {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(DownloadError::Checksum { expected: expected.clone(), actual });
    }
    tokio::fs::rename(&part, &spec.dest).await?;
    Ok(spec.dest.clone())
}

/// SHA-256 of a file on disk (blocking; run on a blocking thread for large files).
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal HTTP/1.1 server with Range support. `fail_after` truncates the
    /// first response to simulate a dropped connection.
    async fn serve(body: Arc<Vec<u8>>, fail_after: Option<usize>, honor_range: bool) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                let body = body.clone();
                let n = hits2.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let mut buf = vec![0; 4096];
                    let len = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..len]).to_lowercase();
                    let start = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').trim_end_matches("-\r").split('-').next()?.trim().parse::<usize>().ok())
                        .filter(|_| honor_range)
                        .unwrap_or(0);
                    let slice = &body[start.min(body.len())..];
                    let status = if start > 0 { "206 Partial Content" } else { "200 OK" };
                    let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", slice.len());
                    let _ = sock.write_all(head.as_bytes()).await;
                    let send = match fail_after {
                        Some(k) if n == 0 => &slice[..k.min(slice.len())],
                        _ => slice,
                    };
                    let _ = sock.write_all(send).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        (format!("http://{addr}/file.gguf"), hits)
    }

    fn body() -> Arc<Vec<u8>> {
        Arc::new((0..300_000u32).map(|i| (i % 251) as u8).collect())
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let data = body();
        let sha = hex::encode(Sha256::digest(&*data));
        let (url, _) = serve(data.clone(), None, true).await;
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec { url, dest: dir.path().join("m.gguf"), sha256: Some(sha), size: Some(data.len() as u64), bearer: None };
        let mut last = None;
        let out = download(&crate::http_client(), &spec, &CancellationToken::new(), |p| last = Some(p)).await.unwrap();
        assert_eq!(std::fs::read(out).unwrap(), *data);
        assert!(last.unwrap().verifying);
        assert!(!part_path(&spec.dest).exists());
    }

    #[tokio::test]
    async fn resumes_after_interruption() {
        let data = body();
        let sha = hex::encode(Sha256::digest(&*data));
        let (url, hits) = serve(data.clone(), Some(100_000), true).await;
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec { url, dest: dir.path().join("m.gguf"), sha256: Some(sha), size: Some(data.len() as u64), bearer: None };
        let client = crate::http_client();
        let first = download(&client, &spec, &CancellationToken::new(), |_| {}).await;
        assert!(matches!(first, Err(DownloadError::Size { .. }) | Err(DownloadError::Network(_))), "{first:?}");
        assert_eq!(std::fs::metadata(part_path(&spec.dest)).unwrap().len(), 100_000);
        download(&client, &spec, &CancellationToken::new(), |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&spec.dest).unwrap(), *data);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn restarts_when_server_ignores_range() {
        let data = body();
        let sha = hex::encode(Sha256::digest(&*data));
        let (url, _) = serve(data.clone(), None, false).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("m.gguf");
        std::fs::write(part_path(&dest), &data[..5000]).unwrap();
        let spec = DownloadSpec { url, dest: dest.clone(), sha256: Some(sha), size: Some(data.len() as u64), bearer: None };
        download(&crate::http_client(), &spec, &CancellationToken::new(), |_| {}).await.unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *data);
    }

    #[tokio::test]
    async fn checksum_mismatch_discards_file() {
        let data = body();
        let (url, _) = serve(data.clone(), None, true).await;
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec { url, dest: dir.path().join("m.gguf"), sha256: Some("00".repeat(32)), size: None, bearer: None };
        let r = download(&crate::http_client(), &spec, &CancellationToken::new(), |_| {}).await;
        assert!(matches!(r, Err(DownloadError::Checksum { .. })));
        assert!(!spec.dest.exists());
        assert!(!part_path(&spec.dest).exists());
    }

    #[tokio::test]
    async fn cancellation_keeps_partial() {
        let data = body();
        let (url, _) = serve(data, None, true).await;
        let dir = tempfile::tempdir().unwrap();
        let spec = DownloadSpec { url, dest: dir.path().join("m.gguf"), sha256: None, size: None, bearer: None };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let r = download(&crate::http_client(), &spec, &cancel, |_| {}).await;
        assert!(matches!(r, Err(DownloadError::Cancelled)));
        assert!(!spec.dest.exists());
    }
}
