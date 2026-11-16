//! The service: owns connections and turns UI commands into official Hermes calls, and gateway
//! traffic into UI events. It runs on a small tokio runtime off the UI thread. The UI talks to it
//! only through [`Cmd`] and [`Ev`] channels.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::auth::{self, Auth, Credential, ServerInfo, Stored};
use crate::conn::{ConnKind, Connection, Endpoint, GapAction};
use crate::gateway::{Gateway, Inbound, LinkState, RpcError, METHOD_NOT_FOUND};
use crate::rest::Rest;
use crate::roster::{self, Bot, BotKey, BOT_CHAT_TITLE, UI_META_KEY};
use crate::store::Store;
use crate::transcript::{self, Activity, ChatMessage, Role, ToolState};

/// Image upload limit (`tui_gateway/prompt_attachments.py`).
pub const MAX_IMAGE_BYTES: u64 = 25 << 20;
/// General attachment cap (`hermes_cli/web_server_boundary.py`).
pub const MAX_FILE_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone)]
pub struct LocalFile {
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub size: u64,
}

impl LocalFile {
    pub fn from_path(path: PathBuf) -> Option<LocalFile> {
        let meta = std::fs::metadata(&path).ok()?;
        if !meta.is_file() {
            return None;
        }
        let name = path.file_name()?.to_string_lossy().into_owned();
        let mime = mime_guess::from_path(&path).first_or_octet_stream().essence_str().to_owned();
        Some(LocalFile { path, name, mime, size: meta.len() })
    }
}

#[derive(Debug, Clone)]
pub enum Cmd {
    /// Add or replace a connection (token read from the keychain).
    Connect(Connection),
    Disconnect(String),
    /// Focus a bot: open its canonical Bot Chat and load history.
    OpenBot(BotKey),
    /// `refresh` applies the long-break policy before the prompt (see `conn::GapAction`).
    Send { key: BotKey, text: String, files: Vec<LocalFile>, refresh: Option<GapAction> },
    Stop(BotKey),
    MarkRead(BotKey),
    /// Download an agent-produced file; `open` opens it with the OS when done.
    /// `save_as`: the exact path picked in a save dialog (wins over `dest`).
    Download { key: BotKey, remote: String, dest: Option<PathBuf>, save_as: Option<PathBuf>, open: bool },
    /// Fetch an image for inline display (cached on disk).
    FetchMedia { key: BotKey, remote: String },
    Approve { conn: String, request_id: String, choice: String },
    /// Any official gateway method, answered with `Ev::Rpc { tag }`. Used by the Apps page, bot
    /// settings and the thread list, whose wire shapes live in `apps.rs` / `roster.rs`.
    Rpc { conn: String, method: String, params: Value, tag: String },
    CreateBot { conn: String, name: String, title: String, description: String },
    SetMeta { key: BotKey, patch: Value },
    RefreshRoster,
    /// `GET /api/analytics/usage` for one profile (or the default store), answered with `Ev::Rpc`.
    Usage { conn: String, profile: Option<String>, days: u32, tag: String },
    /// Health/auth probe of what the user typed (`/api/status` + providers) → `Ev::Probed`.
    Probe { input: String, tag: String },
    /// Look for Hermes dashboards on this machine's default ports → `Ev::LocalServers`.
    DetectLocal,
    /// Sign in and save the credential to the keychain under `conn_id` → `Ev::SignedIn`.
    SignIn { conn_id: String, base: String, method: SignInMethod, tag: String },
    /// Forget the credential (keychain) and close the connection.
    SignOut { conn_id: String },
}

/// How to sign in (see `auth.rs`).
#[derive(Clone)]
pub enum SignInMethod {
    Password { provider: String, username: String, password: String, remember: bool },
    Token { token: String },
    /// A loopback dashboard without accounts.
    Loopback,
}

impl std::fmt::Debug for SignInMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print secrets.
        match self {
            SignInMethod::Password { provider, username, .. } => write!(f, "Password({provider}, {username}, ***)"),
            SignInMethod::Token { .. } => write!(f, "Token(***)"),
            SignInMethod::Loopback => write!(f, "Loopback"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Ev {
    Link { conn: String, state: LinkState },
    Roster { conn: String, bots: Vec<Bot>, unread: HashSet<BotKey> },
    History { key: BotKey, messages: Vec<ChatMessage>, cached: bool },
    /// A user-side message the server started a turn with (e.g. a bot-to-bot DM arriving).
    Incoming { key: BotKey, message: ChatMessage },
    TurnStart { key: BotKey },
    Delta { key: BotKey, text: String },
    Activity { key: BotKey, activity: Activity },
    TurnEnd { key: BotKey, text: String, error: Option<String> },
    Status { key: BotKey, text: String },
    Approval { conn: String, key: Option<BotKey>, request_id: String, command: String, description: String, choices: Vec<String> },
    ApprovalGone { request_id: String },
    MediaReady { remote: String, local: PathBuf },
    Downloaded { key: BotKey, path: PathBuf, open: bool },
    /// Something to surface as an OS notification (a bot replied while not focused).
    Notify { key: BotKey, title: String, body: String },
    Error { key: Option<BotKey>, message: String },
    Info { message: String },
    /// Output of a slash command run through `slash.exec` (or why it could not run).
    Slash { key: BotKey, command: String, output: String, error: bool },
    Rpc { tag: String, conn: String, result: Result<Value, String> },
    Probed { tag: String, result: Result<ServerInfo, String> },
    LocalServers { servers: Vec<ServerInfo> },
    /// `Ok(identity)` ("admin", "This computer"…).
    SignedIn { tag: String, conn_id: String, result: Result<String, String> },
}

struct Live {
    stored: String,
    runtime: String,
}

struct ConnState {
    conn: Connection,
    gw: Gateway,
    rest: Rest,
    ready: Value,
    roster: Vec<Bot>,
    live: HashMap<String, Live>,
    by_runtime: HashMap<String, BotKey>,
    last_roster_json: String,
    turn_text: HashMap<String, String>,
    /// `turn_id` of the turn streaming in each slot (a re-sent `message.start` keeps its text).
    turn_ids: HashMap<String, String>,
    /// Bots whose turn was still running when the socket dropped: resumed on `gateway.ready`.
    resume: HashSet<BotKey>,
    /// `session.resume`'s `inflight` turn, replayed after history loads (docs: *Rebuilding the
    /// in-flight turn on reconnect*).
    inflight: HashMap<String, Value>,
}

struct State {
    conns: HashMap<String, ConnState>,
    focused: Option<BotKey>,
}

#[derive(Clone)]
struct Svc {
    st: Arc<Mutex<State>>,
    store: Arc<Store>,
    ev: async_channel::Sender<Ev>,
    cache_dir: PathBuf,
    /// One `ensure_live` at a time per bot slot, so concurrent callers can't each create or
    /// resume a session.
    opening: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

/// Handle the UI keeps. Dropping it shuts the runtime down.
pub struct Service {
    pub cmd: async_channel::Sender<Cmd>,
    pub ev: async_channel::Receiver<Ev>,
    pub store: Arc<Store>,
    _rt: tokio::runtime::Runtime,
}

impl Service {
    pub fn start(store: Arc<Store>, cache_dir: PathBuf) -> anyhow::Result<Service> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("hermes-io").enable_all().build()?;
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<Cmd>();
        let (ev_tx, ev_rx) = async_channel::unbounded::<Ev>();
        let svc = Svc { st: Arc::new(Mutex::new(State { conns: HashMap::new(), focused: None })), store: store.clone(), ev: ev_tx, cache_dir, opening: Default::default() };
        rt.spawn(svc.clone().run(cmd_rx));
        Ok(Service { cmd: cmd_tx, ev: ev_rx, store, _rt: rt })
    }

    pub fn send(&self, c: Cmd) {
        let _ = self.cmd.try_send(c);
    }
}

fn rpc_msg(e: &RpcError) -> String {
    e.message.clone()
}

impl Svc {
    fn emit(&self, e: Ev) {
        let _ = self.ev.try_send(e);
    }

    async fn run(self, cmds: async_channel::Receiver<Cmd>) {
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                c = cmds.recv() => match c {
                    Ok(c) => self.handle(c),
                    Err(_) => break,
                },
                _ = tick.tick() => {
                    let ids: Vec<String> = self.st.lock().unwrap().conns.keys().cloned().collect();
                    for id in ids {
                        tokio::spawn(self.clone().refresh_roster(id));
                    }
                }
            }
        }
    }

    fn handle(&self, c: Cmd) {
        let s = self.clone();
        match c {
            Cmd::Connect(conn) => s.connect(conn),
            Cmd::Probe { input, tag } => {
                tokio::spawn(async move {
                    let result = auth::probe(&input).await.map_err(|e| e.to_string());
                    s.emit(Ev::Probed { tag, result });
                });
            }
            Cmd::DetectLocal => {
                tokio::spawn(async move {
                    let servers = auth::detect_local().await;
                    s.emit(Ev::LocalServers { servers });
                });
            }
            Cmd::SignIn { conn_id, base, method, tag } => {
                tokio::spawn(async move {
                    let result = sign_in(&conn_id, &base, method).await;
                    s.emit(Ev::SignedIn { tag, conn_id, result });
                });
            }
            Cmd::SignOut { conn_id } => {
                if let Some(c) = s.st.lock().unwrap().conns.remove(&conn_id) {
                    c.gw.stop();
                }
                // Native tokens are held only here: dropping them signs this app out (docs: Desktop
                // Native Sign-In → "Sign out"). `/auth/logout` revokes cookie sessions only.
                crate::conn::Secrets::delete(&conn_id);
            }
            Cmd::Disconnect(id) => {
                if let Some(c) = s.st.lock().unwrap().conns.remove(&id) {
                    c.gw.stop();
                }
            }
            Cmd::OpenBot(key) => {
                s.st.lock().unwrap().focused = Some(key.clone());
                tokio::spawn(async move { s.open_bot(key, true).await });
            }
            Cmd::Send { key, text, files, refresh } => {
                tokio::spawn(async move {
                    if let Err(m) = s.send(key.clone(), text, files, refresh).await {
                        s.emit(Ev::TurnEnd { key: key.clone(), text: String::new(), error: Some(m) });
                    }
                });
            }
            Cmd::Stop(key) => {
                tokio::spawn(async move {
                    if let Some((gw, sid)) = s.live_of(&key) {
                        let _ = gw.call("session.interrupt", json!({"session_id": sid, "profile": key.profile})).await;
                    }
                });
            }
            Cmd::MarkRead(key) => {
                let la = s.bot(&key).map(|b| b.last_active()).unwrap_or(0.0);
                let _ = s.store.set_seen(&key, la.max(now()));
                s.st.lock().unwrap().focused = Some(key);
            }
            Cmd::Download { key, remote, dest, save_as, open } => {
                tokio::spawn(async move {
                    let Some(rest) = s.rest(&key.conn) else { return };
                    let dir = dest.unwrap_or_else(|| directories::UserDirs::new().and_then(|u| u.download_dir().map(|p| p.to_path_buf())).unwrap_or_else(|| s.cache_dir.join("downloads")));
                    match rest.download(&remote, &key.profile, &dir, save_as.as_deref(), |_, _| {}).await {
                        Ok(path) => s.emit(Ev::Downloaded { key: key.clone(), path, open }),
                        Err(e) => s.emit(Ev::Error { key: Some(key), message: e.to_string() }),
                    }
                });
            }
            Cmd::FetchMedia { key, remote } => {
                tokio::spawn(async move {
                    let Some(rest) = s.rest(&key.conn) else { return };
                    // Keyed by server, profile and path; re-fetched after an hour so an image the
                    // agent rewrote in place shows up. `/api/media` caps images at 25 MB.
                    let file = media_cache_path(&s.cache_dir, &key.conn, &key.profile, &remote);
                    if std::fs::metadata(&file).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age < MEDIA_TTL) {
                        s.emit(Ev::MediaReady { remote, local: file });
                        return;
                    }
                    let fetched = async {
                        let url = rest.media_data_url(&remote, &key.profile).await?;
                        let b64 = url.split_once(',').map(|(_, b)| b).ok_or_else(|| anyhow::anyhow!("bad data url"))?;
                        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
                        if let Some(dir) = file.parent() {
                            tokio::fs::create_dir_all(dir).await?;
                        }
                        let part = crate::files::part_path(&file);
                        tokio::fs::write(&part, &bytes).await?;
                        tokio::fs::rename(&part, &file).await?;
                        anyhow::Ok(())
                    };
                    match fetched.await {
                        Ok(()) => s.emit(Ev::MediaReady { remote, local: file }),
                        Err(e) => log::warn!("media {remote}: {e}"),
                    }
                });
            }
            Cmd::Approve { conn, request_id, choice } => {
                if let Some(gw) = s.gw(&conn) {
                    gw.respond(&request_id, json!({"choice": choice}));
                }
            }
            Cmd::Rpc { conn, method, params, tag } => {
                tokio::spawn(async move {
                    let result = match s.gw(&conn) {
                        None => Err("Not connected.".to_owned()),
                        Some(gw) => gw.call_timeout(&method, params, Duration::from_secs(60)).await.map_err(|e| {
                            if e.code == METHOD_NOT_FOUND {
                                format!("This Hermes doesn't offer {method} on this connection.")
                            } else {
                                e.message
                            }
                        }),
                    };
                    if method.starts_with("profiles.") && result.is_ok() {
                        s.clone().refresh_roster(conn.clone()).await;
                    }
                    s.emit(Ev::Rpc { tag, conn, result });
                });
            }
            Cmd::CreateBot { conn, name, title, description } => {
                tokio::spawn(async move { s.create_bot(conn, name, title, description).await });
            }
            Cmd::SetMeta { key, patch } => {
                tokio::spawn(async move {
                    if let Err(m) = s.set_meta(&key, patch).await {
                        s.emit(Ev::Error { key: Some(key), message: m });
                    }
                });
            }
            Cmd::Usage { conn, profile, days, tag } => {
                tokio::spawn(async move {
                    let result = match s.rest(&conn) {
                        Some(rest) => rest.usage(profile.as_deref(), days).await.map_err(|e| e.to_string()),
                        None => Err("Not connected.".into()),
                    };
                    s.emit(Ev::Rpc { tag, conn, result });
                });
            }
            Cmd::RefreshRoster => {
                let ids: Vec<String> = s.st.lock().unwrap().conns.keys().cloned().collect();
                for id in ids {
                    tokio::spawn(s.clone().refresh_roster(id));
                }
            }
        }
    }

    fn gw(&self, conn: &str) -> Option<Gateway> {
        self.st.lock().unwrap().conns.get(conn).map(|c| c.gw.clone())
    }
    fn rest(&self, conn: &str) -> Option<Rest> {
        self.st.lock().unwrap().conns.get(conn).map(|c| c.rest.clone())
    }
    fn bot(&self, key: &BotKey) -> Option<Bot> {
        self.st.lock().unwrap().conns.get(&key.conn)?.roster.iter().find(|b| b.key.same_bot(key)).cloned()
    }
    fn live_of(&self, key: &BotKey) -> Option<(Gateway, String)> {
        let st = self.st.lock().unwrap();
        let c = st.conns.get(&key.conn)?;
        Some((c.gw.clone(), c.live.get(&key.slot())?.runtime.clone()))
    }
    fn all_bots(&self) -> Vec<Bot> {
        self.st.lock().unwrap().conns.values().flat_map(|c| c.roster.clone()).collect()
    }
    fn session_authority(&self, conn: &str) -> bool {
        self.st.lock().unwrap().conns.get(conn).and_then(|c| c.ready.get("session_authority").and_then(Value::as_bool)).unwrap_or(false)
    }

    fn connect(&self, conn: Connection) {
        if conn.kind != ConnKind::Gateway {
            self.emit(Ev::Error { key: None, message: "API-server-only connections are not supported yet in this build.".into() });
            return;
        }
        let ep = match Endpoint::new(&conn.base_url, "") {
            Ok(e) => e,
            Err(e) => return self.emit(Ev::Error { key: None, message: e.to_string() }),
        };
        // The dev `env` connection's token comes from the environment and is never saved.
        let auth = Auth::new(&conn.id, ep.base.clone(), Stored::load(&conn.id), conn.id != "env");
        let Ok(rest) = Rest::new(auth.clone()) else { return };
        let (in_tx, in_rx) = async_channel::unbounded();
        let a = auth.clone();
        let source: crate::gateway::UrlSource = Arc::new(move || {
            let a = a.clone();
            Box::pin(async move { a.ws_url().await })
        });
        let gw = Gateway::spawn(source, in_tx);
        // Seed from cache so the roster paints instantly.
        let cached: Vec<Bot> = self.store.load_rosters().into_iter().filter(|b| b.key.conn == conn.id).collect();
        if !cached.is_empty() {
            let unread = self.unread_of(&cached);
            self.emit(Ev::Roster { conn: conn.id.clone(), bots: cached.clone(), unread });
        }
        let id = conn.id.clone();
        if let Some(old) = self.st.lock().unwrap().conns.insert(
            id.clone(),
            ConnState { conn, gw, rest, ready: Value::Null, roster: cached, live: HashMap::new(), by_runtime: HashMap::new(), last_roster_json: String::new(), turn_text: HashMap::new(), turn_ids: HashMap::new(), resume: HashSet::new(), inflight: HashMap::new() },
        ) {
            old.gw.stop();
        }
        let s = self.clone();
        tokio::spawn(async move {
            while let Ok(m) = in_rx.recv().await {
                s.inbound(&id, m);
            }
        });
    }

    fn inbound(&self, conn: &str, m: Inbound) {
        match m {
            Inbound::Link(state) => {
                if !matches!(state, LinkState::Connected) {
                    let mut st = self.st.lock().unwrap();
                    if let Some(c) = st.conns.get_mut(conn) {
                        // Turns that were streaming get resumed on reconnect (their text and any
                        // pending approvals come back from `session.resume`).
                        let running: Vec<BotKey> = c.by_runtime.values().filter(|k| c.turn_text.contains_key(&k.slot())).cloned().collect();
                        c.resume.extend(running);
                        // Runtime session ids die with the socket; reopen lazily.
                        c.live.clear();
                        c.by_runtime.clear();
                        c.turn_text.clear();
                        c.turn_ids.clear();
                    }
                }
                self.emit(Ev::Link { conn: conn.to_owned(), state });
            }
            Inbound::Ready(payload) => {
                let reopen: Vec<BotKey> = {
                    let mut st = self.st.lock().unwrap();
                    let focused = st.focused.clone().filter(|k| k.conn == conn);
                    let mut keys: Vec<BotKey> = match st.conns.get_mut(conn) {
                        Some(c) => {
                            c.ready = payload;
                            c.resume.drain().collect()
                        }
                        None => vec![],
                    };
                    if let Some(f) = focused {
                        if !keys.contains(&f) {
                            keys.push(f);
                        }
                    }
                    keys
                };
                tokio::spawn(self.clone().refresh_roster(conn.to_owned()));
                for k in reopen {
                    let s = self.clone();
                    tokio::spawn(async move { s.open_bot(k, false).await });
                }
            }
            Inbound::Event { kind, session_id, payload } => self.event(conn, &kind, &session_id, payload),
            Inbound::Request { id, method, params } => self.server_request(conn, id, &method, &params),
        }
    }

    /// A server→client request, live or replayed from `open_requests` after a reconnect.
    fn server_request(&self, conn: &str, id: String, method: &str, params: &Value) {
        let key = params.get("session_id").and_then(Value::as_str).and_then(|sid| self.key_for(conn, sid));
        if method == "approval" {
            let mut choices: Vec<String> = params.get("choices").and_then(Value::as_array).map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            if choices.is_empty() {
                choices = vec!["once".into(), "session".into(), "always".into(), "deny".into()];
            }
            self.emit(Ev::Approval {
                conn: conn.to_owned(),
                key,
                request_id: id,
                command: params.get("command").and_then(Value::as_str).unwrap_or_default().to_owned(),
                description: params.get("description").and_then(Value::as_str).unwrap_or_default().to_owned(),
                choices,
            });
        } else if let Some(gw) = self.gw(conn) {
            // Not implemented: the docs ask hosts to fail fast so the agent moves on.
            gw.respond_error(&id, METHOD_NOT_FOUND, &format!("hermes-desktop does not handle {method} yet"));
        }
    }

    fn key_for(&self, conn: &str, runtime_sid: &str) -> Option<BotKey> {
        let st = self.st.lock().unwrap();
        st.conns.get(conn)?.by_runtime.get(runtime_sid).cloned()
    }

    fn event(&self, conn: &str, kind: &str, sid: &str, p: Value) {
        if kind == "request.cancel" {
            if let Some(id) = p.get("id").and_then(Value::as_str) {
                self.emit(Ev::ApprovalGone { request_id: id.to_owned() });
            }
            return;
        }
        if kind == "sessions.changed" || kind == "profiles.changed" {
            tokio::spawn(self.clone().refresh_roster(conn.to_owned()));
            return;
        }
        let Some(key) = self.key_for(conn, sid) else { return };
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        match kind {
            "message.start" => {
                let tid = s("turn_id");
                let repeat = {
                    let mut st = self.st.lock().unwrap();
                    match st.conns.get_mut(conn) {
                        Some(c) => {
                            let same = !tid.is_empty() && c.turn_ids.get(&key.slot()) == Some(&tid) && c.turn_text.contains_key(&key.slot());
                            if !same {
                                c.turn_text.insert(key.slot(), String::new());
                                c.turn_ids.insert(key.slot(), tid.clone());
                            }
                            same
                        }
                        None => false,
                    }
                };
                if repeat {
                    return; // the same turn announced again: keep what streamed so far
                }
                let dk = s("display_kind");
                // A turn the server started from text we did not type (a bot DM arriving, a
                // routine delivery): show the prompt.
                let shown = p.get("display_text").or_else(|| p.get("text")).and_then(Value::as_str).unwrap_or_default();
                if dk != "hidden" && !shown.is_empty() {
                    let role = match roster::parse_agent_notice(shown) {
                        Some(n) => Role::Agent(n),
                        None if dk.is_empty() => Role::User,
                        None => Role::System,
                    };
                    self.emit(Ev::Incoming { key: key.clone(), message: ChatMessage::new(format!("in-{}", uuid::Uuid::new_v4()), role, shown, Some(now())) });
                }
                self.emit(Ev::TurnStart { key });
            }
            "message.delta" => {
                let t = s("text");
                if !t.is_empty() {
                    self.append_turn(conn, &key, &t);
                    self.emit(Ev::Delta { key, text: t });
                }
            }
            "message.interim" => {
                let t = s("text");
                if !p.get("already_streamed").and_then(Value::as_bool).unwrap_or(false) && !t.is_empty() {
                    let t = format!("{t}\n\n");
                    self.append_turn(conn, &key, &t);
                    self.emit(Ev::Delta { key, text: t });
                }
            }
            "tool.start" | "tool.complete" => {
                let name = s("name");
                let label = p.get("labels").and_then(Value::as_array).and_then(|l| l.first()).and_then(|l| l.get("text")).and_then(Value::as_str).map(str::to_owned);
                let detail = ["preview", "context", "args_text"].iter().find_map(|k| p.get(*k).and_then(Value::as_str)).map(|d| roster::one_line(d, 140));
                let failed = p.get("result").and_then(|r| r.get("error")).is_some_and(|e| !e.is_null() && e != &Value::Bool(false) && e != "");
                self.emit(Ev::Activity {
                    key,
                    activity: Activity {
                        id: s("tool_id"),
                        is_subagent: false,
                        title: transcript::tool_title(&name, p.get("args")).or(label).unwrap_or_else(|| transcript::humanize(&name)),
                        name,
                        detail,
                        summary: p.get("summary").and_then(Value::as_str).map(|t| roster::one_line(t, 160)),
                        state: if kind == "tool.start" { ToolState::Running } else if failed { ToolState::Failed } else { ToolState::Done },
                        duration_s: p.get("duration_s").and_then(Value::as_f64),
                    },
                });
            }
            k if k.starts_with("subagent.") => {
                let id = p.get("subagent_id").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| format!("sub-{}", p.get("task_index").and_then(Value::as_i64).unwrap_or(0)));
                let state = match k {
                    "subagent.complete" => {
                        if s("status") == "failed" || s("status") == "error" { ToolState::Failed } else { ToolState::Done }
                    }
                    _ => ToolState::Running,
                };
                let goal = s("goal");
                let detail = match k {
                    "subagent.tool" => Some(format!("using {}", s("tool_name"))),
                    "subagent.progress" | "subagent.thinking" => Some(roster::one_line(&s("text"), 140)).filter(|t| !t.is_empty()),
                    _ => None,
                };
                self.emit(Ev::Activity {
                    key,
                    activity: Activity {
                        id,
                        is_subagent: true,
                        name: "subagent".into(),
                        title: if goal.is_empty() { "Subagent".into() } else { roster::one_line(&goal, 100) },
                        detail,
                        summary: p.get("summary").and_then(Value::as_str).map(|t| roster::one_line(t, 200)),
                        state,
                        duration_s: None,
                    },
                });
            }
            "status.update" => {
                let t = s("text");
                if !t.is_empty() {
                    self.emit(Ev::Status { key, text: t });
                }
            }
            "message.complete" => {
                let streamed = {
                    let mut st = self.st.lock().unwrap();
                    st.conns.get_mut(conn).and_then(|c| c.turn_text.remove(&key.slot())).unwrap_or_default()
                };
                let mut text = s("text");
                if text.is_empty() {
                    text = streamed;
                }
                let status = s("status");
                let error = p.get("error").and_then(Value::as_str).map(str::to_owned).filter(|e| !e.is_empty()).or_else(|| (status == "error").then(|| "The turn failed.".to_owned()));
                let focused = self.st.lock().unwrap().focused.clone();
                // The app decides whether to alert (it knows if the window has focus).
                if !transcript::is_silent(&text) && !text.is_empty() {
                    let title = self.bot(&key).map(|b| b.title()).unwrap_or_else(|| key.profile.clone());
                    self.emit(Ev::Notify { key: key.clone(), title, body: roster::one_line(&text, 140) });
                }
                if focused.as_ref() == Some(&key) {
                    let _ = self.store.set_seen(&key, now() + 1.0);
                }
                self.emit(Ev::TurnEnd { key, text, error });
                tokio::spawn(self.clone().refresh_roster(conn.to_owned()));
            }
            "error" => self.emit(Ev::Error { key: Some(key), message: s("message") }),
            _ => {}
        }
    }

    fn append_turn(&self, conn: &str, key: &BotKey, t: &str) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.conns.get_mut(conn) {
            c.turn_text.entry(key.slot()).or_default().push_str(t);
        }
    }

    fn unread_of(&self, bots: &[Bot]) -> HashSet<BotKey> {
        let focused = self.st.lock().ok().and_then(|s| s.focused.clone());
        let mut out = HashSet::new();
        for b in bots {
            let la = b.last_active();
            match self.store.seen(&b.key) {
                // First sight: everything so far counts as seen.
                None => {
                    let _ = self.store.set_seen(&b.key, la);
                }
                Some(seen) if la > seen + 0.5 && focused.as_ref() != Some(&b.key) => {
                    out.insert(b.key.clone());
                }
                _ => {}
            }
        }
        out
    }

    async fn refresh_roster(self, conn: String) {
        let Some(gw) = self.gw(&conn) else { return };
        if gw.link() != LinkState::Connected {
            return;
        }
        let conn_name = self.st.lock().unwrap().conns.get(&conn).map(|c| c.conn.name.clone()).unwrap_or_default();
        let r = match gw.call_timeout("profiles.list", json!({}), Duration::from_secs(20)).await {
            Ok(r) => r,
            Err(e) => {
                if e.code == METHOD_NOT_FOUND {
                    self.emit(Ev::Error { key: None, message: "This Hermes doesn't serve profiles.list (update Hermes for Bot Mode).".into() });
                }
                return;
            }
        };
        let rows = r.get("profiles").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut bots: Vec<Bot> = rows.iter().filter_map(|row| Bot::from_row(&conn, &conn_name, row)).collect();
        roster::sort_roster(&mut bots);
        let unread = self.unread_of(&bots);
        let js = serde_json::to_string(&bots).unwrap_or_default() + &format!("{unread:?}");
        let changed = {
            let mut st = self.st.lock().unwrap();
            match st.conns.get_mut(&conn) {
                Some(c) => {
                    c.roster = bots.clone();
                    let ch = c.last_roster_json != js;
                    c.last_roster_json = js;
                    ch
                }
                None => false,
            }
        };
        if changed {
            let _ = self.store.save_roster(&conn, &bots);
            self.emit(Ev::Roster { conn, bots, unread });
        }
    }

    /// Find (or create) the canonical Bot Chat and bind it to this socket.
    async fn ensure_live(&self, key: &BotKey) -> Result<(Gateway, String, String), String> {
        let bound = || {
            let (gw, rt) = self.live_of(key)?;
            let stored = self.st.lock().unwrap().conns.get(&key.conn).and_then(|c| c.live.get(&key.slot()).map(|l| l.stored.clone())).unwrap_or_default();
            Some((gw, rt, stored))
        };
        if let Some(v) = bound() {
            return Ok(v);
        }
        let gate = self.opening.lock().unwrap().entry(format!("{}\u{0}{}", key.conn, key.slot())).or_default().clone();
        let _g = gate.lock().await;
        if let Some(v) = bound() {
            return Ok(v); // another caller bound it while we waited
        }
        let gw = self.gw(&key.conn).ok_or("Not connected.")?;
        let p = &key.profile;
        // A thread is an existing stored session: resume it as is.
        // 1. canonical id from the roster, else look it up by title.
        let mut stored = match &key.thread {
            Some(t) => Some(t.clone()),
            None => self.bot(key).and_then(|b| b.canonical.map(|c| c.id)).filter(|s| !s.is_empty()),
        };
        if stored.is_none() {
            if let Ok(r) = gw.call("session.list", json!({"profile": p, "title": BOT_CHAT_TITLE, "limit": 20, "include_hidden": true})).await {
                let rows = r.get("sessions").or_else(|| r.get("rows")).or(Some(&r)).and_then(Value::as_array).cloned().unwrap_or_default();
                stored = rows
                    .iter()
                    .filter(|row| {
                        let t = row.get("root_title").and_then(Value::as_str).filter(|s| !s.is_empty()).or_else(|| row.get("title").and_then(Value::as_str));
                        t == Some(BOT_CHAT_TITLE)
                    })
                    .max_by(|a, b| {
                        let la = a.get("last_active_at").or_else(|| a.get("started_at")).and_then(Value::as_f64).unwrap_or(0.0);
                        let lb = b.get("last_active_at").or_else(|| b.get("started_at")).and_then(Value::as_f64).unwrap_or(0.0);
                        la.partial_cmp(&lb).unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .and_then(|row| row.get("resolved_id").or_else(|| row.get("session_id")).or_else(|| row.get("id")).and_then(Value::as_str).map(str::to_owned));
            }
        }
        let (runtime, stored) = match stored {
            Some(stored) => {
                let r = gw
                    .call("session.resume", json!({"session_id": stored, "cols": 96, "source": "desktop", "defer_history": true, "omit_messages": true, "profile": p}))
                    .await
                    .map_err(|e| rpc_msg(&e))?;
                let rt = r.get("session_id").and_then(Value::as_str).unwrap_or(&stored).to_owned();
                let st = r.get("stored_session_id").and_then(Value::as_str).map(str::to_owned).unwrap_or(stored);
                self.bind_live(key, &rt, &st);
                self.after_resume(key, &r);
                (rt, st)
            }
            None => self.create_canonical(&gw, p).await?,
        };
        self.bind_live(key, &runtime, &stored);
        Ok((gw, runtime, stored))
    }

    /// Create a canonical Bot Chat (official createCanonicalChat shape): a hidden session titled
    /// "Bot Chat". Used when a bot is born and when a long break starts a fresh session.
    async fn create_canonical(&self, gw: &Gateway, p: &str) -> Result<(String, String), String> {
                let r = gw
                    .call(
                        "session.create",
                        json!({"profile": p, "title": BOT_CHAT_TITLE, "hidden": true, "follow_profile_config": true, "request_id": uuid::Uuid::new_v4().to_string(), "source": "gui"}),
                    )
                    .await
                    .map_err(|e| rpc_msg(&e))?;
                let rt = r.get("session_id").and_then(Value::as_str).ok_or("Hermes didn't return a session.")?.to_owned();
                let st = r.get("stored_session_id").or_else(|| r.pointer("/info/stored_session_id")).and_then(Value::as_str).unwrap_or(&rt).to_owned();
                let title = r.pointer("/info/title").and_then(Value::as_str).unwrap_or_default();
                if title != BOT_CHAT_TITLE {
                    let t = gw.call("session.title", json!({"session_id": rt, "title": BOT_CHAT_TITLE, "profile": p})).await;
                    if matches!(&t, Err(e) if e.code == METHOD_NOT_FOUND) {
                        let rev = r.get("revision").cloned().unwrap_or(Value::Null);
                        let _ = gw
                            .call("session.mutate", json!({"session_id": rt, "request_id": uuid::Uuid::new_v4().to_string(), "expected_revision": rev, "operation": "rename", "payload": {"title": BOT_CHAT_TITLE}, "profile": p}))
                            .await;
                    }
                }
        Ok((rt, st))
    }

    /// Point this bot's live slot at another runtime session.
    /// Re-raise `open_requests` and keep `inflight` for `open_bot` to replay after history.
    fn after_resume(&self, key: &BotKey, r: &Value) {
        if let Some(t) = r.get("inflight").filter(|v| v.is_object()) {
            let mut st = self.st.lock().unwrap();
            if let Some(c) = st.conns.get_mut(&key.conn) {
                c.inflight.insert(key.slot(), t.clone());
                if t.get("streaming").and_then(Value::as_bool) == Some(true) {
                    let so_far = t.get("assistant").and_then(Value::as_str).unwrap_or_default().to_owned();
                    c.turn_text.insert(key.slot(), so_far);
                }
            }
        }
        for req in r.get("open_requests").and_then(Value::as_array).into_iter().flatten() {
            let (Some(id), Some(method)) = (req.get("id").and_then(Value::as_str), req.get("method").and_then(Value::as_str)) else { continue };
            self.server_request(&key.conn, id.to_owned(), method, req.get("params").unwrap_or(&Value::Null));
        }
    }

    /// Replay a resumed in-flight turn with the ordinary turn events.
    fn replay_inflight(&self, key: &BotKey) {
        let t = {
            let mut st = self.st.lock().unwrap();
            st.conns.get_mut(&key.conn).and_then(|c| c.inflight.remove(&key.slot()))
        };
        let Some(t) = t else { return };
        let s = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
        let (user, assistant, error) = (s("user"), s("assistant"), s("error"));
        let streaming = t.get("streaming").and_then(Value::as_bool).unwrap_or(false);
        let dk = s("display_kind");
        let shown = t.get("display_metadata").and_then(|m| m.get("display_text")).and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| user.clone());
        if dk != "hidden" && !shown.is_empty() {
            let role = if dk.is_empty() { Role::User } else { Role::System };
            self.emit(Ev::Incoming { key: key.clone(), message: ChatMessage::new(format!("inflight-{}", uuid::Uuid::new_v4()), role, shown, Some(now())) });
        }
        if !(streaming || !assistant.is_empty() || !error.is_empty()) {
            return;
        }
        self.emit(Ev::TurnStart { key: key.clone() });
        if !assistant.is_empty() {
            self.emit(Ev::Delta { key: key.clone(), text: assistant.clone() });
        }
        if !streaming {
            self.emit(Ev::TurnEnd { key: key.clone(), text: assistant, error: Some(if error.is_empty() { "The turn failed.".into() } else { error }) });
        }
    }

    fn bind_live(&self, key: &BotKey, runtime: &str, stored: &str) {
        let mut st = self.st.lock().unwrap();
        if let Some(c) = st.conns.get_mut(&key.conn) {
            c.by_runtime.insert(runtime.to_owned(), key.clone());
            c.live.insert(key.slot(), Live { stored: stored.to_owned(), runtime: runtime.to_owned() });
        }
    }

    /// `session.compress`, falling back to `session.mutate compress` where the shared gateway
    /// refuses the former (docs: gateway method ledger).
    async fn compress(&self, gw: &Gateway, sid: &str, p: &str, focus: Option<&str>) -> Result<(), String> {
        let r = gw.call_timeout("session.compress", json!({"session_id": sid, "focus_topic": focus, "profile": p}), Duration::from_secs(300)).await;
        let r = match r {
            Err(e) if e.code == METHOD_NOT_FOUND || e.code == -32602 => {
                gw.call_timeout("session.mutate", json!({"session_id": sid, "request_id": uuid::Uuid::new_v4().to_string(), "operation": "compress", "payload": {"focus": focus}, "profile": p}), Duration::from_secs(300)).await
            }
            other => other,
        };
        r.map(|_| ()).map_err(|e| e.message)
    }

    async fn open_bot(&self, key: BotKey, from_cache: bool) {
        if from_cache {
            if let Some((_, msgs)) = self.store.load_chat(&key) {
                self.emit(Ev::History { key: key.clone(), messages: msgs, cached: true });
            }
        }
        let (_, _, stored) = match self.ensure_live(&key).await {
            Ok(v) => v,
            Err(m) => {
                if self.gw(&key.conn).is_some_and(|g| g.link() == LinkState::Connected) {
                    self.emit(Ev::Error { key: Some(key), message: m });
                }
                return;
            }
        };
        let Some(rest) = self.rest(&key.conn) else { return };
        match rest.messages(&stored, &key.profile, 120).await {
            Ok(rows) => {
                let mut msgs = transcript::from_history(&rows);
                // One continuous stream per bot: fold in its other stored sessions (CLI, messaging,
                // earlier Bot Chats) so the UI reads like a single conversation.
                if key.thread.is_none() {
                    msgs = self.merge_other_sessions(&key, &stored, msgs).await;
                }
                let _ = self.store.save_chat(&key, Some(&stored), &msgs);
                self.emit(Ev::History { key: key.clone(), messages: msgs, cached: false });
                self.replay_inflight(&key);
            }
            Err(e) => self.emit(Ev::Error { key: Some(key), message: format!("Couldn't load history: {e}") }),
        }
    }

    /// The bot's other recent sessions (`session.list {profile}` plus each one's stored messages),
    /// merged by time with the canonical chat. Sessions from other surfaces get a small marker
    /// row so the stream says where they came from. Read-only: nothing on the server changes.
    async fn merge_other_sessions(&self, key: &BotKey, canonical: &str, mut msgs: Vec<ChatMessage>) -> Vec<ChatMessage> {
        const MAX_SESSIONS: usize = 12;
        let (Some(gw), Some(rest)) = (self.gw(&key.conn), self.rest(&key.conn)) else { return msgs };
        let Ok(r) = gw.call("session.list", json!({"profile": key.profile, "limit": MAX_SESSIONS + 1, "include_hidden": true})).await else { return msgs };
        let rows = r.get("sessions").or_else(|| r.get("rows")).or(Some(&r)).and_then(Value::as_array).cloned().unwrap_or_default();
        let wanted: Vec<(String, String, String)> = rows
            .iter()
            .filter_map(|row| {
                let id = row.get("resolved_id").or_else(|| row.get("session_id")).or_else(|| row.get("id")).and_then(Value::as_str)?;
                let s = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                (id != canonical).then(|| (id.to_owned(), s("title"), s("source")))
            })
            .take(MAX_SESSIONS)
            .collect();
        // Fetch concurrently so opening a bot stays quick.
        let fetched = futures_util::future::join_all(wanted.iter().map(|(id, _, _)| rest.messages(id, &key.profile, 60))).await;
        let mut extra = Vec::new();
        for ((id, title, source), hist) in wanted.into_iter().zip(fetched) {
            let Ok(hist) = hist else { continue };
            let part = transcript::from_history(&hist);
            let Some(first_ts) = part.iter().find_map(|m| m.timestamp) else { continue };
            // Earlier canonical chats need no marker: they are the same conversation.
            if title != BOT_CHAT_TITLE {
                let label = match (title.is_empty(), source.is_empty()) {
                    (false, false) => format!("{title} · from {source}"),
                    (false, true) => title.clone(),
                    (true, false) => format!("Conversation from {source}"),
                    (true, true) => "Earlier conversation".to_owned(),
                };
                extra.push(ChatMessage::new(format!("sess-{id}"), Role::System, label, Some(first_ts - 0.001)));
            }
            extra.extend(part);
        }
        if !extra.is_empty() {
            msgs.extend(extra);
            msgs.sort_by(|a, b| a.timestamp.unwrap_or(0.0).total_cmp(&b.timestamp.unwrap_or(0.0)));
        }
        msgs
    }

    async fn send(&self, key: BotKey, text: String, files: Vec<LocalFile>, refresh: Option<GapAction>) -> Result<(), String> {
        let (gw, mut sid, _) = self.ensure_live(&key).await?;
        let p = key.profile.clone();
        // /new and /reset would fork the forever-chat: compact instead (docs: Bot Mode).
        if files.is_empty() {
            if let Some(cmd) = roster::rewrite_bot_chat_command(&text) {
                let focus = cmd.strip_prefix("/compact").map(str::trim).filter(|s| !s.is_empty());
                return match self.compress(&gw, &sid, &p, focus).await {
                    Ok(()) => {
                        self.emit(Ev::TurnEnd { key: key.clone(), text: String::new(), error: None });
                        self.emit(Ev::Info { message: "Bot Chat compacted: fresh working context, same conversation.".into() });
                        Ok(())
                    }
                    Err(m) => Err(format!("Couldn't compact: {m}")),
                };
            }
            // Any other slash command runs through the official `slash.exec` (docs: *Shared
            // authority local slash commands*). Skills come back as a message to submit.
            if let Some(cmd) = text.trim().strip_prefix('/').filter(|c| !c.is_empty() && !c.starts_with('/')) {
                let command = format!("/{cmd}");
                let name = command.split_whitespace().next().unwrap_or(&command).to_owned();
                match gw.call_timeout("slash.exec", json!({"session_id": sid, "command": command, "profile": p}), Duration::from_secs(120)).await {
                    Ok(r) if r.get("type").and_then(Value::as_str) == Some("skill") => {
                        let msg = r.get("message").and_then(Value::as_str).unwrap_or_default().to_owned();
                        if msg.is_empty() {
                            return Err(format!("{name} returned nothing to send."));
                        }
                        return Box::pin(self.send(key, msg, vec![], refresh)).await;
                    }
                    Ok(r) => {
                        let out = r.get("output").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| r.to_string());
                        self.emit(Ev::TurnEnd { key: key.clone(), text: String::new(), error: None });
                        self.emit(Ev::Slash { key, command: name, output: out, error: false });
                        return Ok(());
                    }
                    Err(e) => {
                        let why = if e.code == METHOD_NOT_FOUND {
                            "This Hermes doesn't run slash commands for desktop clients (slash.exec).".to_owned()
                        } else if e.message.contains("unsupported_command") {
                            format!("Hermes doesn't run {name} from desktop clients. Try it in the CLI or a messaging app.")
                        } else {
                            e.message.clone()
                        };
                        self.emit(Ev::TurnEnd { key: key.clone(), text: String::new(), error: None });
                        self.emit(Ev::Slash { key, command: name, output: why, error: true });
                        return Ok(());
                    }
                }
            }
        }
        // Long break: refresh the working context before this prompt.
        match refresh {
            Some(GapAction::Compact) => {
                if let Err(m) = self.compress(&gw, &sid, &p, None).await {
                    log::warn!("long-break compaction failed: {m}");
                }
            }
            Some(GapAction::Fresh) if key.thread.is_none() => {
                let (rt, st) = self.create_canonical(&gw, &p).await?;
                self.bind_live(&key, &rt, &st);
                sid = rt;
            }
            _ => {}
        }
        let mut text = text;
        let mut attachments = Vec::new();
        let rest = self.rest(&key.conn).ok_or("Not connected.")?;
        for f in files {
            if f.size > MAX_FILE_BYTES {
                return Err(format!("{} is larger than Hermes accepts (256 MB).", f.name));
            }
            let path = f.path.clone();
            // Read and encode off the async workers so big files never stall streaming.
            let b64 = tokio::task::spawn_blocking(move || std::fs::read(&path).map(|b| base64::engine::general_purpose::STANDARD.encode(b)))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("Couldn't read {}: {e}", f.name))?;
            let data_url = format!("data:{};base64,{}", f.mime, b64);
            if f.mime.starts_with("image/") && f.size <= MAX_IMAGE_BYTES {
                match rest.upload_image(&p, &f.name, data_url.clone()).await {
                    Ok(v) if v.get("path").is_some() => {
                        attachments.push(json!({"path": v["path"], "mime": v.get("mime_type").cloned().unwrap_or(json!(f.mime))}));
                        continue;
                    }
                    _ => {
                        let r = gw.call_timeout("image.attach_bytes", json!({"session_id": sid, "content_base64": b64_of(&data_url), "filename": f.name, "profile": p}), Duration::from_secs(300)).await;
                        if r.is_ok() {
                            continue;
                        }
                    }
                }
            }
            let r = gw
                .call_timeout("file.attach", json!({"session_id": sid, "name": f.name, "data_url": data_url, "profile": p}), Duration::from_secs(600))
                .await
                .map_err(|e| format!("Couldn't attach {}: {}", f.name, e.message))?;
            match r.get("ref_text").and_then(Value::as_str) {
                Some(rt) => text = format!("{text}\n{rt}"),
                None => return Err(format!("Hermes couldn't attach {}.", f.name)),
            }
        }
        if let Some(note) = roster::mention_note(&roster::resolve_mentions(&text, &self.all_bots(), &key.conn)) {
            text.push_str(&note);
        }
        let id_key = if self.session_authority(&key.conn) { "input_id" } else { "submission_id" };
        let mut params = json!({"session_id": sid, "text": text, "profile": p});
        params[id_key] = json!(uuid::Uuid::new_v4().to_string());
        if !attachments.is_empty() {
            params["attachments"] = Value::Array(attachments);
        }
        gw.call("prompt.submit", params).await.map_err(|e| e.message)?;
        let _ = self.store.set_seen(&key, now());
        Ok(())
    }

    async fn create_bot(&self, conn: String, name: String, title: String, description: String) {
        let Some(gw) = self.gw(&conn) else { return };
        let r = gw.call_timeout("profiles.create", json!({"name": name, "description": description}), Duration::from_secs(120)).await;
        match r {
            Ok(_) => {
                if !title.trim().is_empty() {
                    let key = BotKey::new(&conn, &name);
                    self.clone().refresh_roster(conn.clone()).await;
                    let _ = self.set_meta(&key, json!({"title": title, "created": now()})).await;
                }
                self.clone().refresh_roster(conn).await;
                self.emit(Ev::Info { message: format!("Created bot {name}.") });
            }
            Err(e) => self.emit(Ev::Error { key: None, message: format!("Couldn't create {name}: {}", e.message) }),
        }
    }

    /// Compare-and-swap merge of `ui_meta["hermes-bots"]`, as the official Bots plugin does it
    /// (at most 3 attempts; `null` deletes a key).
    async fn set_meta(&self, key: &BotKey, patch: Value) -> Result<(), String> {
        let gw = self.gw(&key.conn).ok_or("Not connected.")?;
        for _ in 0..3 {
            let bot = self.bot(key).ok_or("Unknown bot.")?;
            let mut meta = serde_json::to_value(&bot.meta).unwrap_or(json!({}));
            if let (Some(m), Some(pt)) = (meta.as_object_mut(), patch.as_object()) {
                for (k, v) in pt {
                    if v.is_null() {
                        m.remove(k);
                    } else {
                        m.insert(k.clone(), v.clone());
                    }
                }
            }
            let mut params = json!({"name": key.profile, "ui_meta": {UI_META_KEY: meta}});
            if let Some(rev) = bot.meta_revision {
                params["ui_meta_expected_revisions"] = json!({UI_META_KEY: rev});
            }
            let r = gw.call("profiles.configure", params).await.map_err(|e| e.message)?;
            let conflict = r.pointer("/applied/ui_meta_conflicts").is_some_and(|c| c.as_object().is_some_and(|o| !o.is_empty()));
            self.clone().refresh_roster(key.conn.clone()).await;
            if !conflict {
                return Ok(());
            }
        }
        Err("Another desktop kept changing this bot; try again.".into())
    }
}

fn b64_of(data_url: &str) -> &str {
    data_url.split_once(";base64,").map(|(_, b)| b).unwrap_or("")
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
}

/// Verify a sign-in method against the server and keep the result in the keychain.
async fn sign_in(conn_id: &str, base: &str, method: SignInMethod) -> Result<String, String> {
    let base = Endpoint::new(base, "").map_err(|e| e.to_string())?.base;
    let stored = match method {
        SignInMethod::Password { provider, username, password, remember } => {
            let cred = auth::login_password(&base, &provider, &username, &password).await.map_err(|e| e.to_string())?;
            Stored { cred, provider: Some(provider), username: Some(username), password: remember.then_some(password) }
        }
        SignInMethod::Token { token } => Stored { cred: Credential::Token { token }, ..Default::default() },
        SignInMethod::Loopback => Stored { cred: Credential::Loopback, ..Default::default() },
    };
    // Prove the credential works before saving it: `GET /api/auth/me` needs a valid one.
    let probe = Auth::new(conn_id, base, stored.clone(), false);
    let rest = Rest::new(probe.clone()).map_err(|e| e.to_string())?;
    match rest.me().await {
        Ok(_) => {}
        Err(e) if matches!(stored.cred, Credential::Token { .. }) => return Err(format!("Hermes didn't accept that token. {e}")),
        Err(e) => return Err(e.to_string()),
    }
    if conn_id != "env" {
        stored.save(conn_id).map_err(|e| format!("Couldn't save to the system keychain: {e}"))?;
    }
    Ok(probe.identity())
}

const MEDIA_TTL: Duration = Duration::from_secs(3600);

/// Cache file for an inline image: one per (server, profile, remote path), keeping the extension
/// so the image decoder can sniff it.
pub fn media_cache_path(cache_dir: &std::path::Path, conn: &str, profile: &str, remote: &str) -> PathBuf {
    let ext = std::path::Path::new(remote).extension().and_then(|e| e.to_str()).filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("img").to_ascii_lowercase();
    cache_dir.join("media").join(format!("{:016x}.{ext}", hash(&format!("{conn}\0{profile}\0{remote}"))))
}
