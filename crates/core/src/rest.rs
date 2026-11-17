//! Dashboard REST (`hermes serve`). Auth comes from [`crate::auth::Auth`]: `Authorization: Bearer`
//! after a sign-in, `X-Hermes-Session-Token` for a token or a loopback dashboard. A 401 renews the
//! credential once and retries. Profile selection: `?profile=`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use std::sync::Arc;

use reqwest::Method;

use crate::files;
use crate::auth::Auth;

/// A 404 from the dashboard. For a session the gateway has just created or resumed, it means
/// the session has no stored rows yet (a row appears with its first prompt).
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hermes answered 404 Not Found {}", self.0)
    }
}

impl std::error::Error for NotFound {}

#[derive(Clone)]
pub struct Rest {
    pub auth: Arc<Auth>,
    http: reqwest::Client,
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

impl Rest {
    pub fn new(auth: Arc<Auth>) -> Result<Rest> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            // A stalled server must not hang history or a download forever.
            .read_timeout(std::time::Duration::from_secs(45))
            // Never carry the session token to another origin.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("hermes-desktop/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Rest { auth, http })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.auth.base.as_str().trim_end_matches('/'), path)
    }

    /// Send with the current credential; on 401 renew it once (refresh, or a silent sign-in) and
    /// retry.
    async fn send(&self, m: Method, path: &str, body: Option<&Value>) -> Result<reqwest::Response> {
        let mut renewed = false;
        loop {
            let h = self.auth.headers().await.map_err(|e| anyhow!("{e}"))?;
            let mut b = self.http.request(m.clone(), self.url(path));
            for (k, v) in &h {
                b = b.header(*k, v);
            }
            if let Some(j) = body {
                b = b.json(j);
            }
            let r = b.send().await.map_err(|e| anyhow!("Couldn't reach Hermes: {e}"))?;
            if r.status().as_u16() == 401 && !renewed {
                if let Some((_, v)) = h.first() {
                    renewed = true;
                    if self.auth.rejected(v).await.is_ok() {
                        continue;
                    }
                }
            }
            return Ok(r);
        }
    }

    async fn json(&self, m: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        // Whole-call limit on top of the read timeout (uploads get longer).
        let limit = std::time::Duration::from_secs(if m == Method::GET { 60 } else { 300 });
        tokio::time::timeout(limit, self.json_inner(m, path, body)).await.map_err(|_| anyhow!("Hermes took too long to answer."))?
    }

    async fn json_inner(&self, m: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let r = self.send(m, path, body).await?;
        let st = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if st.as_u16() == 401 {
            bail!("Sign in to this Hermes again.");
        }
        if st.as_u16() == 403 {
            bail!("Hermes refused that ({st}).");
        }
        if !st.is_success() {
            let d = body.get("detail").map(|d| d.to_string()).unwrap_or_default();
            if st.as_u16() == 404 {
                return Err(NotFound(d).into());
            }
            bail!("Hermes answered {st} {d}");
        }
        Ok(body)
    }

    /// `GET /api/status`: reachability and auth providers.
    pub async fn status(&self) -> Result<Value> {
        self.json(Method::GET, "/api/status", None).await
    }

    /// `GET /api/auth/me`: the verified identity (any valid credential; source `dashboard_auth/routes.py`).
    pub async fn me(&self) -> Result<Value> {
        self.json(Method::GET, "/api/auth/me", None).await
    }

    /// `GET /api/analytics/usage?days=&profile=`: daily and per-model tokens, cost, sessions and
    /// API calls from the profile's session store (source `hermes_cli/web_routers/analytics.py`).
    pub async fn usage(&self, profile: Option<&str>, days: u32) -> Result<Value> {
        let mut path = format!("/api/analytics/usage?days={days}");
        if let Some(p) = profile {
            path.push_str(&format!("&profile={}", enc(p)));
        }
        self.json(Method::GET, &path, None).await
    }

    /// `GET /api/sessions?order=recent&min_messages=1`: a profile's conversations by latest
    /// activity across each compression chain, with `last_active` (source
    /// `hermes_cli/web_routers/sessions.py`). Empty sessions, cron runs (as the official desktop's
    /// recents leave them out) and internal sources (`INTERNAL_LISTING_SOURCES`) are excluded.
    pub async fn sessions(&self, profile: &str, limit: usize) -> Result<Vec<Value>> {
        let path = format!("/api/sessions?profile={}&limit={limit}&order=recent&min_messages=1&exclude_sources={}", enc(profile), crate::wire::NOT_CONVERSATIONS.join(","));
        let v = self.json(Method::GET, &path, None).await?;
        Ok(v.get("sessions").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Stored history of a session, newest page (source `hermes_cli/web_routers/sessions.py`).
    pub async fn messages(&self, session_id: &str, profile: &str, limit: u32) -> Result<Vec<Value>> {
        let path = format!(
            "/api/sessions/{}/messages?profile={}&limit={limit}&order=latest&include_compacted=true",
            enc(session_id),
            enc(profile)
        );
        let v = self.json(Method::GET, &path, None).await?;
        Ok(v.get("messages").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// `POST /api/chat/image-upload?profile=` → `{path, mime_type, bytes}`
    /// (source `hermes_cli/web_routers/files.py`).
    pub async fn upload_image(&self, profile: &str, filename: &str, data_url: String) -> Result<Value> {
        let path = format!("/api/chat/image-upload?profile={}", enc(profile));
        self.json(Method::POST, &path, Some(&json!({"data_url": data_url, "filename": filename}))).await
    }

    /// Stream a file from the Hermes host without buffering it in memory
    /// (`GET /api/files/download?path=`). It lands at `save_as` when the user picked a path in a
    /// save dialog (replacing what they agreed to replace), else under `dest_dir` with a portable,
    /// non-clobbering name. Downloads are written to a private `.part` file first, removed on
    /// failure, and flagged as coming from the network.
    pub async fn download(&self, remote: &str, profile: &str, dest_dir: &Path, save_as: Option<&Path>, progress: impl Fn(u64, Option<u64>)) -> Result<PathBuf> {
        let path = format!("/api/files/download?path={}&profile={}", enc(remote), enc(profile));
        let r = self.send(Method::GET, &path, None).await?;
        let st = r.status();
        if !st.is_success() {
            bail!(match st.as_u16() {
                404 => "That file is no longer on the Hermes host.".to_owned(),
                403 | 401 => "Hermes won't share that file.".to_owned(),
                _ => format!("Download failed ({st})."),
            });
        }
        let total = r.content_length();
        let target = match save_as {
            Some(p) => p.to_path_buf(),
            None => {
                tokio::fs::create_dir_all(dest_dir).await?;
                files::unique(dest_dir, &files::safe_file_name(remote))
            }
        };
        let part = files::part_path(&target);
        let result: Result<()> = async {
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
            if save_as.is_some() && tokio::fs::metadata(&target).await.is_ok() {
                // Windows can't rename over an existing file.
                let _ = tokio::fs::remove_file(&target).await;
            }
            tokio::fs::rename(&part, &target).await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
        files::mark_downloaded(&target);
        Ok(target)
    }

    /// Inline image bytes: `GET /api/media?path=` → `{data_url}` (≤ 25 MB).
    pub async fn media_data_url(&self, remote: &str, profile: &str) -> Result<String> {
        let path = format!("/api/media?path={}&profile={}", enc(remote), enc(profile));
        let v = self.json(Method::GET, &path, None).await?;
        v.get("data_url").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| anyhow!("no image data"))
    }
}

pub use crate::files::unique;
