//! A stand-in Hermes backend for tests and offline development.
//!
//! It follows the documented wire. Dashboard REST uses `X-Hermes-Session-Token`.
//! `/api/ws?token=` serves JSON-RPC 2.0 (`gateway.ready` event, `client.capabilities`,
//! `profiles.*`, `session.*`, `prompt.submit` with streamed `event` notifications and a
//! server→client `approval` request). `/v1/capabilities` and `/health` stand in for the API
//! server. Replies are canned; the shapes are what matter.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;

#[derive(Clone)]
struct Profile {
    name: String,
    description: String,
    ui_meta: Value,
    revision: i64,
}

#[derive(Clone)]
struct Session {
    /// Compression parent: this session continues `parent` after a compaction. The registry
    /// row (the root) keeps the pre-compaction transcript; the chain's last session is the
    /// live tip (`get_compression_tip`).
    parent: Option<String>,
    profile: String,
    title: String,
    messages: Vec<Value>,
    last_active: f64,
}

struct Inner {
    token: String,
    profiles: Vec<Profile>,
    sessions: HashMap<String, Session>,
    runtime: HashMap<String, String>,
    next_row: i64,
    /// Socket writers by runtime session id, for delivering events.
    sinks: HashMap<String, mpsc::UnboundedSender<String>>,
    pending_approvals: HashMap<String, tokio::sync::oneshot::Sender<Value>>,
    files: HashMap<String, Vec<u8>>,
    /// Delay between streamed words (ms).
    pace_ms: u64,
    /// Apps state (per mock, not per profile, to keep it simple).
    connected: Vec<String>,
    mcp_installed: Vec<String>,
    toolsets_off: Vec<String>,
    /// Gated mode (`--basic-auth`): the `basic` provider's account, as on a non-loopback bind.
    basic: Option<(String, String)>,
    login: LoginState,
    /// stored session → runtime id of the socket it is attached to now (turns follow a resume).
    attached: HashMap<String, String>,
    /// stored session → the turn still running (`session.resume.inflight`).
    inflight: HashMap<String, Value>,
    /// stored session → unanswered server→client request frames (`open_requests`).
    open_reqs: HashMap<String, Vec<Value>>,
    /// Closes every socket (tests: simulate a dropped connection).
    kill: Option<tokio::sync::broadcast::Sender<()>>,
    /// Serve sockets as the session authority (`gateway.ready.session_authority`), with the
    /// canonical contracts, instead of the plain gateway dispatcher.
    authority: bool,
    /// `session.create` retry keys already answered → stored id (`idempotency_key` /
    /// `request_id`), so a retried create returns the same session.
    created: HashMap<String, String>,
    /// Images attached with `image.attach_bytes`, waiting for the session's next prompt.
    staged: HashMap<String, Vec<Value>>,
    /// Requests refused for keys outside their contract: `(method, message)`.
    refusals: Vec<(String, String)>,
    /// Methods answered without a contract refusal, in order.
    handled: Vec<String>,
    /// Plain contracts of the first strict releases (v2026.9.14 / v2026.9.24).
    legacy: bool,
    /// Extra `(method, key)` refusals (a release that dropped a key).
    forbidden: Vec<(String, String)>,
    /// Methods this release does not serve (-32601).
    unserved: Vec<String>,
    /// Delay before `gateway.ready` (ms); requests are served meanwhile, as on a real socket.
    ready_delay_ms: u64,
}

impl Inner {
    /// Live tip of a compression chain (`get_compression_tip`): the input when nothing continues it.
    fn tip(&self, id: &str) -> String {
        let mut cur = id.to_owned();
        while let Some((child, _)) = self
            .sessions
            .iter()
            .find(|(_, s)| s.parent.as_deref() == Some(cur.as_str()))
        {
            cur = child.clone();
        }
        cur
    }

    /// Root of the chain `id` belongs to.
    fn root(&self, id: &str) -> String {
        let mut cur = id.to_owned();
        while let Some(p) = self.sessions.get(&cur).and_then(|s| s.parent.clone()) {
            cur = p;
        }
        cur
    }

    /// The profile's canonical "Bot Chat" registry row (title identity, as `get_session_by_title`).
    fn canonical_root(&self, profile: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|(_, s)| s.profile == profile && s.title == "Bot Chat" && s.parent.is_none())
            .map(|(id, _)| id.clone())
    }
}

/// `_PROFILE_IMPLICIT` (`gateway/session_controls.py`): the authority's session verbs whose
/// `profile` key it strips (own profile) or routes on (a sibling) before validating params.
const PROFILE_IMPLICIT: &[&str] = &[
    "session.create",
    "session.list",
    "session.info",
    "session.resume",
    "session.detach",
    "prompt.submit",
    "prompt.receipt",
    "prompt.cancel",
    "session.interrupt",
    "session.events.since",
    "session.mutate",
    "session.stats",
    "approval.respond",
];

/// Keys each method's params may carry, from hermes-agent v0.23.2 `tui_gateway/contracts`
/// (plain dispatcher: `METHODS`; authority: `CANONICAL_METHODS`, checked after `profile` is
/// stripped, see [`PROFILE_IMPLICIT`]). Both refuse unknown keys before any handler runs
/// (`registry.validate_params`, `canonical_param_problems`); the authority also refuses missing
/// required keys. Returns `(keys, required, canonical)`.
fn contract(
    method: &str,
    authority: bool,
) -> Option<(&'static [&'static str], &'static [&'static str], bool)> {
    const NONE: &[&str] = &[];
    let canonical: Option<(&[&str], &[&str])> = match method {
        "session.create" => Some((
            &[
                "request_id",
                "source",
                "cwd",
                "row_cwd",
                "model",
                "provider",
                "base_url",
                "api_key",
                "toolsets",
                "reasoning",
                "max_turns",
                "ignore_rules",
                "yolo",
                "safe_mode",
                "ignore_user_config",
                "skills",
                "checkpoints",
                "accept_hooks",
                "pass_session_id",
                "editor",
                "title",
                "hidden",
                "follow_profile_config",
                "service_tier",
                "cwd_explicit",
                "run_budget",
                "verbose",
                "client_env",
                "client",
            ],
            NONE,
        )),
        "session.resume" => Some((
            &[
                "session_id",
                "title",
                "latest",
                "workspace",
                "source",
                "editor",
                "api_key",
                "defer_history",
                "omit_messages",
                "cols",
                "display_projection",
            ],
            NONE,
        )),
        "session.list" => Some((
            &["scope", "limit", "title", "include_hidden", "workspace", "picker", "query", "cursor"],
            NONE,
        )),
        "session.interrupt" => Some((
            &["session_id", "execution_generation"],
            &["session_id", "execution_generation"],
        )),
        // `submission_id` / `input_id` are both optional here; the handler requires one of them
        // (`AuthorityConnection.submission`), see `dispatch`.
        "prompt.submit" => Some((
            &[
                "session_id",
                "text",
                "submission_id",
                "input_id",
                "queued",
                "attachments",
                "finite",
                "unattended",
                "surface",
                "voice_context",
                "interrupted",
                "voice_turn",
                "display_kind",
                "title_preview",
                "turn_author",
                "api_text",
                "model_override",
                "clarify_batch",
            ],
            &["session_id", "text"],
        )),
        "session.mutate" => Some((
            &[
                "session_id",
                "request_id",
                "expected_revision",
                "operation",
                "payload",
                "expected_generation",
            ],
            &[
                "session_id",
                "request_id",
                "expected_revision",
                "operation",
                "payload",
            ],
        )),
        _ => None,
    };
    if let (true, Some((keys, required))) = (authority, canonical) {
        return Some((keys, required, true));
    }
    let keys: &[&str] = match method {
        "client.capabilities" => &["server_requests", "desktop_local"],
        "profiles.list" => &["profile", "include_sessions"],
        "profiles.configure" => &[
            "profile",
            "name",
            "ui_meta",
            "ui_meta_expected_revisions",
            "soul",
            "description",
            "model",
            "provider",
            "confirm_expensive_model",
            "disabled_skills",
            "enabled_toolsets",
            "enabled_mcp_servers",
        ],
        "profiles.create" => &[
            "profile",
            "name",
            "description",
            "clone_from",
            "clone_all",
            "clone_channels",
            "clone_config",
            "no_skills",
            "no_alias",
            "soul",
            "model",
            "provider",
            "share_auth",
            "mirror_credentials",
            "report",
            "sync_imports",
        ],
        "profiles.describe" => &["profile", "name"],
        "session.list" => &["profile", "title", "limit", "include_hidden"],
        "session.create" => &[
            "profile",
            "cols",
            "source",
            "cwd",
            "cwd_explicit",
            "messages",
            "parent_session_id",
            "title",
            "model",
            "provider",
            "reasoning_effort",
            "fast",
            "service_tier",
            "close_on_disconnect",
            "hidden",
            "room_plumbing",
            "follow_profile_config",
            "idempotency_key",
        ],
        "session.title" => &["session_id", "profile", "title"],
        "session.resume" => &[
            "session_id",
            "profile",
            "cols",
            "source",
            "lazy",
            "defer_history",
            "omit_messages",
            "eager_build",
            "close_on_disconnect",
            "inline_images",
        ],
        "session.interrupt" => &["session_id", "profile", "expected_hosted_task_id"],
        "session.compress" => &["session_id", "profile", "focus_topic"],
        "slash.exec" => &["session_id", "command", "profile"],
        "image.attach_bytes" => &[
            "session_id",
            "profile",
            "content_base64",
            "data",
            "filename",
            "ext",
        ],
        "file.attach" => &["session_id", "profile", "path", "data_url", "name", "staged_upload"],
        "prompt.submit" => &[
            "session_id",
            "profile",
            "text",
            "display_kind",
            "interrupted",
            "queued",
            "surface",
            "voice_context",
            "voice_turn",
            "title_preview",
            "truncate_before_user_ordinal",
            "truncate_before_row_id",
            "truncate_before_message_id",
            "confirm_truncate",
            "confirm_empty_truncate",
            "confirm_deep_truncate",
            "rebind_survivor_row_ids",
        ],
        "commands.catalog" => &["session_id", "profile"],
        "connectors.catalog" | "mcp.catalog" => &["profile"],
        "connectors.list" => &["profile", "owner"],
        "connectors.connect" => &[
            "profile",
            "owner",
            "connectors",
            "reconnect",
            "alias",
            "connection_id",
        ],
        "connectors.tools" => &["profile", "slug", "refresh"],
        "mcp.servers.add" => &["profile", "name", "preset", "config", "bearer_token"],
        "mcp.servers.remove" => &["profile", "name"],
        "toolsets.list" => &["session_id"],
        "tools.configure" => &["action", "names", "session_id", "profile"],
        "skills.manage" => &["profile", "action", "query", "page", "page_size"],
        _ => return None,
    };
    Some((keys, NONE, false))
}

/// Plain-gateway keys that the first strict releases (v2026.9.14, v2026.9.24) did not declare
/// yet, for [`Mock::as_release_2026_9`].
fn absent_in_2026_9(method: &str) -> &'static [&'static str] {
    match method {
        "session.create" => &["cwd_explicit", "idempotency_key", "service_tier"],
        "session.resume" => &["inline_images"],
        "prompt.submit" => &["confirm_deep_truncate", "title_preview", "voice_turn"],
        _ => &[],
    }
}

/// A contract refusal, as Hermes words it: `(code, message, data)`.
type Refusal = (i64, String, Value);

/// The gateway's refusal for params outside the method's contract. `legacy`: the plain
/// contracts of v2026.9.x; `forbidden`: extra `(method, key)` pairs a test wants refused.
fn check_params(method: &str, p: &Value, authority: bool, legacy: bool, forbidden: &[(String, String)]) -> Result<(), Refusal> {
    if legacy && method == "client.capabilities" {
        return Ok(()); // no contract yet in v2026.9.x: not validated
    }
    let Some((allowed, required, canonical)) = contract(method, authority) else {
        return Ok(());
    };
    let mut obj = p.as_object().cloned().unwrap_or_default();
    if canonical && PROFILE_IMPLICIT.contains(&method) {
        obj.remove("profile");
    }
    let accepts = |k: &str| {
        allowed.contains(&k)
            && !(legacy && !authority && absent_in_2026_9(method).contains(&k))
            && !forbidden.iter().any(|(m, f)| m == method && f == k)
    };
    let extra: Vec<String> = obj.keys().filter(|k| !accepts(k)).cloned().collect();
    if canonical {
        // `canonical_param_problems`: every unknown and missing key, in `data.fields`.
        let missing = required.iter().filter(|k| obj.get(**k).is_none_or(Value::is_null)).map(|k| k.to_string());
        let fields: Vec<String> = extra.into_iter().chain(missing).collect();
        if !fields.is_empty() {
            return Err((4001, "invalid_params".into(), json!({"reason": "invalid_params", "fields": fields})));
        }
        return Ok(());
    }
    // `validate_params`: the first unknown key, with Hermes's own (version-skew) advice.
    if let Some(k) = extra.first() {
        return Err((4000, format!("invalid params for {method}: {k}: Extra inputs are not permitted — the client and the Hermes backend are out of sync (different versions); run `hermes update` and restart both"), Value::Null));
    }
    Ok(())
}

/// Sign-in bookkeeping for gated mode (shapes of `hermes_cli/dashboard_auth` + `web_server_auth`).
#[derive(Default)]
struct LoginState {
    access_ttl: i64,
    seq: u64,
    /// pkce cookie -> (challenge, redirect_uri, state)
    pkce: HashMap<String, (String, String, String)>,
    /// code -> (challenge, user)
    codes: HashMap<String, (String, String)>,
    /// access token -> expires_at
    access: HashMap<String, i64>,
    refresh: HashMap<String, String>,
    tickets: Vec<String>,
    attempts: Vec<f64>,
    /// Counters for tests.
    pub refreshed: u32,
    pub logins: u32,
    pub password_posts: u32,
    refresh_delay_ms: u64,
}

#[derive(Clone)]
pub struct Mock(Arc<Mutex<Inner>>);

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

fn meta(title: &str, color: &str) -> Value {
    json!({"hermes-bots": {"title": title, "color": color}})
}

impl Mock {
    pub fn new(token: &str) -> Mock {
        let t = now();
        let profiles = vec![
            Profile { name: "default".into(), description: "General assistant".into(), ui_meta: meta("Hermes", "#7c5cff"), revision: 1 },
            Profile { name: "researcher".into(), description: "Finds and summarizes sources".into(), ui_meta: meta("Research Buddy", "#2bb673"), revision: 2 },
            Profile { name: "scribe".into(), description: "Writes and edits prose".into(), ui_meta: meta("Scribe", "#e8a33d"), revision: 1 },
            Profile { name: "coder".into(), description: "Writes and reviews code".into(), ui_meta: meta("Code Monkey", "#3d8be8"), revision: 1 },
            Profile { name: "archivist".into(), description: "Old bot".into(), ui_meta: json!({"hermes-bots": {"title": "Archivist", "hidden": true}}), revision: 1 },
        ];
        let mut sessions = HashMap::new();
        let row = |id: i64, role: &str, content: &str, ts: f64| json!({"id": id, "role": role, "content": content, "timestamp": ts});
        sessions.insert(
            "s-default".to_owned(),
            Session {
                parent: None,
                profile: "default".into(),
                title: "Bot Chat".into(),
                messages: vec![
                    row(1, "user", "Morning! What's on today?", t - 7200.0),
                    row(2, "assistant", "Good morning. You have **two** things queued:\n\n1. Review the *Q3 notes*\n2. Reply to Sam about the offsite\n\nWant me to draft the reply?", t - 7190.0),
                    row(5, "user", "Keep an eye on agent news for me every weekday morning", t - 5400.0),
                    json!({"id": 6, "role": "assistant", "content": "", "timestamp": t - 5395.0,
                        "tool_calls": [{"id": "cj1", "type": "function", "function": {"name": "cronjob", "arguments": "{\"action\":\"update\",\"job_id\":\"j-news\",\"name\":\"News watch\",\"schedule\":\"0 8 * * 1-5\"}"}}]}),
                    row(7, "assistant", "Done. **News watch** now runs at 08:00 on weekdays and posts a short digest here.", t - 5390.0),
                    row(3, "user", "Message from 🤖 Research Buddy (@research-buddy): I found 3 papers on retrieval-augmented agents, summary in your inbox.", t - 3600.0),
                    row(8, "user", "Message from 🤖 Research Buddy (@research-buddy): Also flagged a 2024 follow-up on memory compaction. It is the second link in the doc.", t - 3598.0),
                    row(4, "assistant", "Research Buddy sent over three papers. Here's the gist:\n\n```rust\nfn rank(papers: &[Paper]) -> Vec<&Paper> {\n    let mut v: Vec<_> = papers.iter().collect();\n    v.sort_by_key(|p| std::cmp::Reverse(p.citations));\n    v\n}\n```\n\nSee [the survey](https://example.com/survey) for details.", t - 3590.0),
                ],
                last_active: t - 3590.0,
            },
        );
        sessions.insert(
            "s-researcher".to_owned(),
            Session {
                parent: None,
                profile: "researcher".into(),
                title: "Bot Chat".into(),
                messages: vec![
                    row(10, "user", "Find papers on agent memory", t - 86400.0),
                    row(
                        11,
                        "assistant",
                        "On it. Top pick: *MemGPT* (2023).",
                        t - 86300.0,
                    ),
                ],
                last_active: t - 86300.0,
            },
        );
        sessions.insert(
            "s-coder-old".to_owned(),
            Session {
                parent: None,
                profile: "coder".into(),
                title: "Refactor auth".into(),
                messages: vec![
                    row(20, "user", "refactor the auth module", t - 4000.0),
                    row(
                        21,
                        "assistant",
                        "Done: split into `token.rs` and `session.rs`.",
                        t - 3900.0,
                    ),
                ],
                last_active: t - 3900.0,
            },
        );
        sessions.insert(
            "s-coder-tg".to_owned(),
            Session {
                parent: None,
                profile: "coder".into(),
                title: "Deploy checklist".into(),
                messages: vec![
                    row(30, "user", "what's left before we deploy?", t - 90000.0),
                    row(
                        31,
                        "assistant",
                        "Two things: bump the version and run the migration.",
                        t - 89900.0,
                    ),
                ],
                last_active: t - 89900.0,
            },
        );
        Mock(Arc::new(Mutex::new(Inner {
            token: token.to_owned(),
            profiles,
            sessions,
            runtime: HashMap::new(),
            next_row: 100,
            sinks: HashMap::new(),
            pending_approvals: HashMap::new(),
            files: HashMap::from([("/home/hermes/out/chart.png".to_owned(), demo_png(320, 180)), ("/home/hermes/out/report.pdf".to_owned(), b"%PDF-1.4\n% mock report\n".to_vec())]),
            pace_ms: 25,
            connected: vec!["github".into()],
            mcp_installed: vec!["filesystem".into()],
            toolsets_off: vec!["image_gen".into()],
            basic: None,
            attached: HashMap::new(),
            inflight: HashMap::new(),
            open_reqs: HashMap::new(),
            kill: Some(tokio::sync::broadcast::channel(4).0),
            login: LoginState { access_ttl: 12 * 3600, ..Default::default() },
            authority: false,
            created: HashMap::new(),
            staged: HashMap::new(),
            refusals: Vec::new(),
            handled: Vec::new(),
            legacy: false,
            forbidden: Vec::new(),
            unserved: Vec::new(),
            ready_delay_ms: 0,
        })))
    }

    /// Every request refused for params outside its contract, as `(method, message)` (tests).
    pub fn refusals(&self) -> Vec<(String, String)> {
        self.0.lock().unwrap().refusals.clone()
    }

    /// Add `n` empty sessions (cron runs and idle TUI windows) newer than everything else, as a
    /// busy real server has: they must not crowd real conversations out of the stream.
    pub fn add_empty_sessions(&self, profile: &str, n: usize) {
        let mut i = self.0.lock().unwrap();
        for k in 0..n {
            let kind = if k % 2 == 0 { "cron" } else { "tui" };
            let s = Session { parent: None, profile: profile.to_owned(), title: String::new(), messages: vec![], last_active: now() - k as f64 };
            i.sessions.insert(format!("local-{k}-{kind}"), s);
        }
    }

    /// How many times `method` reached its handler (tests).
    pub fn handled(&self, method: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .handled
            .iter()
            .filter(|m| *m == method)
            .count()
    }

    /// One stored session's own transcript (tests).
    pub fn messages_in(&self, id: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .sessions
            .get(id)
            .map(|s| s.messages.clone())
            .unwrap_or_default()
    }

    /// Serve every socket as the session authority (`gateway.ready.session_authority: true`).
    pub fn as_session_authority(self) -> Mock {
        self.0.lock().unwrap().authority = true;
        self
    }

    /// Serve sockets opened from now on in the other mode (a server restarted with or without
    /// the session authority; existing sockets keep theirs until dropped).
    pub fn set_session_authority(&self, on: bool) {
        self.0.lock().unwrap().authority = on;
    }

    /// Enforce the plain contracts of Hermes v2026.9.14 / v2026.9.24, the first strict releases
    /// (no `idempotency_key` on `session.create`, no `server_requests` capability, ...).
    pub fn as_release_2026_9(self) -> Mock {
        self.0.lock().unwrap().legacy = true;
        self
    }

    /// Refuse `key` on `method` as outside its contract (a release without that key).
    pub fn forbid(&self, method: &str, key: &str) {
        self.0.lock().unwrap().forbidden.push((method.to_owned(), key.to_owned()));
    }

    /// Answer `method` with -32601 (a release that does not serve it).
    pub fn unserve(&self, method: &str) {
        self.0.lock().unwrap().unserved.push(method.to_owned());
    }

    /// Send `gateway.ready` only `ms` after a socket opens.
    pub fn set_ready_delay(&self, ms: u64) {
        self.0.lock().unwrap().ready_delay_ms = ms;
    }

    /// Turn a profile's Bot Chat into a compressed lineage, as a long-lived chat on a real
    /// server is: the registry row keeps two messages from `days_ago`, and a continuation (the
    /// live tip, untitled) holds everything since. Returns `(root, tip)`.
    pub fn compact_bot_chat(&self, profile: &str, days_ago: f64) -> (String, String) {
        let mut i = self.0.lock().unwrap();
        let root = i.canonical_root(profile).expect("profile has a Bot Chat");
        let tip = format!("{root}-c1");
        let old = now() - days_ago * 86400.0;
        let s = i.sessions.get_mut(&root).unwrap();
        let recent = std::mem::replace(
            &mut s.messages,
            vec![
                json!({"id": 901, "role": "user", "content": "An old question from weeks ago", "timestamp": old}),
                json!({"id": 902, "role": "assistant", "content": "An old answer from weeks ago", "timestamp": old + 5.0}),
            ],
        );
        let last_active = s.last_active;
        s.last_active = old + 5.0;
        let continuation = Session {
            parent: Some(root.clone()),
            profile: profile.to_owned(),
            title: String::new(),
            messages: recent,
            last_active,
        };
        i.sessions.insert(tip.clone(), continuation);
        (root, tip)
    }

    /// Require the `basic` provider's username/password, like a dashboard on a public bind.
    pub fn with_basic_auth(self, user: &str, pass: &str) -> Mock {
        self.0.lock().unwrap().basic = Some((user.to_owned(), pass.to_owned()));
        self
    }

    /// Access-token lifetime in seconds (default 12 h, as `session_ttl_seconds`).
    pub fn set_access_ttl(&self, secs: i64) {
        self.0.lock().unwrap().login.access_ttl = secs;
    }

    /// Revoke every native session (as changing the password does).
    pub fn revoke_sessions(&self) {
        let mut i = self.0.lock().unwrap();
        i.login.access.clear();
        i.login.refresh.clear();
    }

    /// Number of canonical "Bot Chat" sessions a profile has (tests).
    pub fn bot_chats(&self, profile: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .sessions
            .values()
            .filter(|s| s.profile == profile && s.title == "Bot Chat" && s.parent.is_none())
            .count()
    }

    /// Drop every WebSocket, as a network blip would. Running turns keep going.
    pub fn drop_sockets(&self) {
        if let Some(k) = &self.0.lock().unwrap().kill {
            let _ = k.send(());
        }
    }

    /// Change the `basic` account's password (sessions survive until `revoke_sessions`).
    pub fn set_password(&self, pass: &str) {
        if let Some(b) = &mut self.0.lock().unwrap().basic {
            b.1 = pass.to_owned();
        }
    }

    /// Slow down `/auth/native/refresh` (tests racing sign-out against a refresh).
    pub fn set_refresh_delay(&self, ms: u64) {
        self.0.lock().unwrap().login.refresh_delay_ms = ms;
    }

    /// Password sign-in attempts so far, successful or not.
    pub fn password_posts(&self) -> u32 {
        self.0.lock().unwrap().login.password_posts
    }

    /// (refreshes, password logins) so far.
    pub fn auth_counts(&self) -> (u32, u32) {
        let i = self.0.lock().unwrap();
        (i.login.refreshed, i.login.logins)
    }

    pub fn set_pace(&self, ms: u64) {
        self.0.lock().unwrap().pace_ms = ms;
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(index))
            .route("/api/status", get(status))
            .route("/api/auth/providers", get(providers))
            .route("/api/auth/me", get(me))
            .route("/api/auth/ws-ticket", post(ws_ticket))
            .route("/auth/native/authorize", get(native_authorize))
            .route("/auth/password-login", post(password_login))
            .route("/auth/native/token", post(native_token))
            .route("/auth/native/refresh", post(native_refresh))
            .route("/api/ws", get(ws))
            .route("/api/sessions", get(sessions_list))
            .route("/api/sessions/{id}/messages", get(messages))
            .route("/api/analytics/usage", get(usage))
            .route("/api/chat/image-upload", post(image_upload))
            .route("/api/files/download", get(download))
            .route("/api/media", get(media))
            .route("/static/logos/{name}", get(logo))
            .route("/v1/capabilities", get(capabilities))
            .route("/health", get(|| async { Json(json!({"status": "ok"})) }))
            .with_state(self.clone())
    }

    /// Bind on `addr` (port 0 for any) and serve in the background. Returns the bound address.
    pub async fn spawn(self, addr: &str) -> SocketAddr {
        let l = tokio::net::TcpListener::bind(addr).await.expect("bind");
        let a = l.local_addr().unwrap();
        let r = self.router();
        tokio::spawn(async move { axum::serve(l, r).await.unwrap() });
        a
    }

    /// Transcript of a stored session (for tests).
    pub fn messages_of(&self, profile: &str) -> Vec<Value> {
        let i = self.0.lock().unwrap();
        let Some(root) = i.canonical_root(profile) else {
            return vec![];
        };
        let mut out = Vec::new();
        let mut cur = Some(root);
        while let Some(id) = cur {
            out.extend(i.sessions[&id].messages.clone());
            cur = i
                .sessions
                .iter()
                .find(|(_, s)| s.parent.as_deref() == Some(id.as_str()))
                .map(|(c, _)| c.clone());
        }
        out
    }

    pub fn ui_meta_of(&self, profile: &str) -> Value {
        self.0.lock().unwrap().profiles.iter().find(|p| p.name == profile).map(|p| p.ui_meta.clone()).unwrap_or(Value::Null)
    }
}

fn authed(m: &Mock, h: &HeaderMap) -> bool {
    who(m, h).is_some()
}

/// The caller's identity: the deployment session token (header or Bearer), or a live native
/// access token (`Authorization: Bearer`), as `web_server.py` accepts them.
fn who(m: &Mock, h: &HeaderMap) -> Option<String> {
    let i = m.0.lock().unwrap();
    let hv = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let bearer = hv("authorization").and_then(|a| a.strip_prefix("Bearer ").map(str::to_owned));
    if i.token.is_empty() && i.basic.is_none() {
        return Some("local".into());
    }
    if !i.token.is_empty() && (hv("x-hermes-session-token").as_deref() == Some(i.token.as_str()) || bearer.as_deref() == Some(i.token.as_str())) {
        return Some("session-token".into());
    }
    let b = bearer?;
    let exp = *i.login.access.get(&b)?;
    (exp > now() as i64).then(|| i.basic.as_ref().map(|(u, _)| u.clone()).unwrap_or_default())
}

fn next_id(i: &mut Inner, kind: &str) -> String {
    i.login.seq += 1;
    format!("{kind}-{}-{}", i.login.seq, (now() * 1000.0) as u64 % 100_000)
}

fn err(code: StatusCode, detail: &str) -> Response {
    (code, Json(json!({"detail": detail}))).into_response()
}

/// A loopback dashboard serves its SPA with the session token injected.
async fn index(State(m): State<Mock>) -> Response {
    let i = m.0.lock().unwrap();
    if i.basic.is_some() {
        return axum::response::Redirect::to("/login").into_response();
    }
    let page = format!("<!doctype html><html><head><script>window.__HERMES_SESSION_TOKEN__=\"{}\";</script></head><body><div id=root></div></body></html>", i.token);
    axum::response::Html(page).into_response()
}

async fn status(State(m): State<Mock>) -> Json<Value> {
    let gated = m.0.lock().unwrap().basic.is_some();
    if gated {
        Json(json!({"version": "mock", "auth_required": true, "auth_providers": ["basic"], "auth_flows": ["native_pkce"], "gateway_running": true}))
    } else {
        Json(json!({"version": "mock", "auth_required": false, "auth_providers": ["session_token"], "gateway_running": true}))
    }
}

async fn providers(State(m): State<Mock>) -> Response {
    if m.0.lock().unwrap().basic.is_none() {
        return err(StatusCode::SERVICE_UNAVAILABLE, "no auth provider registered");
    }
    Json(json!({"providers": [{"name": "basic", "display_name": "Username & password", "supports_password": true}]})).into_response()
}

async fn me(State(m): State<Mock>, h: HeaderMap) -> Response {
    match who(&m, &h) {
        Some(u) => Json(json!({"user_id": u, "provider": "basic", "display_name": u})).into_response(),
        None => err(StatusCode::UNAUTHORIZED, "unauthenticated"),
    }
}

async fn native_authorize(State(m): State<Mock>, Query(q): Query<HashMap<String, String>>) -> Response {
    let mut i = m.0.lock().unwrap();
    if i.basic.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let g = |k: &str| q.get(k).cloned().unwrap_or_default();
    if g("code_challenge_method") != "S256" || g("code_challenge").is_empty() || !g("redirect_uri").starts_with("http://127.0.0.1:") || g("provider") != "basic" {
        return err(StatusCode::BAD_REQUEST, "invalid authorize request");
    }
    let c = next_id(&mut i, "pkce");
    i.login.pkce.insert(c.clone(), (g("code_challenge"), g("redirect_uri"), g("state")));
    let mut r = axum::response::Redirect::to("/login").into_response();
    r.headers_mut().insert("set-cookie", format!("hermes_session_pkce={c}; HttpOnly; SameSite=Lax; Path=/").parse().unwrap());
    r.headers_mut().insert(axum::http::header::LOCATION, "/login".parse().unwrap());
    *r.status_mut() = StatusCode::FOUND;
    r
}

async fn password_login(State(m): State<Mock>, h: HeaderMap, Json(b): Json<Value>) -> Response {
    let mut i = m.0.lock().unwrap();
    let Some((user, pass)) = i.basic.clone() else { return StatusCode::NOT_FOUND.into_response() };
    if b.get("provider").and_then(Value::as_str) != Some("basic") {
        return err(StatusCode::NOT_FOUND, "unknown provider");
    }
    i.login.password_posts += 1;
    let t = now();
    i.login.attempts.retain(|a| t - a < 60.0);
    if i.login.attempts.len() >= 10 {
        return err(StatusCode::TOO_MANY_REQUESTS, "Too many sign-in attempts");
    }
    i.login.attempts.push(t);
    let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    if s("username") != user || s("password") != pass {
        return err(StatusCode::UNAUTHORIZED, "Invalid credentials");
    }
    let cookie = h.get("cookie").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let pk = cookie.split(';').filter_map(|c| c.trim().strip_prefix("hermes_session_pkce=")).next().map(str::to_owned);
    match pk.and_then(|k| i.login.pkce.remove(&k)) {
        Some((challenge, redirect, state)) => {
            let code = next_id(&mut i, "code");
            i.login.codes.insert(code.clone(), (challenge, user));
            Json(json!({"ok": true, "next": format!("{redirect}?code={code}&state={state}")})).into_response()
        }
        // The browser flow would set hermes_session_at/rt cookies here; the app never uses it.
        None => Json(json!({"ok": true, "next": s("next")})).into_response(),
    }
}

fn mint(i: &mut Inner, user: &str) -> Value {
    let at = next_id(i, "at");
    let rt = next_id(i, "rt");
    let exp = now() as i64 + i.login.access_ttl;
    i.login.access.insert(at.clone(), exp);
    i.login.refresh.insert(rt.clone(), user.to_owned());
    json!({"access_token": at, "refresh_token": rt, "token_type": "Bearer", "expires_at": exp, "provider": "basic", "user_id": user})
}

async fn native_token(State(m): State<Mock>, Json(b): Json<Value>) -> Response {
    use sha2::Digest as _;
    let mut i = m.0.lock().unwrap();
    let code = b.get("code").and_then(Value::as_str).unwrap_or_default();
    let verifier = b.get("code_verifier").and_then(Value::as_str).unwrap_or_default();
    let Some((challenge, user)) = i.login.codes.remove(code) else { return err(StatusCode::BAD_REQUEST, "invalid_grant") };
    let want = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
    if want != challenge {
        return err(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    i.login.logins += 1;
    Json(mint(&mut i, &user)).into_response()
}

async fn native_refresh(State(m): State<Mock>, Json(b): Json<Value>) -> Response {
    let delay = m.0.lock().unwrap().login.refresh_delay_ms;
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let mut i = m.0.lock().unwrap();
    let rt = b.get("refresh_token").and_then(Value::as_str).unwrap_or_default();
    let Some(user) = i.login.refresh.remove(rt) else { return err(StatusCode::UNAUTHORIZED, "session_expired") };
    i.login.refreshed += 1;
    Json(mint(&mut i, &user)).into_response()
}

async fn ws_ticket(State(m): State<Mock>, h: HeaderMap) -> Response {
    if !authed(&m, &h) {
        return err(StatusCode::UNAUTHORIZED, "unauthenticated");
    }
    let mut i = m.0.lock().unwrap();
    let t = next_id(&mut i, "ticket");
    i.login.tickets.push(t.clone());
    Json(json!({"ticket": t, "ttl_seconds": 30})).into_response()
}

async fn capabilities(State(_m): State<Mock>) -> Json<Value> {
    Json(json!({"object": "hermes.api_server.capabilities", "platform": "hermes-agent", "model": "hermes-agent",
        "auth": {"type": "bearer", "required": true},
        "features": {"chat_completions": true, "responses_api": true, "run_submission": true, "run_status": true, "run_events_sse": true, "run_stop": true, "reasoning_streaming": true}}))
}

async fn messages(State(m): State<Mock>, h: HeaderMap, Path(id): Path<String>) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let i = m.0.lock().unwrap();
    match i.sessions.get(&id).filter(|s| !s.messages.is_empty()) {
        // A created session has no stored row until its first prompt (`session.create`).
        Some(s) => Json(json!({"session_id": id, "profile": s.profile, "messages": s.messages, "pagination": {"limit": 120, "offset": 0, "order": "latest", "returned": s.messages.len()}})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /api/sessions` (`hermes_cli/web_routers/sessions.py`): `order=recent` sorts compression
/// chains by their latest activity and lists each once, as its tip, with `last_active`; hidden
/// sessions (canonical Bot Chats) are left out.
async fn sessions_list(
    State(m): State<Mock>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let i = m.0.lock().unwrap();
    let profile = q
        .get("profile")
        .cloned()
        .unwrap_or_else(|| "default".into());
    let limit: usize = q.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20);
    let recent = q.get("order").map(String::as_str) == Some("recent");
    let min_messages: usize = q.get("min_messages").and_then(|l| l.parse().ok()).unwrap_or(0);
    let excluded: Vec<&str> = q.get("exclude_sources").map(|s| s.split(',').collect()).unwrap_or_default();
    let mut rows: Vec<(f64, f64, Value)> = i
        .sessions
        .iter()
        .filter(|(id, s)| {
            s.profile == profile && i.tip(id) == **id && i.sessions[&i.root(id)].title != "Bot Chat"
        })
        .filter(|(id, s)| s.messages.len() >= min_messages && !excluded.contains(&source_of(id)))
        .map(|(id, s)| {
            let root = i.root(id);
            let started = i.sessions[&root]
                .messages
                .first()
                .and_then(|m| m["timestamp"].as_f64())
                .unwrap_or(0.0);
            let mut v = summary(id, s);
            v["started_at"] = json!(started);
            v["source"] = json!(source_of(id));
            v["_lineage_root_id"] = if root == *id {
                Value::Null
            } else {
                json!(root)
            };
            (s.last_active, started, v)
        })
        .collect();
    rows.sort_by(|a, b| {
        if recent {
            b.0.total_cmp(&a.0)
        } else {
            b.1.total_cmp(&a.1)
        }
    });
    let total = rows.len();
    let sessions: Vec<Value> = rows.into_iter().take(limit).map(|r| r.2).collect();
    Json(json!({"sessions": sessions, "total": total, "limit": limit, "offset": 0, "storage": {}}))
        .into_response()
}

/// `GET /api/analytics/usage` (shape of `hermes_cli/web_routers/analytics.py`), with
/// deterministic made-up numbers per profile so the Usage page has something to draw.
async fn usage(State(m): State<Mock>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let days: i64 = q.get("days").and_then(|d| d.parse().ok()).unwrap_or(30).clamp(1, 365);
    let profile = q.get("profile").cloned().unwrap_or_else(|| "default".into());
    let seed = profile.bytes().fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
    let weight = match profile.as_str() {
        "default" => 1.0,
        "coder" => 1.6,
        "researcher" => 0.7,
        _ => 0.15,
    };
    let today = chrono::Local::now().date_naive();
    let mut daily = vec![];
    let (mut ti, mut to, mut tc, mut ts, mut ta) = (0i64, 0i64, 0f64, 0i64, 0i64);
    for d in (0..days).rev() {
        let r = (seed.wrapping_mul(d as u64 + 3) >> 7) % 100;
        if r < 22 {
            continue; // quiet day
        }
        let input = ((r as f64 * 950.0 + 4000.0) * weight) as i64;
        let output = input / 4;
        let cost = (input as f64 * 3.0 + output as f64 * 15.0) / 1_000_000.0;
        let sessions = 1 + (r % 3) as i64;
        let calls = sessions * (3 + (r % 7) as i64);
        let day = (today - chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
        daily.push(json!({"day": day, "input_tokens": input, "output_tokens": output, "cache_read_tokens": input / 3, "reasoning_tokens": 0,
            "estimated_cost": cost, "actual_cost": 0, "sessions": sessions, "api_calls": calls}));
        ti += input; to += output; tc += cost; ts += sessions; ta += calls;
    }
    let by_model = json!([{"model": "mock-model", "input_tokens": ti, "output_tokens": to, "estimated_cost": tc, "sessions": ts, "api_calls": ta}]);
    Json(json!({"daily": daily, "by_model": by_model, "by_task": [], "period_days": days, "skills": [], "tools": [],
        "totals": {"total_input": ti, "total_output": to, "total_cache_read": ti / 3, "total_reasoning": 0, "total_estimated_cost": tc,
            "total_actual_cost": 0, "total_sessions": ts, "total_api_calls": ta}})).into_response()
}

async fn image_upload(State(m): State<Mock>, h: HeaderMap, Json(body): Json<Value>) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let du = body["data_url"].as_str().unwrap_or_default();
    let (head, b64) = du.split_once(";base64,").unwrap_or(("", ""));
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap_or_default();
    let name = body["filename"].as_str().unwrap_or("image.png");
    let path = format!("/home/hermes/uploads/{}-{name}", &uuid::Uuid::new_v4().to_string()[..8]);
    let n = bytes.len();
    m.0.lock().unwrap().files.insert(path.clone(), bytes);
    Json(json!({"path": path, "mime_type": head.trim_start_matches("data:"), "bytes": n})).into_response()
}

async fn download(State(m): State<Mock>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = q.get("path").cloned().unwrap_or_default();
    match m.0.lock().unwrap().files.get(&p) {
        Some(b) => ([("content-type", "application/octet-stream")], b.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A third-party logo host for the logo fetcher tests (not a Hermes endpoint).
async fn logo(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-tiny-";
    match name.as_str() {
        "ok.png" => ([(axum::http::header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response(),
        "big.png" => ([(axum::http::header::CONTENT_TYPE, "image/png")], vec![0u8; 2 * 1024 * 1024]).into_response(),
        "page.png" => ([(axum::http::header::CONTENT_TYPE, "text/html")], b"<script>".to_vec()).into_response(),
        _ => axum::http::StatusCode::NOT_FOUND.into_response(),
    }
}

async fn media(State(m): State<Mock>, h: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !authed(&m, &h) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let p = q.get("path").cloned().unwrap_or_default();
    match m.0.lock().unwrap().files.get(&p) {
        Some(b) => Json(json!({"data_url": format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(b))})).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws(State(m): State<Mock>, Query(q): Query<HashMap<String, String>>, up: WebSocketUpgrade) -> Response {
    {
        let mut i = m.0.lock().unwrap();
        // Single-use tickets (`_consume_ws_ticket`) or the deployment session token.
        let ticket_ok = q.get("ticket").and_then(|t| i.login.tickets.iter().position(|x| x == t)).map(|p| i.login.tickets.remove(p)).is_some();
        let token_ok = (!i.token.is_empty() && q.get("token") == Some(&i.token)) || (i.token.is_empty() && i.basic.is_none());
        if !ticket_ok && !token_ok {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    up.max_message_size(400 << 20).on_upgrade(move |s| socket(m, s))
}

fn event(kind: &str, sid: &str, payload: Value) -> String {
    json!({"jsonrpc": "2.0", "method": "event", "params": {"type": kind, "session_id": sid, "payload": payload}}).to_string()
}

async fn socket(m: Mock, s: WebSocket) {
    let (mut sink, mut stream) = s.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let mut kill_w = m.0.lock().unwrap().kill.as_ref().map(|k| k.subscribe());
    tokio::spawn(async move {
        loop {
            let t = match &mut kill_w {
                Some(k) => tokio::select! {
                    t = rx.recv() => t,
                    _ = k.recv() => {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                },
                None => rx.recv().await,
            };
            let Some(t) = t else { break };
            if sink.send(Message::Text(t.into())).await.is_err() {
                break;
            }
        }
    });
    let mut ready = json!({"heartbeat": true, "change_events": true, "replay_epoch": "mock"});
    // The mode is fixed per socket (`_authority_connection` at accept).
    let (authority, ready_delay) = {
        let i = m.0.lock().unwrap();
        (i.authority, i.ready_delay_ms)
    };
    if authority {
        ready["session_authority"] = json!(true);
    }
    let ready_frame = event("gateway.ready", "", ready);
    if ready_delay == 0 {
        let _ = tx.send(ready_frame);
    } else {
        let tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ready_delay)).await;
            let _ = tx.send(ready_frame);
        });
    }
    let mut kill = m.0.lock().unwrap().kill.as_ref().map(|k| k.subscribe());
    loop {
        let next = match &mut kill {
            Some(k) => tokio::select! {
                n = stream.next() => n,
                _ = k.recv() => None,
            },
            None => stream.next().await,
        };
        let Some(Ok(msg)) = next else { break };
        let Message::Text(t) = msg else { continue };
        let Ok(f) = serde_json::from_str::<Value>(&t) else { continue };
        // A response to one of our server→client requests.
        if f.get("method").is_none() {
            if let Some(id) = f.get("id").and_then(Value::as_str) {
                let mut i = m.0.lock().unwrap();
                for v in i.open_reqs.values_mut() {
                    v.retain(|r| r["id"].as_str() != Some(id));
                }
                if let Some(w) = i.pending_approvals.remove(id) {
                    let _ = w.send(f.get("result").cloned().unwrap_or(Value::Null));
                }
            }
            continue;
        }
        let id = f["id"].clone();
        let method = f["method"].as_str().unwrap_or_default().to_owned();
        let p = f.get("params").cloned().unwrap_or(json!({}));
        let reply = match checked(&m, &method, &p, authority) {
            Err((code, msg, data)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg, "data": data}}),
            Ok(()) => match dispatch(&m, &method, &p, &tx, authority) {
                Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
                Err((code, msg)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}}),
            },
        };
        let _ = tx.send(reply.to_string());
    }
}

const CONNECTORS: &[(&str, &str, &str, &str)] = &[
    ("github", "GitHub", "developer", "Repos, issues and pull requests"),
    ("linear", "Linear", "developer", "Plan and track issues"),
    ("gmail", "Gmail", "productivity", "Read, search and draft email"),
    ("google_calendar", "Google Calendar", "productivity", "Events and availability"),
    ("notion", "Notion", "productivity", "Pages and databases"),
    ("slack", "Slack", "communication", "Channels and messages"),
    ("figma", "Figma", "design", "Files, frames and comments"),
    ("stripe", "Stripe", "finance", "Payments and customers"),
];
const MCP: &[(&str, &str, &[&str])] = &[
    ("filesystem", "Read and write files in allowed folders", &[]),
    ("playwright", "Drive a headless browser", &[]),
    ("brave_search", "Web search through Brave", &["BRAVE_API_KEY"]),
    ("postgres", "Query a Postgres database", &["DATABASE_URL"]),
];
const TOOLSETS: &[(&str, &str, u32)] = &[
    ("web", "Search and fetch web pages", 3),
    ("terminal", "Run shell commands", 2),
    ("file", "Read and edit files", 4),
    ("image_gen", "Generate images", 1),
    ("memory", "Long-term memory", 2),
];

/// Where a mock session came from, by its id's suffix.
fn source_of(id: &str) -> &'static str {
    if id.ends_with("-tg") {
        "telegram"
    } else if id.ends_with("-cron") {
        "cron"
    } else if id.ends_with("-tui") {
        "tui"
    } else {
        "cli"
    }
}

fn summary(id: &str, s: &Session) -> Value {
    let last = s.messages.last().and_then(|m| m["content"].as_str()).unwrap_or_default();
    let preview: String = last.chars().take(80).collect();
    json!({"id": id, "title": s.title, "preview": preview, "started_at": s.messages.first().map(|m| m["timestamp"].clone()), "last_active": s.last_active, "message_count": s.messages.len()})
}

/// The checks a request meets before any handler: the release serves the method, and the
/// params fit its contract. Refusals are recorded for the tests.
fn checked(m: &Mock, method: &str, p: &Value, authority: bool) -> Result<(), Refusal> {
    let mut i = m.0.lock().unwrap();
    if i.unserved.iter().any(|u| u == method) {
        return Err((-32601, format!("unknown method: {method}"), json!({"reason": "unknown_method"})));
    }
    if let Err(e) = check_params(method, p, authority, i.legacy, &i.forbidden) {
        let detail = match e.2.get("fields") {
            Some(f) => format!("{}: {f}", e.1),
            None => e.1.clone(),
        };
        i.refusals.push((method.to_owned(), detail));
        return Err(e);
    }
    Ok(())
}

fn dispatch(m: &Mock, method: &str, p: &Value, tx: &mpsc::UnboundedSender<String>, authority: bool) -> Result<Value, (i64, String)> {
    let mut i = m.0.lock().unwrap();
    i.handled.push(method.to_owned());
    // The authority serves session verbs itself; methods only the plain dispatcher had are
    // unknown there (`ws_legacy_fallback`: the session namespace never falls back).
    if authority && matches!(method, "session.title" | "session.compress") {
        return Err((-32601, format!("unknown method: {method}")));
    }
    let profile = p["profile"].as_str().unwrap_or("default").to_owned();
    match method {
        "client.capabilities" => Ok(json!({"server_requests": ["approval", "clarify", "sudo", "secret"]})),
        "gateway.ping" => Ok(json!({"pong": true})),
        "profiles.list" => {
            let rows: Vec<Value> = i
                .profiles
                .iter()
                .map(|pr| {
                    // `_canonical_session_row`: `id` is the registry row, `resolved_id` the live tip, and
                    // the preview, title and times describe the tip.
                    let canon = i.canonical_root(&pr.name).map(|root| {
                        let tip = i.tip(&root);
                        let t = &i.sessions[&tip];
                        let mut v = summary(&tip, t);
                        v["id"] = json!(root);
                        v["resolved_id"] = json!(tip);
                        v["root_title"] = json!("Bot Chat");
                        v
                    });
                    let last = i
                        .sessions
                        .iter()
                        .filter(|(id, s)| s.profile == pr.name && i.root(id) != i.canonical_root(&pr.name).unwrap_or_default())
                        .max_by(|a, b| a.1.last_active.total_cmp(&b.1.last_active))
                        .map(|(id, s)| summary(id, s));
                    json!({"name": pr.name, "path": format!("~/.hermes/profiles/{}", pr.name), "is_default": pr.name == "default", "model": "mock-model", "provider": "mock",
                        "description": pr.description, "display_name": "", "skill_count": 3, "previous_names": [],
                        "last_session": last, "canonical_session": canon, "ui_meta": pr.ui_meta, "ui_meta_revisions": {"hermes-bots": pr.revision}, "has_avatar": false})
                })
                .collect();
            Ok(json!({"profiles": rows, "bot_mode_protocol": true, "install_id": "mock-install"}))
        }
        "profiles.configure" => {
            let name = p["name"].as_str().unwrap_or_default();
            let pr = i.profiles.iter_mut().find(|x| x.name == name).ok_or((4004, "unknown profile".to_owned()))?;
            if let Some(exp) = p.pointer("/ui_meta_expected_revisions/hermes-bots").and_then(Value::as_i64) {
                if exp != pr.revision {
                    return Ok(json!({"ok": false, "applied": {"ui_meta_conflicts": {"hermes-bots": {"expected": exp, "actual": pr.revision}}}}));
                }
            }
            if let Some(d) = p["description"].as_str() {
                pr.description = d.to_owned();
            }
            if let Some(new) = p.pointer("/ui_meta/hermes-bots") {
                pr.ui_meta["hermes-bots"] = new.clone();
                pr.revision += 1;
            }
            Ok(json!({"ok": true, "applied": {"ui_meta": true, "ui_meta_revisions": {"hermes-bots": pr.revision}}}))
        }
        "profiles.create" => {
            let name = p["name"].as_str().unwrap_or_default().to_owned();
            if name.is_empty() || i.profiles.iter().any(|x| x.name == name) {
                return Err((-32602, "profile exists or invalid name".into()));
            }
            i.profiles.push(Profile { name: name.clone(), description: p["description"].as_str().unwrap_or_default().into(), ui_meta: json!({}), revision: 0 });
            Ok(json!({"ok": true, "name": name, "soul_set": false}))
        }
        "session.list" => {
            // `methods_session` (plain) / `CanonicalListParams` (authority, whose default scope is
            // "live": only sessions attached right now).
            let row = |i: &Inner, id: &str, tip: &str| {
                let (s, t) = (&i.sessions[id], &i.sessions[tip]);
                let preview: String = t
                    .messages
                    .last()
                    .and_then(|m| m["content"].as_str())
                    .unwrap_or_default()
                    .chars()
                    .take(80)
                    .collect();
                let source = source_of(id);
                let started = s
                    .messages
                    .first()
                    .map(|m| m["timestamp"].clone())
                    .unwrap_or(json!(0));
                json!({"id": id, "title": s.title, "preview": preview, "started_at": started, "message_count": t.messages.len(), "source": source, "_lineage_root_id": Value::Null})
            };
            if let Some(title) = p["title"].as_str() {
                // Exact-title lookup: the registry row, with the lineage's live tip as `resolved_id`.
                let found = i
                    .sessions
                    .iter()
                    .find(|(_, s)| s.profile == profile && s.title == title && s.parent.is_none())
                    .map(|(id, _)| id.clone());
                let Some(id) = found else {
                    return Ok(json!({"sessions": []}));
                };
                let tip = i.tip(&id);
                let mut r = row(&i, &id, &tip);
                r["resolved_id"] = json!(tip);
                return Ok(json!({"sessions": [r]}));
            }
            let live_only = authority && p["scope"].as_str().unwrap_or("live") == "live";
            let limit = p["limit"].as_u64().unwrap_or(200) as usize;
            let mut tips: Vec<(String, f64)> = i
                .sessions
                .iter()
                .filter(|(id, s)| s.profile == profile && i.tip(id) == **id)
                .filter(|(id, _)| {
                    p["include_hidden"].as_bool().unwrap_or(false)
                        || i.sessions[&i.root(id)].title != "Bot Chat"
                })
                .filter(|(id, _)| !live_only || i.attached.contains_key(*id))
                .map(|(id, s)| (id.clone(), s.last_active))
                .collect();
            // Most recently active first; the row itself carries no activity time.
            tips.sort_by(|a, b| b.1.total_cmp(&a.1));
            // A compressed chain is listed once, as its tip (an untitled continuation stays untitled),
            // naming its root in `_lineage_root_id`.
            let rows: Vec<Value> = tips
                .iter()
                .take(limit)
                .map(|(id, _)| {
                    let mut r = row(&i, id, id);
                    let root = i.root(id);
                    if root != *id {
                        r["_lineage_root_id"] = json!(root);
                    }
                    r
                })
                .collect();
            Ok(json!({"sessions": rows}))
        }
        "session.create" => {
            let retry = p
                .get(if authority {
                    "request_id"
                } else {
                    "idempotency_key"
                })
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(stored) = retry.as_ref().and_then(|k| i.created.get(k)).cloned() {
                let rt = i.attached.get(&stored).cloned().unwrap_or_default();
                return Ok(
                    json!({"session_id": rt, "stored_session_id": stored, "info": {"title": i.sessions[&stored].title, "model": "mock-model"}, "revision": 1, "execution_generation": 1}),
                );
            }
            let stored = format!("s-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            let rt = if authority {
                stored.clone()
            } else {
                format!("rt-{}", &uuid::Uuid::new_v4().to_string()[..8])
            };
            // Like the real plain gateway, the title arrives through session.title afterwards.
            i.sessions.insert(
                stored.clone(),
                Session {
                    parent: None,
                    profile: profile.clone(),
                    title: String::new(),
                    messages: vec![],
                    last_active: now(),
                },
            );
            i.runtime.insert(rt.clone(), stored.clone());
            i.sinks.insert(rt.clone(), tx.clone());
            i.attached.insert(stored.clone(), rt.clone());
            if let Some(k) = retry {
                i.created.insert(k, stored.clone());
            }
            Ok(
                json!({"session_id": rt, "stored_session_id": stored, "info": {"title": "", "model": "mock-model"}, "revision": 1, "execution_generation": 1}),
            )
        }
        "session.mutate" if authority => {
            let rt = p["session_id"].as_str().unwrap_or_default();
            let stored = i
                .runtime
                .get(rt)
                .cloned()
                .ok_or((4001, "not_found".to_owned()))?;
            match p["operation"].as_str().unwrap_or_default() {
                "rename" => {
                    let title = p
                        .pointer("/payload/title")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    i.sessions.get_mut(&stored).unwrap().title = title.clone();
                    Ok(
                        json!({"session_id": stored, "operation": "rename", "revision": 2, "title": title}),
                    )
                }
                "compress" => {
                    if p.get("expected_generation").is_none_or(Value::is_null) {
                        return Err((4001, "stale_generation".into()));
                    }
                    let row = i.next_row;
                    i.next_row += 1;
                    i.sessions.get_mut(&stored).unwrap().messages.push(json!({"id": row, "role": "user", "content": "", "display_kind": "context_compaction", "display_metadata": {"display_text": "Context compacted"}, "timestamp": now()}));
                    Ok(
                        json!({"session_id": stored, "operation": "compress", "revision": 2, "status": "compressed"}),
                    )
                }
                other => Err((4001, format!("unsupported operation {other}"))),
            }
        }
        "session.title" => {
            let rt = p["session_id"].as_str().unwrap_or_default();
            let stored = i.runtime.get(rt).cloned().ok_or((4004, "unknown session".to_owned()))?;
            if let Some(s) = i.sessions.get_mut(&stored) {
                s.title = p["title"].as_str().unwrap_or_default().to_owned();
            }
            Ok(json!({"ok": true}))
        }
        "session.resume" => {
            // Resuming a session id opens THAT session: a compressed root replays its own,
            // pre-compaction transcript (the client must ask for the tip).
            let stored = p["session_id"].as_str().unwrap_or_default().to_owned();
            if !i.sessions.contains_key(&stored) {
                return Err((4004, "session not found".into()));
            }
            let rt = if authority {
                stored.clone()
            } else {
                format!("rt-{}", &uuid::Uuid::new_v4().to_string()[..8])
            };
            i.runtime.insert(rt.clone(), stored.clone());
            i.sinks.insert(rt.clone(), tx.clone());
            i.attached.insert(stored.clone(), rt.clone());
            let title = i.sessions[&stored].title.clone();
            let inflight = i.inflight.get(&stored).cloned().unwrap_or(Value::Null);
            let open: Vec<Value> = i
                .open_reqs
                .get(&stored)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|mut r| {
                    r["params"]["session_id"] = json!(rt);
                    r
                })
                .collect();
            Ok(
                json!({"session_id": rt, "stored_session_id": stored, "running": !inflight.is_null(), "info": {"title": title, "model": "mock-model"}, "inflight": inflight, "open_requests": open, "revision": 1, "execution_generation": 1}),
            )
        }
        "session.interrupt" => Ok(json!({"ok": true})),
        "profiles.describe" => {
            let name = p["name"].as_str().unwrap_or(&profile).to_owned();
            let pr = i.profiles.iter().find(|x| x.name == name).ok_or((4004, "unknown profile".to_owned()))?;
            let toolsets: Vec<Value> = TOOLSETS.iter().map(|(n, d, c)| json!({"name": n, "label": n, "description": d, "tool_count": c, "enabled": !i.toolsets_off.iter().any(|x| x == n)})).collect();
            Ok(json!({"name": pr.name, "description": pr.description, "soul": format!("You are {}, a helpful bot.", pr.name), "model": {"provider": "mock", "default": "mock-model"},
                "skills": [{"name": "arxiv", "enabled": true}, {"name": "git_workflow", "enabled": true}], "toolsets": toolsets, "toolsets_pinned": false,
                "mcp_servers": i.mcp_installed.iter().map(|n| json!({"name": n, "enabled": true, "transport": "stdio"})).collect::<Vec<_>>()}))
        }
        "connectors.catalog" => Ok(json!({"connectors": CONNECTORS.iter().map(|(slug, name, cat, desc)| json!({"slug": slug, "name": name, "category": cat, "description": desc, "logo_url": null})).collect::<Vec<_>>()})),
        "connectors.list" => Ok(json!({"available": true, "connectors": i.connected.iter().map(|c| json!({"connector": c, "enabled": true, "connected": true, "connection_status": "active", "status_reason": null, "gateway_disabled_tools": []})).collect::<Vec<_>>()})),
        "connectors.connect" => {
            let slug = p["connectors"][0].as_str().unwrap_or_default().to_owned();
            if !CONNECTORS.iter().any(|c| c.0 == slug) {
                return Err((4004, "CONNECTOR_NOT_FOUND".into()));
            }
            if !i.connected.contains(&slug) {
                i.connected.push(slug.clone());
            }
            Ok(json!({"op_id": "op-1", "seq": 1, "deadline_at": now() + 600.0, "settled": false, "status": "initiated",
                "targets": [{"name": slug, "kind": "connector", "action": "authorize", "state": "initiated", "connect_url": format!("https://connect.example.invalid/{slug}")}]}))
        }
        "connectors.tools" => {
            let slug = p["slug"].as_str().unwrap_or_default();
            Ok(json!({"connector": slug, "toolkit_version": "1", "etag": "e", "fetched_at": now(), "source": "cache", "stale": false,
                "tools": [{"slug": format!("{slug}_search"), "name": "Search", "description": "Search items", "facet": "read", "hints": [], "categories": [], "no_auth": false, "deprecated": false},
                          {"slug": format!("{slug}_create"), "name": "Create", "description": "Create an item", "facet": "write", "hints": [], "categories": [], "no_auth": false, "deprecated": false}]}))
        }
        "mcp.catalog" => Ok(json!({"servers": MCP.iter().map(|(n, d, req)| json!({"name": n, "description": d, "connector_slug": null, "installed": i.mcp_installed.iter().any(|x| x == n), "enabled": true, "requires": req, "transport": "stdio"})).collect::<Vec<_>>()})),
        "mcp.servers.add" => {
            let name = p["preset"].as_str().or(p["name"].as_str()).unwrap_or_default().to_owned();
            if !i.mcp_installed.contains(&name) {
                i.mcp_installed.push(name.clone());
            }
            Ok(json!({"ok": true, "name": name, "server": {"name": name, "transport": "stdio", "args": [], "env": [], "enabled": true, "source": "config"}}))
        }
        "mcp.servers.remove" => {
            let name = p["name"].as_str().unwrap_or_default().to_owned();
            i.mcp_installed.retain(|x| *x != name);
            Ok(json!({"ok": true, "name": name}))
        }
        "toolsets.list" => Ok(json!({"toolsets": TOOLSETS.iter().map(|(n, d, c)| json!({"name": n, "description": d, "tool_count": c, "enabled": !i.toolsets_off.iter().any(|x| x == n)})).collect::<Vec<_>>()})),
        "tools.configure" => {
            let enable = p["action"] == "enable";
            for n in p["names"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                i.toolsets_off.retain(|x| x != n);
                if !enable {
                    i.toolsets_off.push(n.to_owned());
                }
            }
            let enabled: Vec<&str> = TOOLSETS.iter().map(|t| t.0).filter(|n| !i.toolsets_off.iter().any(|x| x == n)).collect();
            Ok(json!({"changed": p["names"], "enabled_toolsets": enabled, "missing_servers": [], "reset": false, "unknown": []}))
        }
        "commands.catalog" => {
            let session = vec![json!(["/status", "Show session, model, token, and context info"]), json!(["/new", "Start a new session (fresh session ID + history)"]),
                json!(["/compress", "Manually compress conversation context"]), json!(["/title", "Set a title for the current session"]), json!(["/retry", "Retry the last message (resend to agent)"])];
            let info = vec![json!(["/help", "Show available commands"]), json!(["/usage", "Show token usage and rate limits for the current session"]),
                json!(["/version", "Show Hermes version"]), json!(["/whoami", "Show your identity and access on this surface"])];
            let config = vec![json!(["/model", "Switch model (session-scoped; --global to persist)"]), json!(["/personality", "Set a predefined personality"])];
            let mut pairs: Vec<Value> = session.iter().chain(info.iter()).chain(config.iter()).cloned().collect();
            pairs.push(json!(["/arxiv", "Search and summarise arXiv papers"]));
            Ok(json!({"pairs": pairs, "canon": {}, "sub": {}, "commands": {"/model": {"argument_mode": "mixed", "desktop": "hidden"}},
                "categories": [{"name": "Session", "pairs": session}, {"name": "Info", "pairs": info}, {"name": "Configuration", "pairs": config}],
                "skills": {"/arxiv": {"usage": 3, "origin": "hub"}}, "skill_count": 1, "warning": "", "plugin_discovery_error": "", "plugin_commands": {}}))
        }
        "slash.exec" => {
            let cmd = p["command"].as_str().unwrap_or_default().trim().to_owned();
            let name = cmd.split_whitespace().next().unwrap_or_default();
            let rt = p["session_id"].as_str().unwrap_or_default();
            let stored = i.runtime.get(rt).cloned().unwrap_or_default();
            let n = i.sessions.get(&stored).map(|s| s.messages.len()).unwrap_or(0);
            match name {
                "/status" => Ok(json!({"type": "exec", "output": format!("Session {stored}\nModel: mock-model (mock)\nMessages: {n}\nContext: 4,210 / 200,000 tokens (2%)")})),
                "/version" => Ok(json!({"type": "exec", "output": "Hermes Agent (mock)"})),
                "/whoami" => Ok(json!({"type": "exec", "output": "Local owner · session:read session:control session:submit"})),
                "/help" | "/commands" => Ok(json!({"type": "exec", "output": "/status  /version  /whoami  /title  /help\nType / in the composer to see everything."})),
                "/arxiv" => Ok(json!({"type": "skill", "name": "arxiv", "message": format!("Use the arxiv skill: {}", cmd.trim_start_matches("/arxiv").trim()), "display": cmd})),
                _ => Err((4090, format!("unsupported_command: {name} is not available over this connection"))),
            }
        }
        "skills.manage" => Ok(json!({"skills": {"research": ["arxiv", "paper_notes"], "software": ["git_workflow", "code_review"]}})),
        "session.compress" => {
            let rt = p["session_id"].as_str().unwrap_or_default();
            let stored = i.runtime.get(rt).cloned().ok_or((4004, "unknown session".to_owned()))?;
            let row = i.next_row;
            i.next_row += 1;
            if let Some(s) = i.sessions.get_mut(&stored) {
                s.messages.push(json!({"id": row, "role": "user", "content": "", "display_kind": "context_compaction", "display_metadata": {"display_text": "Context compacted"}, "timestamp": now()}));
            }
            Ok(json!({"ok": true, "folded": 4}))
        }
        "image.attach_bytes" => {
            let rt = p["session_id"].as_str().unwrap_or_default().to_owned();
            if !i.runtime.contains_key(&rt) {
                return Err((4004, "unknown session".into()));
            }
            let name = p["filename"].as_str().unwrap_or("image.png");
            let path = format!(
                "/home/hermes/uploads/{}-{name}",
                &uuid::Uuid::new_v4().to_string()[..8]
            );
            i.staged
                .entry(rt)
                .or_default()
                .push(json!({"path": path, "mime": "image/png"}));
            Ok(json!({"attached": true, "path": path}))
        }
        "file.attach" => {
            let name = p["name"].as_str().unwrap_or("file").to_owned();
            let path = format!("/home/hermes/uploads/{name}");
            let n = p["data_url"].as_str().map(|d| d.len()).unwrap_or(0);
            i.files.insert(path.clone(), vec![0; n.min(16)]);
            Ok(json!({"attached": true, "path": path, "ref_text": format!("@file:{path}")}))
        }
        "prompt.submit" => {
            // The authority's admission is keyed by the client's id (`submission_id`, or the
            // older `input_id`): `AuthorityConnection.submission` refuses a submit without one.
            let has_id = |k: &str| p[k].as_str().is_some_and(|s| !s.is_empty());
            if authority && !has_id("submission_id") && !has_id("input_id") {
                i.refusals.push((method.to_owned(), "invalid_params: no submission_id".into()));
                return Err((4001, "invalid_params".into()));
            }
            let rt = p["session_id"].as_str().unwrap_or_default().to_owned();
            let stored = i.runtime.get(&rt).cloned().ok_or((4004, "unknown session".to_owned()))?;
            let text = p["text"].as_str().unwrap_or_default().to_owned();
            let row = i.next_row;
            i.next_row += 1;
            let mut user_row = json!({"id": row, "role": "user", "content": text, "timestamp": now()});
            // The authority's admission names its attachments; a plain socket's prompt takes the
            // images staged on the session.
            let staged = i.staged.remove(&rt).unwrap_or_default();
            match p.get("attachments") {
                Some(a) => user_row["attachments"] = a.clone(),
                None if !staged.is_empty() => user_row["attachments"] = Value::Array(staged),
                None => {}
            }
            let s = i.sessions.get_mut(&stored).unwrap();
            s.messages.push(user_row);
            s.last_active = now();
            let pace = i.pace_ms;
            drop(i);
            let m2 = m.clone();
            tokio::spawn(async move { run_turn(m2, rt, stored, text, pace).await });
            Ok(json!({"admission_id": uuid::Uuid::new_v4().to_string(), "status": "accepted", "user_row_id": row}))
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    }
}

fn push_row(m: &Mock, stored: &str, role: &str, content: &str, extra: Value) {
    let mut i = m.0.lock().unwrap();
    let row = i.next_row;
    i.next_row += 1;
    if let Some(s) = i.sessions.get_mut(stored) {
        let mut r = json!({"id": row, "role": role, "content": content, "timestamp": now()});
        if let (Some(o), Some(e)) = (r.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        s.messages.push(r);
        s.last_active = now();
    }
}

/// Send an event to whichever socket `stored` is attached to now.
fn emit(m: &Mock, stored: &str, kind: &str, payload: Value) {
    let i = m.0.lock().unwrap();
    let Some(rt) = i.attached.get(stored) else { return };
    if let Some(tx) = i.sinks.get(rt) {
        let _ = tx.send(event(kind, rt, payload));
    }
}

async fn stream_reply(m: &Mock, stored: &str, reply: &str, pace: u64) {
    for chunk in reply.split_inclusive(' ') {
        if let Some(t) = m.0.lock().unwrap().inflight.get_mut(stored) {
            let so_far = t["assistant"].as_str().unwrap_or_default().to_owned() + chunk;
            t["assistant"] = json!(so_far);
        }
        emit(m, stored, "message.delta", json!({"text": chunk}));
        if pace > 0 {
            tokio::time::sleep(Duration::from_millis(pace)).await;
        }
    }
}

async fn run_turn(m: Mock, rt: String, stored: String, text: String, pace: u64) {
    let _ = &rt;
    m.0.lock().unwrap().inflight.insert(stored.clone(), json!({"user": text, "assistant": "", "streaming": true}));
    let lower = text.to_lowercase();
    emit(&m, &stored, "message.start", json!({}));
    let mut tool_calls = vec![];
    if lower.contains("search") || lower.contains("look up") {
        emit(&m, &stored, "tool.start", json!({"tool_id": "t1", "name": "web_search", "context": "hermes agent gpui", "labels": [{"text": "Searching the web"}]}));
        tokio::time::sleep(Duration::from_millis(pace * 8)).await;
        emit(&m, &stored, "tool.complete", json!({"tool_id": "t1", "name": "web_search", "duration_s": 1.2, "summary": "5 results"}));
        tool_calls.push(json!({"id": "t1", "function": {"name": "web_search"}}));
    }
    if lower.contains("routine") || lower.contains("remind") {
        let args = json!({"action": "create", "name": "News watch", "schedule": "0 8 * * 1-5", "prompt": "Summarise agent news"});
        emit(&m, &stored, "tool.start", json!({"tool_id": "cj2", "name": "cronjob", "args": args, "context": "create News watch"}));
        tokio::time::sleep(Duration::from_millis(pace * 6)).await;
        emit(&m, &stored, "tool.complete", json!({"tool_id": "cj2", "name": "cronjob", "duration_s": 0.3, "summary": "job j-news scheduled"}));
        tool_calls.push(json!({"id": "cj2", "function": {"name": "cronjob", "arguments": args.to_string()}}));
    }
    if lower.contains("delegate") || lower.contains("in parallel") {
        for (n, goal) in ["Survey crates.io for GPUI widgets", "Benchmark idle RAM"].iter().enumerate() {
            let id = format!("sa{n}");
            emit(&m, &stored, "subagent.start", json!({"subagent_id": id, "goal": goal, "task_count": 2, "task_index": n}));
            emit(&m, &stored, "subagent.tool", json!({"subagent_id": id, "goal": goal, "tool_name": "terminal"}));
        }
        tokio::time::sleep(Duration::from_millis(pace * 12)).await;
        for (n, goal) in ["Survey crates.io for GPUI widgets", "Benchmark idle RAM"].iter().enumerate() {
            emit(&m, &stored, "subagent.complete", json!({"subagent_id": format!("sa{n}"), "goal": goal, "status": "completed", "summary": "done"}));
        }
        tool_calls.push(json!({"id": "d1", "function": {"name": "delegate_task"}}));
    }
    if lower.contains("rm -rf") || lower.contains("dangerous") {
        let (w, r) = tokio::sync::oneshot::channel();
        let id = format!("srq-{}", &uuid::Uuid::new_v4().to_string()[..6]);
        {
            let mut i = m.0.lock().unwrap();
            i.pending_approvals.insert(id.clone(), w);
            let cur = i.attached.get(&stored).cloned().unwrap_or(rt.clone());
            let frame = json!({"jsonrpc": "2.0", "id": id, "method": "approval", "params": {"session_id": cur, "request_id": id, "command": "rm -rf build/", "description": "Delete the build directory"}});
            i.open_reqs.entry(stored.clone()).or_default().push(json!({"id": id, "method": "approval", "params": frame["params"].clone()}));
            if let Some(tx) = i.sinks.get(&cur) {
                let _ = tx.send(frame.to_string());
            }
        }
        let choice = tokio::time::timeout(Duration::from_secs(120), r).await.ok().and_then(|r| r.ok()).and_then(|v| v["choice"].as_str().map(str::to_owned)).unwrap_or("deny".into());
        let reply = if choice == "deny" { "Okay, I won't run it." } else { "Removed `build/`." };
        stream_reply(&m, &stored, reply, pace).await;
        push_row(&m, &stored, "assistant", reply, json!({}));
        m.0.lock().unwrap().inflight.remove(&stored);
        emit(&m, &stored, "message.complete", json!({"text": reply, "status": "complete"}));
        return;
    }
    let mention = text.find("message_agent target: \"").map(|i| text[i + 23..].split('"').next().unwrap_or("").to_owned());
    let reply = if let Some(target) = &mention {
        format!("I've messaged **@{target}** with `message_agent`; I'll pass on the reply when it lands.")
    } else if lower.contains("chart") || lower.contains("image") {
        "Here's the chart you asked for:\n\nMEDIA:/home/hermes/out/chart.png\n\nAnd the full report:\n\nMEDIA:/home/hermes/out/report.pdf".to_owned()
    } else if lower.contains("code") {
        "Sure, here's a tiny example:\n\n```rust\nfn main() {\n    println!(\"hello from hermes\");\n}\n```\n\n- fast\n- **native**\n- no webview".to_owned()
    } else if lower.contains("@file:") {
        "Got the file, thanks. It looks fine.".to_owned()
    } else {
        let first = text.lines().next().unwrap_or_default();
        format!("You said: *{}*. This is the mock Hermes streaming a reply word by word so the client can be tested end to end.", first.chars().take(60).collect::<String>())
    };
    stream_reply(&m, &stored, &reply, pace).await;
    push_row(&m, &stored, "assistant", &reply, if tool_calls.is_empty() { json!({}) } else { json!({"tool_calls": tool_calls}) });
    m.0.lock().unwrap().inflight.remove(&stored);
    emit(&m, &stored, "message.complete", json!({"text": reply, "status": "complete", "usage": {"input": 10, "output": 20}}));
    // Bot-to-bot: the teammate's reply comes back into this Bot Chat as an attributed
    // notice and starts a new turn (docs: Bot-to-bot messaging).
    if let Some(target) = mention {
        tokio::time::sleep(Duration::from_millis(pace * 20)).await;
        let title = {
            let i = m.0.lock().unwrap();
            i.profiles.iter().find(|p| p.name == target).and_then(|p| p.ui_meta.pointer("/hermes-bots/title").and_then(Value::as_str).map(str::to_owned)).unwrap_or(target.clone())
        };
        let handle = slug(&title);
        let notice = format!("Message from 🤖 {title} (@{handle}): Happy to help, I've picked this up and will report back within the hour.");
        push_row(&m, &stored, "user", &notice, json!({}));
        emit(&m, &stored, "message.start", json!({"text": notice}));
        let r2 = format!("{title} confirmed they're on it.");
        stream_reply(&m, &stored, &r2, pace).await;
        push_row(&m, &stored, "assistant", &r2, json!({}));
        emit(&m, &stored, "message.complete", json!({"text": r2, "status": "complete"}));
    }
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for w in s.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&w.to_lowercase());
    }
    out
}

/// A small valid PNG (gradient with bars) built with stored deflate blocks.
pub fn demo_png(w: u32, h: u32) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut c = 0xffff_ffffu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend(&c);
        out.extend(crc(&c).to_be_bytes());
    }
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let bar = x / 40;
            let height = 30 + (bar * 37 % 120);
            let in_bar = x % 40 > 8 && h - y < height;
            let (r, g, b) = if in_bar { (124u8, 92u8, 255u8) } else { (30 + (y * 20 / h) as u8, 30, 38 + (x * 30 / w) as u8) };
            raw.extend([r, g, b]);
        }
    }
    let mut z = vec![0x78, 0x01];
    for (n, block) in raw.chunks(65535).enumerate() {
        let last = (n + 1) * 65535 >= raw.len();
        z.push(last as u8);
        z.extend((block.len() as u16).to_le_bytes());
        z.extend((!(block.len() as u16)).to_le_bytes());
        z.extend(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}
