//! Dashboard REST (`hermes serve`). Auth: `X-Hermes-Session-Token` (docs: *Hermes Desktop*,
//! *Web Dashboard → Deployment session-token provider*). Profile selection: `?profile=`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::conn::Endpoint;

#[derive(Clone)]
pub struct Rest {
    pub ep: Endpoint,
    http: reqwest::Client,
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

impl Rest {
    pub fn new(ep: Endpoint) -> Result<Rest> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("hermes-desktop/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Rest { ep, http })
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let b = self.http.request(m, self.ep.http(path));
        if self.ep.token.is_empty() { b } else { b.header("X-Hermes-Session-Token", &self.ep.token) }
    }

    async fn json(&self, b: reqwest::RequestBuilder) -> Result<Value> {
        let r = b.send().await.map_err(|e| anyhow!("Couldn't reach Hermes: {e}"))?;
        let st = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if st.as_u16() == 401 || st.as_u16() == 403 {
            bail!("Hermes refused the session token ({st}).");
        }
        if !st.is_success() {
            let d = body.get("detail").map(|d| d.to_string()).unwrap_or_default();
            bail!("Hermes answered {st} {d}");
        }
        Ok(body)
    }

    /// `GET /api/status`: reachability and auth providers.
    pub async fn status(&self) -> Result<Value> {
        self.json(self.req(reqwest::Method::GET, "/api/status")).await
    }

    /// Stored history of a session, newest page (source `hermes_cli/web_routers/sessions.py`).
    pub async fn messages(&self, session_id: &str, profile: &str, limit: u32) -> Result<Vec<Value>> {
        let path = format!(
            "/api/sessions/{}/messages?profile={}&limit={limit}&order=latest&include_compacted=true",
            enc(session_id),
            enc(profile)
        );
        let v = self.json(self.req(reqwest::Method::GET, &path)).await?;
        Ok(v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// `POST /api/chat/image-upload?profile=` → `{path, mime_type, bytes}`
    /// (source `hermes_cli/web_routers/files.py`).
    pub async fn upload_image(&self, profile: &str, filename: &str, data_url: String) -> Result<Value> {
        let path = format!("/api/chat/image-upload?profile={}", enc(profile));
        self.json(self.req(reqwest::Method::POST, &path).json(&json!({"data_url": data_url, "filename": filename}))).await
    }

    /// Stream a file from the Hermes host to `dest_dir` without buffering it in memory.
    /// `GET /api/files/download?path=`.
    pub async fn download(&self, remote: &str, profile: &str, dest_dir: &Path, progress: impl Fn(u64, Option<u64>)) -> Result<PathBuf> {
        let path = format!("/api/files/download?path={}&profile={}", enc(remote), enc(profile));
        let r = self.req(reqwest::Method::GET, &path).send().await?;
        let st = r.status();
        if !st.is_success() {
            bail!(match st.as_u16() {
                404 => "That file is no longer on the Hermes host.".to_owned(),
                403 | 401 => "Hermes won't share that file.".to_owned(),
                _ => format!("Download failed ({st})."),
            });
        }
        let total = r.content_length();
        tokio::fs::create_dir_all(dest_dir).await?;
        let name = remote.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or("file");
        let target = unique(dest_dir, name);
        let part = target.with_extension("part");
        let mut f = tokio::fs::File::create(&part).await?;
        let mut got = 0u64;
        let mut s = r.bytes_stream();
        while let Some(chunk) = s.next().await {
            let chunk = chunk?;
            got += chunk.len() as u64;
            f.write_all(&chunk).await?;
            progress(got, total);
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&part, &target).await?;
        Ok(target)
    }

    /// Inline image bytes: `GET /api/media?path=` → `{data_url}` (≤ 25 MB).
    pub async fn media_data_url(&self, remote: &str, profile: &str) -> Result<String> {
        let path = format!("/api/media?path={}&profile={}", enc(remote), enc(profile));
        let v = self.json(self.req(reqwest::Method::GET, &path)).await?;
        v.get("data_url").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| anyhow!("no image data"))
    }
}

/// `name`, or `name (2)` etc. when it exists.
pub fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_owned(), format!(".{e}")),
        None => (name.to_owned(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}
