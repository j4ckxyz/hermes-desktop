//! Connections: which Hermes backends the user configured. Only names and URLs are stored on
//! disk. Session tokens and API keys live in the OS keychain (`keyring`: macOS Keychain, Windows
//! Credential Manager, Linux Secret Service).

use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub prefs: Prefs,
    /// Last window size (logical pixels), restored at launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_size: Option<(f32, f32)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}

/// What to do before sending when the bot's stream has been quiet for a long time. Hermes
/// itself never resets a conversation on inactivity (docs: *Session continuity*), so the client
/// asks for it with official calls: `session.compress` or a fresh canonical `session.create`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GapAction {
    /// Keep the same context.
    Off,
    /// Summarise older turns into a fresh working context, same session.
    #[default]
    Compact,
    /// Start a new canonical Bot Chat session; the old one stays in the stream.
    Fresh,
}

/// Local, per-machine preferences (Settings window). Every field has a default so older and newer
/// config files both load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: ThemePref,
    /// UI font size in px (12 to 18).
    pub font_size: f32,
    pub density: Density,
    pub notifications: bool,
    pub notify_only_unfocused: bool,
    pub sounds: bool,
    pub enter_to_send: bool,
    pub render_markdown: bool,
    pub show_timestamps: bool,
    pub show_tool_activity: bool,
    pub download_dir: Option<String>,
    pub image_previews: bool,
    pub hide_previews: bool,
    pub log_level: String,
    pub network_debug: bool,
    pub gap_action: GapAction,
    /// Quiet time, in hours, that counts as a long break.
    pub gap_hours: u32,
    pub sidebar_collapsed: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            theme: ThemePref::System,
            font_size: 14.0,
            density: Density::Comfortable,
            notifications: true,
            notify_only_unfocused: true,
            sounds: false,
            enter_to_send: true,
            render_markdown: true,
            show_timestamps: true,
            show_tool_activity: true,
            download_dir: None,
            image_previews: true,
            hide_previews: false,
            log_level: "warn".into(),
            network_debug: false,
            gap_action: GapAction::Compact,
            gap_hours: 6,
            sidebar_collapsed: false,
        }
    }
}

impl Prefs {
    pub fn font_size(&self) -> f32 {
        self.font_size.clamp(12.0, 18.0)
    }
    /// Spacing multiplier for the chosen density.
    pub fn space(&self) -> f32 {
        match self.density {
            Density::Compact => 0.75,
            Density::Comfortable => 1.0,
            Density::Spacious => 1.25,
        }
    }
}

pub fn dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("dev", "hermes-desktop", "Hermes Desktop")
}

#[cfg(test)]
mod prefs_tests {
    use super::*;
    #[test]
    fn old_configs_get_default_prefs_and_partial_prefs_merge() {
        let c: Config = serde_json::from_str(r#"{"connections": []}"#).unwrap();
        assert_eq!(c.prefs, Prefs::default());
        let c: Config = serde_json::from_str(r#"{"connections": [], "prefs": {"theme": "light", "font_size": 40}}"#).unwrap();
        assert_eq!(c.prefs.theme, ThemePref::Light);
        assert_eq!(c.prefs.font_size(), 18.0);
        assert!(c.prefs.enter_to_send);
    }
}

/// A developer/test override from the environment. Release builds ignore every one of them, so
/// an end user's environment can't inject a server, token, script or alternate home.
pub fn dev_env(name: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(name).ok()
    } else {
        None
    }
}

pub fn config_path() -> PathBuf {
    if let Some(p) = dev_env("HERMES_DESKTOP_HOME") {
        return PathBuf::from(p).join("config.json");
    }
    dirs().map(|d| d.config_dir().join("config.json")).unwrap_or_else(|| PathBuf::from("hermes-desktop-config.json"))
}

pub fn data_dir() -> PathBuf {
    if let Some(p) = dev_env("HERMES_DESKTOP_HOME") {
        return PathBuf::from(p);
    }
    dirs().map(|d| d.data_dir().to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

impl Config {
    pub fn load() -> Config {
        Self::load_from(&config_path())
    }

    /// A file that exists but doesn't parse is kept as `config.json.bak` before defaults are
    /// used, so a bad edit or a newer version's file never silently loses the server list.
    pub fn load_from(p: &Path) -> Config {
        let Ok(bytes) = std::fs::read(p) else { return Config::default() };
        match serde_json::from_slice(&bytes) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("{} is unreadable ({e}); keeping a copy as .bak", p.display());
                let _ = std::fs::write(p.with_extension("json.bak"), &bytes);
                Config::default()
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    /// Write to a temporary file in the same directory, then rename over the old one: a crash or
    /// full disk mid-write leaves the previous config intact.
    pub fn save_to(&self, p: &Path) -> Result<()> {
        let dir = p.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".config.{}.tmp", uuid::Uuid::new_v4().simple()));
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(self)?)?;
            f.sync_all()?;
            std::fs::rename(&tmp, p)
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            anyhow!(e)
        }).with_context(|| format!("writing {}", p.display()))
    }
}

/// Keychain access for a connection's secret.
pub struct Secrets;

/// Tests and scripted runs set `HERMES_DESKTOP_EPHEMERAL_SECRETS=1` to keep secrets in
/// process memory instead of the real keychain (never on disk either way).
fn ephemeral() -> Option<&'static std::sync::Mutex<std::collections::HashMap<String, String>>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
    dev_env("HERMES_DESKTOP_EPHEMERAL_SECRETS").map(|_| M.get_or_init(Default::default))
}

impl Secrets {
    fn entry(conn_id: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, conn_id).map_err(|e| anyhow!("keychain: {e}"))
    }
    pub fn get(conn_id: &str) -> Option<String> {
        // Dev/test override; never persisted.
        if let Some(t) = dev_env(&format!("HERMES_TOKEN_{}", conn_id.to_uppercase().replace('-', "_"))) {
            return Some(t);
        }
        if let Some(m) = ephemeral() {
            return m.lock().unwrap().get(conn_id).cloned();
        }
        Self::entry(conn_id).ok()?.get_password().ok()
    }
    pub fn set(conn_id: &str, secret: &str) -> Result<()> {
        if let Some(m) = ephemeral() {
            m.lock().unwrap().insert(conn_id.to_owned(), secret.to_owned());
            return Ok(());
        }
        Self::entry(conn_id)?.set_password(secret).map_err(|e| anyhow!("keychain: {e}"))
    }
    pub fn delete(conn_id: &str) {
        if let Some(m) = ephemeral() {
            m.lock().unwrap().remove(conn_id);
            return;
        }
        if let Ok(e) = Self::entry(conn_id) {
            let _ = e.delete_credential();
        }
    }
}

/// A resolved backend address plus credential.
#[derive(Clone)]
pub struct Endpoint {
    pub base: Url,
    pub token: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Endpoint({}, token={})", self.base, if self.token.is_empty() { "none" } else { "***" })
    }
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
    #[test]
    fn config_writes_are_atomic_and_bad_files_are_kept() {
        let dir = std::env::temp_dir().join(format!("hd-cfg-{}", uuid::Uuid::new_v4().simple()));
        let p = dir.join("config.json");
        let c = Config { user_name: Some("Ada".into()), ..Default::default() };
        c.save_to(&p).unwrap();
        assert_eq!(Config::load_from(&p).user_name.as_deref(), Some("Ada"));
        // No temporary files left next to it.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::write(&p, b"{ not json").unwrap();
        assert!(Config::load_from(&p).connections.is_empty());
        assert_eq!(std::fs::read(dir.join("config.json.bak")).unwrap(), b"{ not json");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dev_overrides_exist_only_in_debug_builds() {
        std::env::set_var("HERMES_DESKTOP_TEST_PROBE", "1");
        assert_eq!(dev_env("HERMES_DESKTOP_TEST_PROBE").is_some(), cfg!(debug_assertions));
        assert!(dev_env("HERMES_DESKTOP_TEST_UNSET").is_none());
    }

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
