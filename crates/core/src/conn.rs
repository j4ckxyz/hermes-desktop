//! Connections: which Hermes backends the user configured. Only names and URLs are stored on
//! disk. Session tokens and API keys live in the OS keychain (`keyring`: macOS Keychain, Windows
//! Credential Manager, Linux Secret Service).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use url::Url;

const KEYRING_SERVICE: &str = "dev.hermes-desktop";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnKind {
    /// `hermes serve` / dashboard backend: REST + `/api/ws` gateway (full Bot Mode).
    Gateway,
    /// OpenAI-compatible API server only (`/v1`); `profile` set means multiplexed
    /// `/p/<profile>/v1`.
    ApiServer { profile: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    /// Device name shown on badges and used to qualify handles ("Homelab").
    pub name: String,
    pub base_url: String,
    pub kind: ConnKind,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub connections: Vec<Connection>,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub user_name: Option<String>,
}

pub fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "hermes-desktop", "Hermes Desktop")
}

pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("HERMES_DESKTOP_HOME") {
        return PathBuf::from(p).join("config.json");
    }
    dirs().map(|d| d.config_dir().join("config.json")).unwrap_or_else(|| PathBuf::from("hermes-desktop-config.json"))
}

pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("HERMES_DESKTOP_HOME") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.data_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

impl Config {
    pub fn load() -> Config {
        std::fs::read(config_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let p = config_path();
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&p, serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", p.display()))
    }
}

/// Keychain access for a connection's secret.
pub struct Secrets;

impl Secrets {
    fn entry(conn_id: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, conn_id).map_err(|e| anyhow!("keychain: {e}"))
    }
    pub fn get(conn_id: &str) -> Option<String> {
        // Dev/test override; never persisted.
        if let Ok(t) = std::env::var(format!("HERMES_TOKEN_{}", conn_id.to_uppercase().replace('-', "_"))) {
            return Some(t);
        }
        Self::entry(conn_id).ok()?.get_password().ok()
    }
    pub fn set(conn_id: &str, secret: &str) -> Result<()> {
        Self::entry(conn_id)?.set_password(secret).map_err(|e| anyhow!("keychain: {e}"))
    }
    pub fn delete(conn_id: &str) {
        if let Ok(e) = Self::entry(conn_id) {
            let _ = e.delete_credential();
        }
    }
}

/// A resolved backend address plus credential.
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub base: Url,
    pub token: String,
}

impl Endpoint {
    pub fn new(base_url: &str, token: &str) -> Result<Endpoint> {
        let mut s = base_url.trim().to_owned();
        if !s.contains("://") {
            s = format!("http://{s}");
        }
        // A pasted dashboard URL may carry a path such as /login or /sessions; reverse-proxy
        // prefixes are kept.
        let mut base = Url::parse(&s).map_err(|_| anyhow!("That doesn't look like an address: {base_url}"))?;
        base.set_query(None);
        base.set_fragment(None);
        let path = base.path().trim_end_matches('/').to_owned();
        let path = path.strip_suffix("/login").unwrap_or(&path).to_owned();
        base.set_path(&path);
        Ok(Endpoint { base, token: token.to_owned() })
    }

    /// `path` must start with `/`.
    pub fn http(&self, path: &str) -> String {
        format!("{}{}", self.base.as_str().trim_end_matches('/'), path)
    }

    /// `/api/ws?token=…` as the official desktop builds it for a token connection
    /// (`apps/desktop/electron/connection-config.ts`).
    pub fn ws_url(&self) -> String {
        let mut u = self.base.clone();
        let scheme = if u.scheme() == "https" { "wss" } else { "ws" };
        let _ = u.set_scheme(scheme);
        let mut s = format!("{}/api/ws", u.as_str().trim_end_matches('/'));
        if !self.token.is_empty() {
            s.push_str("?token=");
            s.push_str(&url::form_urlencoded::byte_serialize(self.token.as_bytes()).collect::<String>());
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_normalizes() {
        let e = Endpoint::new("homelab.ts.net:9119/login?next=/", "a b").unwrap();
        assert_eq!(e.http("/api/status"), "http://homelab.ts.net:9119/api/status");
        assert_eq!(e.ws_url(), "ws://homelab.ts.net:9119/api/ws?token=a+b");
        let e = Endpoint::new("https://x.example/hermes/", "t").unwrap();
        assert_eq!(e.ws_url(), "wss://x.example/hermes/api/ws?token=t");
    }
}
