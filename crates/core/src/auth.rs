//! Signing in to a Hermes dashboard with the official mechanisms only (see docs/PROTOCOL.md →
//! "Sign-in"). Sources: `hermes_cli/dashboard_auth/routes.py`, `…/middleware.py`,
//! `…/ws_tickets.py`, `hermes_cli/gateway_connections_oauth.py`, and the docs pages *Web
//! Dashboard → Authentication (gated mode)* and *Desktop Native Sign-In (RFC 8252)*.
//!
//! * **Discovery**: `GET /api/status` (`version`, `auth_required`, `auth_flows`, `auth_providers`)
//!   and `GET /api/auth/providers` (`{providers:[{name, display_name, supports_password}]}`).
//! * **Username/password** (gated dashboards): the RFC 8252 native flow the official Desktop uses,
//!   driven without a browser because password providers need no redirect:
//!   `GET /auth/native/authorize` (our PKCE challenge + a loopback `redirect_uri`, sets the
//!   `hermes_session_pkce` cookie) → `POST /auth/password-login {provider, username, password}`
//!   with that cookie → `{ok, next: "<redirect_uri>?code=…&state=…"}` → `POST /auth/native/token
//!   {code, code_verifier}` → `{access_token, refresh_token, expires_at, provider, user_id}`.
//!   No session cookie is ever kept; REST uses `Authorization: Bearer`.
//! * **Refresh**: `POST /auth/native/refresh {refresh_token, provider}`; 401 `session_expired`
//!   means sign in again (silently, when the user let us remember the password).
//! * **WebSocket**: `POST /api/auth/ws-ticket` (Bearer) → `{ticket, ttl_seconds: 30}`, single use,
//!   presented as `/api/ws?ticket=`. A fresh ticket is minted for every (re)connect.
//! * **Loopback dashboards** (no accounts): the page injects `window.__HERMES_SESSION_TOKEN__`,
//!   which rotates on every restart; it is read again whenever it is refused.
//! * **Token** (advanced): a deployment session token (`X-Hermes-Session-Token`, `?token=`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::conn::Secrets;

/// The dashboard's default port (`hermes dashboard`), then the Webapp's.
pub const LOCAL_PORTS: [u16; 2] = [9119, 9120];
/// Refresh this long before the access token lapses (the official clients use 60–120 s).
const REFRESH_MARGIN_SECS: i64 = 120;
/// Never visited: the server validates that it is a canonical loopback literal and returns it
/// with the code in the JSON body of `/auth/password-login`.
const NATIVE_REDIRECT: &str = "http://127.0.0.1:53682/callback";
const UA: &str = concat!("hermes-desktop/", env!("CARGO_PKG_VERSION"));

/// What the keychain holds for a connection (one JSON entry per connection id).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// Nothing yet (or signed out).
    None,
    /// Deployment session token or dashboard token pasted in Advanced.
    Token { token: String },
    /// RFC 8252 bearer tokens from a password (or native) sign-in.
    Native {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        expires_at: i64,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        user_id: String,
    },
    /// Loopback dashboard without accounts: the token is read from the page when needed.
    Loopback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub cred: Credential,
    /// Password provider name, for silent re-authentication.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    /// Kept only when the user chose "Stay signed in" (keychain only, never on disk).
    #[serde(default)]
    pub password: Option<String>,
}

impl Default for Stored {
    fn default() -> Self {
        Stored { cred: Credential::None, provider: None, username: None, password: None }
    }
}

impl Stored {
    /// Read from the keychain. Entries saved by 0.4 and earlier are a bare token string.
    pub fn load(conn_id: &str) -> Stored {
        match Secrets::get(conn_id) {
            Some(s) if s.trim_start().starts_with('{') => serde_json::from_str(&s).unwrap_or_default(),
            Some(s) if !s.is_empty() => Stored { cred: Credential::Token { token: s }, ..Default::default() },
            _ => Stored::default(),
        }
    }
    pub fn save(&self, conn_id: &str) -> anyhow::Result<()> {
        Secrets::set(conn_id, &serde_json::to_string(self)?)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AuthError {
    InvalidCredentials,
    RateLimited,
    /// The session can't be renewed and there is no remembered password.
    SignInRequired,
    Unreachable(String),
    Tls(String),
    Timeout,
    NotHermes,
    Unsupported(String),
    Server(u16, String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::InvalidCredentials => write!(f, "Wrong username or password."),
            AuthError::RateLimited => write!(f, "Too many attempts. Wait a minute, then try again."),
            AuthError::SignInRequired => write!(f, "Your session ended. Sign in again."),
            AuthError::Unreachable(m) => write!(f, "Couldn't reach the server. {m}"),
            AuthError::Tls(m) => write!(f, "Secure connection failed: {m}"),
            AuthError::Timeout => write!(f, "The server didn't answer in time."),
            AuthError::NotHermes => write!(f, "That address answered, but it isn't a Hermes dashboard."),
            AuthError::Unsupported(m) => write!(f, "{m}"),
            AuthError::Server(s, m) if m.is_empty() => write!(f, "The server answered {s}."),
            AuthError::Server(s, m) => write!(f, "The server answered {s}: {m}"),
        }
    }
}

impl std::error::Error for AuthError {}

fn classify(e: &reqwest::Error) -> AuthError {
    if e.is_timeout() {
        return AuthError::Timeout;
    }
    // Walk the source chain: rustls / webpki errors surface as TLS problems.
    let mut chain = String::new();
    let mut src: Option<&dyn std::error::Error> = Some(e);
    while let Some(s) = src {
        chain.push_str(&s.to_string().to_lowercase());
        chain.push(' ');
        src = s.source();
    }
    if chain.contains("corrupt message") || chain.contains("invalidcontenttype") || chain.contains("wrong version number") {
        return AuthError::Tls("That port answers plain HTTP, not HTTPS. Try http:// instead, or the server's HTTPS address.".into());
    }
    if chain.contains("certificate") || chain.contains("tls") || chain.contains("handshake") || chain.contains("unknownissuer") || chain.contains("invalid peer") {
        let hint = if chain.contains("unknownissuer") || chain.contains("unknown issuer") || chain.contains("self-signed") || chain.contains("self signed") {
            "The certificate isn't trusted (self-signed?). Use Tailscale's HTTPS name or a trusted certificate."
        } else if chain.contains("notvalidforname") || chain.contains("not valid for") {
            "The certificate doesn't match this address."
        } else if chain.contains("expired") {
            "The server's certificate has expired."
        } else {
            "Check that the server speaks HTTPS on this port, or use http:// on a trusted network."
        };
        return AuthError::Tls(hint.into());
    }
    if chain.contains("dns") || chain.contains("failed to lookup") || chain.contains("nodename nor servname") || chain.contains("name or service not known") {
        return AuthError::Unreachable("The name doesn't resolve. Check the spelling, or that Tailscale is connected.".into());
    }
    if chain.contains("refused") {
        return AuthError::Unreachable("Nothing is listening there. Is `hermes dashboard` running, and on this port?".into());
    }
    AuthError::Unreachable("Check the address and your network or Tailscale.".into())
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(20))
        // Sign-in must see the 302 from /auth/native/authorize itself.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

fn join(base: &Url, path: &str) -> String {
    format!("{}{}", base.as_str().trim_end_matches('/'), path)
}

/// What a server said about itself.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub base: Url,
    pub version: String,
    pub auth_required: bool,
    /// `/api/status.auth_flows` contains `native_pkce`.
    pub native: bool,
    pub providers: Vec<Provider>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub name: String,
    pub display_name: String,
    pub supports_password: bool,
}

impl ServerInfo {
    pub fn password_provider(&self) -> Option<&Provider> {
        self.providers.iter().find(|p| p.supports_password)
    }
    pub fn is_loopback(&self) -> bool {
        is_loopback(&self.base)
    }
}

pub fn is_loopback(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Clean up what the user typed: drop `/login`, queries and fragments; keep proxy prefixes.
fn normalize(mut u: Url) -> Url {
    u.set_query(None);
    u.set_fragment(None);
    let path = u.path().trim_end_matches('/').to_owned();
    let path = path.strip_suffix("/login").unwrap_or(&path).to_owned();
    u.set_path(&path);
    u
}

/// Addresses to try for what the user typed, most likely first.
pub fn candidates(input: &str) -> Result<Vec<Url>, AuthError> {
    let s = input.trim();
    if s.is_empty() {
        return Err(AuthError::Unsupported("Enter the address you open Hermes at.".into()));
    }
    let bad = || AuthError::Unsupported(format!("That doesn't look like an address: {s}"));
    if s.contains("://") {
        let u = Url::parse(s).map_err(|_| bad())?;
        if !matches!(u.scheme(), "http" | "https") {
            return Err(bad());
        }
        return Ok(vec![normalize(u)]);
    }
    let probe = Url::parse(&format!("http://{s}")).map_err(|_| bad())?;
    let host_local = is_loopback(&probe);
    let has_port = probe.port().is_some();
    let mut out = vec![];
    let mut push = |raw: String| {
        if let Ok(u) = Url::parse(&raw) {
            let u = normalize(u);
            if !out.contains(&u) {
                out.push(u);
            }
        }
    };
    if host_local {
        push(format!("http://{s}"));
        if !has_port {
            for p in LOCAL_PORTS {
                push(format!("http://{}:{p}{}", probe.host_str().unwrap_or("127.0.0.1"), probe.path()));
            }
        }
    } else {
        // Tailscale Serve / reverse proxies are HTTPS; a bare LAN host is usually :9119.
        push(format!("https://{s}"));
        if !has_port {
            push(format!("http://{}:9119{}", probe.host_str().unwrap_or_default(), probe.path()));
        }
        push(format!("http://{s}"));
    }
    Ok(out)
}

/// Find the Hermes dashboard behind `input` and describe how to sign in.
pub async fn probe(input: &str) -> Result<ServerInfo, AuthError> {
    let client = http();
    let mut first_err: Option<AuthError> = None;
    let mut answered = false;
    for base in candidates(input)? {
        match probe_one(&client, &base).await {
            Ok(info) => return Ok(info),
            Err(AuthError::NotHermes) => answered = true,
            Err(e) => {
                // Prefer the error of the first (most likely) candidate, but a TLS failure on the
                // https guess is less useful than "nothing there" when http works neither.
                if first_err.is_none() || matches!(first_err, Some(AuthError::Tls(_))) && !matches!(e, AuthError::Tls(_)) && !input.contains("https://") {
                    first_err = Some(e);
                }
            }
        }
    }
    if answered {
        return Err(AuthError::NotHermes);
    }
    Err(first_err.unwrap_or(AuthError::NotHermes))
}

async fn probe_one(client: &reqwest::Client, base: &Url) -> Result<ServerInfo, AuthError> {
    let r = client.get(join(base, "/api/status")).timeout(Duration::from_secs(6)).send().await.map_err(|e| classify(&e))?;
    if !r.status().is_success() {
        return Err(AuthError::NotHermes);
    }
    let status: Value = r.json().await.map_err(|_| AuthError::NotHermes)?;
    if status.get("version").is_none() {
        return Err(AuthError::NotHermes);
    }
    let auth_required = status.get("auth_required").and_then(Value::as_bool).unwrap_or(false);
    let native = status.get("auth_flows").and_then(Value::as_array).is_some_and(|a| a.iter().any(|f| f == "native_pkce"));
    let mut providers = vec![];
    if auth_required {
        if let Ok(r) = client.get(join(base, "/api/auth/providers")).send().await {
            if let Ok(v) = r.json::<Value>().await {
                for p in v.get("providers").and_then(Value::as_array).into_iter().flatten() {
                    let name = p.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                    if !name.is_empty() {
                        providers.push(Provider {
                            display_name: p.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(&name).to_owned(),
                            supports_password: p.get("supports_password").and_then(Value::as_bool).unwrap_or(false),
                            name,
                        });
                    }
                }
            }
        }
        if providers.is_empty() {
            // Older builds only name them in /api/status.
            for n in status.get("auth_providers").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                providers.push(Provider { name: n.into(), display_name: n.into(), supports_password: n == "basic" });
            }
        }
    }
    Ok(ServerInfo { base: base.clone(), version: status.get("version").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())).unwrap_or_default(), auth_required, native, providers })
}

/// Hermes dashboards on this machine (default ports), fastest first.
pub async fn detect_local() -> Vec<ServerInfo> {
    let client = http();
    let futs = LOCAL_PORTS.iter().map(|p| {
        let client = client.clone();
        let base = Url::parse(&format!("http://127.0.0.1:{p}")).unwrap();
        async move { probe_one(&client, &base).await.ok() }
    });
    futures_util::future::join_all(futs).await.into_iter().flatten().collect()
}

struct Pkce {
    verifier: String,
    challenge: String,
    state: String,
}

impl Pkce {
    fn new() -> Pkce {
        let rand = || {
            let mut b = Vec::with_capacity(32);
            b.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
            b.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
        };
        let verifier = rand();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce { verifier, challenge, state: rand() }
    }
}

/// `name=value` pairs from `Set-Cookie`, to send back within this one sign-in.
fn cookies_of(r: &reqwest::Response) -> String {
    r.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.split(';').next())
        .map(str::trim)
        .filter(|c| c.contains('='))
        .collect::<Vec<_>>()
        .join("; ")
}

async fn detail(r: reqwest::Response) -> String {
    let body = r.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&body).ok().and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned)).unwrap_or_default()
}

/// Username + password sign-in, start to finish, without a browser (module docs).
pub async fn login_password(base: &Url, provider: &str, username: &str, password: &str) -> Result<Credential, AuthError> {
    let client = http();
    let pkce = Pkce::new();
    let mut authorize = Url::parse(&join(base, "/auth/native/authorize")).map_err(|_| AuthError::NotHermes)?;
    authorize
        .query_pairs_mut()
        .append_pair("provider", provider)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("redirect_uri", NATIVE_REDIRECT)
        .append_pair("state", &pkce.state);
    let started = client.get(authorize.as_str()).send().await.map_err(|e| classify(&e))?;
    let st = started.status();
    if st.as_u16() == 404 {
        return Err(AuthError::Unsupported("This Hermes is too old for app sign-in. Update Hermes, or use a session token under Advanced.".into()));
    }
    if !st.is_redirection() {
        let d = detail(started).await;
        return Err(AuthError::Server(st.as_u16(), d));
    }
    let cookie = cookies_of(&started);
    let mut req = client.post(join(base, "/auth/password-login")).json(&json!({"provider": provider, "username": username, "password": password, "next": ""}));
    if !cookie.is_empty() {
        req = req.header(reqwest::header::COOKIE, cookie);
    }
    let login = req.send().await.map_err(|e| classify(&e))?;
    match login.status().as_u16() {
        200 => {}
        401 => return Err(AuthError::InvalidCredentials),
        429 => return Err(AuthError::RateLimited),
        s => {
            let d = detail(login).await;
            return Err(AuthError::Server(s, d));
        }
    }
    let body: Value = login.json().await.map_err(|_| AuthError::Server(200, "unexpected sign-in reply".into()))?;
    let next = body.get("next").and_then(Value::as_str).unwrap_or_default();
    let landing = Url::parse(next).ok().filter(|_| next.starts_with(NATIVE_REDIRECT)).ok_or_else(|| AuthError::Unsupported("The server didn't return an app sign-in code. Update Hermes, or use a session token under Advanced.".into()))?;
    let q = |k: &str| landing.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned());
    if q("state").as_deref() != Some(pkce.state.as_str()) {
        return Err(AuthError::Server(400, "sign-in state mismatch, try again".into()));
    }
    let code = q("code").ok_or_else(|| AuthError::Server(400, "no authorization code".into()))?;
    let r = client.post(join(base, "/auth/native/token")).json(&json!({"code": code, "code_verifier": pkce.verifier})).send().await.map_err(|e| classify(&e))?;
    if !r.status().is_success() {
        let s = r.status().as_u16();
        return Err(AuthError::Server(s, detail(r).await));
    }
    native_from(&r.json::<Value>().await.unwrap_or(Value::Null))
}

fn native_from(v: &Value) -> Result<Credential, AuthError> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    let access_token = s("access_token");
    if access_token.is_empty() {
        return Err(AuthError::Server(200, "no access token in the reply".into()));
    }
    Ok(Credential::Native {
        access_token,
        refresh_token: Some(s("refresh_token")).filter(|r| !r.is_empty()),
        expires_at: v.get("expires_at").and_then(Value::as_f64).unwrap_or(0.0) as i64,
        provider: s("provider"),
        user_id: s("user_id"),
    })
}

/// The per-process token a loopback dashboard injects into its page.
pub async fn loopback_token(base: &Url) -> Result<String, AuthError> {
    let page = http().get(join(base, "/")).send().await.map_err(|e| classify(&e))?.text().await.map_err(|e| classify(&e))?;
    let rest = page.split_once("__HERMES_SESSION_TOKEN__").map(|(_, r)| r).ok_or(AuthError::SignInRequired)?;
    let rest = rest.trim_start().strip_prefix('=').ok_or(AuthError::SignInRequired)?.trim_start();
    let rest = rest.strip_prefix('"').ok_or(AuthError::SignInRequired)?;
    let token = rest.split('"').next().unwrap_or_default();
    if token.is_empty() {
        return Err(AuthError::SignInRequired);
    }
    Ok(token.to_owned())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// A connection's live credentials: refreshes once for any number of callers, persists rotated
/// tokens to the keychain, and re-authenticates silently when it can.
pub struct Auth {
    conn_id: String,
    pub base: Url,
    http: reqwest::Client,
    stored: Mutex<Stored>,
    /// Loopback dashboards: the token read from the page.
    loopback: Mutex<Option<String>>,
    gate: tokio::sync::Mutex<()>,
    persist: bool,
}

impl Auth {
    pub fn new(conn_id: &str, base: Url, stored: Stored, persist: bool) -> Arc<Auth> {
        Arc::new(Auth { conn_id: conn_id.to_owned(), base, http: http(), stored: Mutex::new(stored), loopback: Mutex::new(None), gate: tokio::sync::Mutex::new(()), persist })
    }

    pub fn load(conn_id: &str, base: Url) -> Arc<Auth> {
        Self::new(conn_id, base, Stored::load(conn_id), true)
    }

    pub fn stored(&self) -> Stored {
        self.stored.lock().unwrap().clone()
    }

    fn set(&self, s: Stored) {
        if self.persist {
            if let Err(e) = s.save(&self.conn_id) {
                log::warn!("couldn't save renewed sign-in to the keychain: {e}");
            }
        }
        *self.stored.lock().unwrap() = s;
    }

    /// Headers for one REST call, refreshing first when the access token is about to lapse.
    pub async fn headers(&self) -> Result<Vec<(&'static str, String)>, AuthError> {
        let cred = self.stored().cred;
        let cred = match &cred {
            Credential::Native { expires_at, refresh_token: Some(_), access_token, .. } if *expires_at > 0 && *expires_at - now() < REFRESH_MARGIN_SECS => {
                self.renew(access_token).await?;
                self.stored().cred
            }
            _ => cred,
        };
        Ok(match cred {
            Credential::None => vec![],
            Credential::Token { token } => vec![("X-Hermes-Session-Token", token)],
            Credential::Native { access_token, .. } => vec![("Authorization", format!("Bearer {access_token}"))],
            Credential::Loopback => {
                let t = self.loopback_cached().await?;
                vec![("X-Hermes-Session-Token", t)]
            }
        })
    }

    async fn loopback_cached(&self) -> Result<String, AuthError> {
        if let Some(t) = self.loopback.lock().unwrap().clone() {
            return Ok(t);
        }
        let t = loopback_token(&self.base).await?;
        *self.loopback.lock().unwrap() = Some(t.clone());
        Ok(t)
    }

    /// The credential in `headers` was refused: renew it (once, however many callers ask).
    /// `stale` is the header value that failed.
    pub async fn rejected(&self, stale: &str) -> Result<(), AuthError> {
        match self.stored().cred {
            Credential::Loopback => {
                let mut lb = self.loopback.lock().unwrap();
                if lb.as_deref() == Some(stale) {
                    *lb = None;
                }
                Ok(())
            }
            Credential::Native { access_token, .. } if stale.ends_with(&access_token) => self.renew(&access_token).await,
            Credential::Native { .. } => Ok(()),
            _ => Err(AuthError::SignInRequired),
        }
    }

    /// Rotate tokens via `/auth/native/refresh`, or sign in again with the remembered password.
    async fn renew(&self, stale_access: &str) -> Result<(), AuthError> {
        let _g = self.gate.lock().await;
        let st = self.stored();
        let Credential::Native { access_token, refresh_token, provider, .. } = &st.cred else { return Err(AuthError::SignInRequired) };
        if access_token != stale_access {
            return Ok(()); // someone else already renewed
        }
        if let Some(rt) = refresh_token {
            let r = self.http.post(join(&self.base, "/auth/native/refresh")).json(&json!({"refresh_token": rt, "provider": provider})).send().await.map_err(|e| classify(&e))?;
            match r.status().as_u16() {
                200..=299 => {
                    let mut fresh = native_from(&r.json::<Value>().await.unwrap_or(Value::Null))?;
                    if let Credential::Native { refresh_token: new_rt, .. } = &mut fresh {
                        if new_rt.is_none() {
                            *new_rt = Some(rt.clone());
                        }
                    }
                    self.set(Stored { cred: fresh, ..st.clone() });
                    log::info!("renewed the session for {}", self.conn_id);
                    return Ok(());
                }
                400 | 401 => {} // session_expired: fall through to a fresh sign-in
                s => return Err(AuthError::Server(s, detail(r).await)),
            }
        }
        match (&st.provider, &st.username, &st.password) {
            (Some(p), Some(u), Some(pw)) => {
                let cred = login_password(&self.base, p, u, pw).await?;
                self.set(Stored { cred, ..st.clone() });
                log::info!("signed in again silently for {}", self.conn_id);
                Ok(())
            }
            _ => Err(AuthError::SignInRequired),
        }
    }

    /// The `/api/ws` URL for the next connect attempt (a fresh single-use ticket each time).
    pub async fn ws_url(&self) -> Result<String, AuthError> {
        let mut u = self.base.clone();
        let _ = u.set_scheme(if u.scheme() == "https" { "wss" } else { "ws" });
        let root = format!("{}/api/ws", u.as_str().trim_end_matches('/'));
        let q = |k: &str, v: &str| format!("{root}?{k}={}", url::form_urlencoded::byte_serialize(v.as_bytes()).collect::<String>());
        Ok(match self.stored().cred {
            Credential::None => root,
            Credential::Token { token } => q("token", &token),
            Credential::Loopback => {
                // Re-read every time: the token rotates whenever the dashboard restarts.
                *self.loopback.lock().unwrap() = None;
                q("token", &self.loopback_cached().await?)
            }
            Credential::Native { .. } => {
                for attempt in 0..2 {
                    let h = self.headers().await?;
                    let mut req = self.http.post(join(&self.base, "/api/auth/ws-ticket"));
                    for (k, v) in &h {
                        req = req.header(*k, v);
                    }
                    let r = req.send().await.map_err(|e| classify(&e))?;
                    match r.status().as_u16() {
                        200..=299 => {
                            let v: Value = r.json().await.unwrap_or(Value::Null);
                            let t = v.get("ticket").and_then(Value::as_str).unwrap_or_default();
                            if t.is_empty() {
                                return Err(AuthError::Server(200, "no WebSocket ticket in the reply".into()));
                            }
                            return Ok(q("ticket", t));
                        }
                        401 if attempt == 0 => {
                            let stale = h.first().map(|(_, v)| v.clone()).unwrap_or_default();
                            self.rejected(&stale).await?;
                        }
                        401 => return Err(AuthError::SignInRequired),
                        s => return Err(AuthError::Server(s, detail(r).await)),
                    }
                }
                return Err(AuthError::SignInRequired);
            }
        })
    }

    /// Who is signed in, for Settings → Account.
    pub fn identity(&self) -> String {
        let st = self.stored();
        match &st.cred {
            Credential::Native { user_id, .. } => st.username.clone().filter(|u| !u.is_empty()).unwrap_or_else(|| user_id.clone()),
            Credential::Loopback => "This computer".into(),
            Credential::Token { .. } => "Session token".into(),
            Credential::None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_urls() {
        let c = candidates("homelab.tail1234.ts.net").unwrap();
        assert_eq!(c[0].as_str(), "https://homelab.tail1234.ts.net/");
        assert_eq!(c[1].as_str(), "http://homelab.tail1234.ts.net:9119/");
        let c = candidates("localhost").unwrap();
        assert!(c.iter().any(|u| u.as_str() == "http://localhost:9119/"));
        let c = candidates("https://x.example/hermes/login?next=/").unwrap();
        assert_eq!(c, vec![Url::parse("https://x.example/hermes").unwrap()]);
        assert!(candidates("").is_err());
        assert!(candidates("ftp://x").is_err());
    }

    #[test]
    fn legacy_keychain_entries_are_tokens() {
        let s: Stored = serde_json::from_str(r#"{"cred":{"kind":"native","access_token":"a","expires_at":5}}"#).unwrap();
        assert!(matches!(s.cred, Credential::Native { .. }));
        assert_eq!(serde_json::to_string(&Credential::Loopback).unwrap(), r#"{"kind":"loopback"}"#);
    }

    #[test]
    fn pkce_is_s256() {
        let p = Pkce::new();
        assert!(p.verifier.len() >= 43);
        assert_eq!(p.challenge, base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(p.verifier.as_bytes())));
    }
}
