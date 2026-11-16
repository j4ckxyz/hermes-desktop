//! Connector logos from the catalog's `logo_url`, fetched by us instead of the UI toolkit's
//! image loader: a dedicated client that never carries Hermes credentials, HTTPS only (redirects
//! included), a short timeout, a size cap, image content types only, and a disk cache so the
//! Apps page doesn't refetch on every open.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;

pub const MAX_LOGO_BYTES: usize = 1024 * 1024;
const TTL: Duration = Duration::from_secs(7 * 24 * 3600);

pub fn cache_path(cache_dir: &Path, url: &str) -> PathBuf {
    let h = url.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
    cache_dir.join("logos").join(format!("{h:016x}.img"))
}

fn client(https_only: bool) -> Result<reqwest::Client> {
    let policy = reqwest::redirect::Policy::custom(move |a| {
        if a.previous().len() >= 3 {
            a.error("too many redirects")
        } else if https_only && a.url().scheme() != "https" {
            a.error("redirect away from https")
        } else {
            a.follow()
        }
    });
    Ok(reqwest::Client::builder().https_only(https_only).redirect(policy).timeout(Duration::from_secs(15)).user_agent("hermes-desktop").build()?)
}

/// The cached logo for `url`, downloading it when missing or older than a week.
pub async fn fetch(cache_dir: &Path, url: &str) -> Result<PathBuf> {
    fetch_with(cache_dir, url, true).await
}

/// `https_only` is false only in tests against the local mock.
pub async fn fetch_with(cache_dir: &Path, url: &str, https_only: bool) -> Result<PathBuf> {
    let file = cache_path(cache_dir, url);
    if std::fs::metadata(&file).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < TTL) {
        return Ok(file);
    }
    let r = client(https_only)?.get(url).send().await?;
    if !r.status().is_success() {
        bail!("logo: HTTP {}", r.status());
    }
    let ct = r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
    if !ct.starts_with("image/") {
        bail!("logo: not an image ({ct})");
    }
    if r.content_length().is_some_and(|n| n as usize > MAX_LOGO_BYTES) {
        bail!("logo: too large");
    }
    let mut body = Vec::new();
    let mut s = r.bytes_stream();
    while let Some(chunk) = s.next().await {
        body.extend_from_slice(&chunk?);
        if body.len() > MAX_LOGO_BYTES {
            bail!("logo: too large");
        }
    }
    let dir = file.parent().ok_or_else(|| anyhow!("no cache dir"))?;
    tokio::fs::create_dir_all(dir).await?;
    let part = crate::files::part_path(&file);
    tokio::fs::write(&part, &body).await?;
    tokio::fs::rename(&part, &file).await?;
    Ok(file)
}
