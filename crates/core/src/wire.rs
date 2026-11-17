//! Every gateway request this client sends, shaped per socket mode, and listing rows.
//!
//! A Hermes gateway WebSocket is either a **plain gateway** socket (`server.dispatch`,
//! contracts in `tui_gateway/contracts/*.py`) or a **session-authority** socket (`gateway.ready`
//! says `session_authority: true`; contracts in `tui_gateway/contracts/canonical*.py`). Since
//! Hermes v2026.9.14 both refuse keys a method's contract does not declare:
//! * plain: `4000 invalid params for <method>: <key>: Extra inputs are not permitted — the client
//!   and the Hermes backend are out of sync (different versions); run `hermes update` and restart
//!   both` (`tui_gateway/contracts/registry.py::validate_params`, first unknown key only);
//! * authority: `4001 invalid_params`, `data.fields` listing every unknown or missing key
//!   (`gateway/session_controls.py::dispatch`, `registry.canonical_param_problems`).
//!
//! So the request builders below send only keys the mode's contract declares, checked against
//! hermes-agent v2026.9.14, v0.21.6, v0.22.0, v0.22.1, v0.22.5 and v0.23.2 (the `contracts` test
//! module pins them). Keys a request can do without are listed in [`droppable`]: if a Hermes
//! release refuses one, [`crate::gateway::Gateway::call_compat`] sends the request again
//! without it instead of failing. Any other refusal is this client's bug, and [`explain`] says so
//! instead of passing on Hermes's "run `hermes update`" advice.

use serde_json::{json, Value};

use crate::gateway::{RpcError, METHOD_NOT_FOUND};
use crate::roster::BOT_CHAT_TITLE;

/// Oldest Hermes release whose contracts every request here was checked against. Earlier
/// releases ignore unknown keys, so they may work, but they are not checked.
pub const MIN_HERMES: &str = "v2026.9.14";
/// Newest Hermes release the contracts were checked against.
pub const CHECKED_HERMES: &str = "v0.23.2";
const ISSUES: &str = "https://github.com/j4ckxyz/hermes-desktop/issues";

/// Methods the chat cannot work without. A Hermes that does not serve one of them is genuinely
/// incompatible, and [`explain`] says which release to run.
const CORE_METHODS: [&str; 5] = ["profiles.list", "session.create", "session.resume", "session.list", "prompt.submit"];

/// The socket's protocol mode, from its `gateway.ready` payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `server.dispatch` (`tui_gateway/contracts/*.py`). Also every Hermes before the session
    /// authority (v0.22.1) and any socket whose `gateway.ready` never says otherwise.
    Plain,
    /// `gateway.ready.session_authority: true` (`tui_gateway/contracts/canonical*.py`).
    Authority,
}

impl Mode {
    pub fn of_ready(ready: &Value) -> Mode {
        if ready.get("session_authority").and_then(Value::as_bool) == Some(true) {
            Mode::Authority
        } else {
            Mode::Plain
        }
    }

    pub fn is_authority(self) -> bool {
        self == Mode::Authority
    }
}

/// Keys a request still does its job without, per method. Only these are ever dropped when a
/// Hermes release refuses them. `profile` is never here: without it a multi-profile gateway
/// would act on the wrong bot. Neither are `prompt.submit`'s `submission_id` (the authority
/// requires an id) or `attachments` (dropping them would silently lose the user's images).
pub fn droppable(method: &str) -> &'static [&'static str] {
    match method {
        // Plain `idempotency_key` arrived after v2026.9.24; the authority mints an id if
        // `request_id` is missing ("Omit the ID for a new session", Gateway local sessions).
        "session.create" => &["idempotency_key", "request_id", "follow_profile_config", "hidden", "source"],
        "session.resume" => &["cols", "source", "defer_history", "omit_messages"],
        "session.list" => &["include_hidden"],
        "image.attach_bytes" => &["filename"],
        "profiles.create" => &["description"],
        _ => &[],
    }
}

/// `session.create` for a canonical Bot Chat. The retry key is `request_id` on the authority
/// (`CanonicalCreateParams`) and `idempotency_key` on a plain socket (`SessionCreateParams`).
pub fn create_params(mode: Mode, profile: &str, retry_key: &str) -> Value {
    let mut p = json!({"profile": profile, "title": BOT_CHAT_TITLE, "hidden": true, "follow_profile_config": true, "source": "gui"});
    p[if mode.is_authority() {
        "request_id"
    } else {
        "idempotency_key"
    }] = json!(retry_key);
    p
}

/// `session.resume`: the same keys in both modes (`SessionResumeParams`,
/// `CanonicalResumeParams`). `attach` resumes as this client's live view (terminal width and
/// source); otherwise it is a snapshot read for the revision/generation fences.
pub fn resume_params(session_id: &str, profile: &str, attach: bool) -> Value {
    let mut p = json!({"session_id": session_id, "defer_history": true, "omit_messages": true, "profile": profile});
    if attach {
        p["cols"] = json!(96);
        p["source"] = json!("desktop");
    }
    p
}

/// `session.list`: stored sessions, hidden ones included (canonical chats are born hidden).
/// The authority lists only live sessions unless asked for `scope: "stored"`; a plain socket
/// has no `scope` key and lists stored sessions.
pub fn list_params(mode: Mode, profile: &str, title: Option<&str>, limit: usize) -> Value {
    let mut p = json!({"profile": profile, "limit": limit, "include_hidden": true});
    if let Some(t) = title {
        p["title"] = json!(t);
    }
    if mode.is_authority() {
        p["scope"] = json!("stored");
    }
    p
}

/// The session to resume from an exact-title `session.list` lookup: the lineage's live
/// compression tip (`resolved_id`) when the server followed one, else the row itself.
pub fn canonical_from_list(r: &Value) -> Option<String> {
    let rows = r.get("sessions").and_then(Value::as_array)?;
    rows.iter()
        .filter(|row| {
            let root = row
                .get("root_title")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            root.or_else(|| row.get("title").and_then(Value::as_str)) == Some(BOT_CHAT_TITLE)
        })
        .max_by(|a, b| activity(a).total_cmp(&activity(b)))
        .and_then(live_id)
}

/// `prompt.submit`. The authority's admission requires a client id, documented as
/// `submission_id` (*Gateway local sessions*: `prompt.submit({session_id, submission_id, text})`;
/// `input_id` is the older alias), and takes staged `attachments` (`CanonicalSubmitParams`). A
/// plain socket's `PromptSubmitParams` has never had either key: images are attached to the
/// session beforehand and files ride in the text as `@file:` references.
pub fn submit_params(mode: Mode, session_id: &str, text: &str, profile: &str, submission_id: &str, attachments: Vec<Value>) -> Value {
    let mut p = json!({"session_id": session_id, "text": text, "profile": profile});
    if mode.is_authority() {
        p["submission_id"] = json!(submission_id);
        if !attachments.is_empty() {
            p["attachments"] = Value::Array(attachments);
        }
    }
    p
}

/// `session.interrupt`. The authority interrupts one execution generation
/// (`CanonicalControlParams`), never "whatever runs now"; a plain socket takes none.
pub fn interrupt_params(mode: Mode, session_id: &str, profile: &str, generation: Value) -> Value {
    let mut p = json!({"session_id": session_id, "profile": profile});
    if mode.is_authority() {
        p["execution_generation"] = generation;
    }
    p
}

/// `session.title` (plain socket; the shared gateway answers -32601, use [`mutate_params`]).
pub fn title_params(session_id: &str, profile: &str, title: &str) -> Value {
    json!({"session_id": session_id, "title": title, "profile": profile})
}

/// `session.compress` (plain socket; the shared gateway answers -32601, use [`mutate_params`]).
pub fn compress_params(session_id: &str, profile: &str, focus: Option<&str>) -> Value {
    json!({"session_id": session_id, "focus_topic": focus, "profile": profile})
}

/// `session.mutate` (authority only, `SessionMutateParams`): revision-fenced, and generation-
/// fenced when `generation` is given. `profile` is implicit on the authority's own profile
/// (stripped before validation, `_PROFILE_IMPLICIT`) and routes to a sibling profile otherwise.
pub fn mutate_params(session_id: &str, profile: &str, request_id: &str, revision: Value, generation: Option<Value>, operation: &str, payload: Value) -> Value {
    let mut p = json!({"session_id": session_id, "request_id": request_id, "expected_revision": revision, "operation": operation, "payload": payload, "profile": profile});
    if let Some(g) = generation {
        p["expected_generation"] = g;
    }
    p
}

/// `image.attach_bytes` (`ImageAttachBytesParams`): stage an image on a plain socket's session.
pub fn image_attach_params(session_id: &str, profile: &str, filename: &str, content_base64: &str) -> Value {
    json!({"session_id": session_id, "content_base64": content_base64, "filename": filename, "profile": profile})
}

/// `file.attach` (`FileAttachParams`): upload a file; the result's `ref_text` goes in the prompt.
pub fn file_attach_params(session_id: &str, profile: &str, name: &str, data_url: &str) -> Value {
    json!({"session_id": session_id, "name": name, "data_url": data_url, "profile": profile})
}

/// `slash.exec` (`SlashExecParams`).
pub fn slash_params(session_id: &str, profile: &str, command: &str) -> Value {
    json!({"session_id": session_id, "command": command, "profile": profile})
}

/// `profiles.create` (`ProfilesCreateParams`).
pub fn profile_create_params(name: &str, description: &str) -> Value {
    json!({"name": name, "description": description})
}

/// `profiles.configure` (`ProfilesConfigureParams`): a compare-and-swap of one `ui_meta` key.
pub fn profile_meta_params(name: &str, key: &str, meta: Value, revision: Option<i64>) -> Value {
    let mut p = json!({"name": name, "ui_meta": {key: meta}});
    if let Some(rev) = revision {
        p["ui_meta_expected_revisions"] = json!({key: rev});
    }
    p
}

/// `toolsets.list` is session-scoped (`_SessionScoped`: only `session_id`).
pub fn toolsets_params(session_id: Option<&str>) -> Value {
    match session_id {
        Some(s) => json!({"session_id": s}),
        None => json!({}),
    }
}

/// Top-level keys of `method`'s params that Hermes refused as outside its contract, from
/// either mode's refusal (see the module docs). Empty for any other error.
pub fn refused_keys(method: &str, e: &RpcError) -> Vec<String> {
    let top = |loc: &str| loc.split('.').next().unwrap_or(loc).to_owned();
    match e.code {
        4000 => e
            .message
            .strip_prefix("invalid params for ")
            .and_then(|r| r.strip_prefix(method))
            .and_then(|r| r.strip_prefix(": "))
            .and_then(|r| r.split_once(": "))
            .filter(|(_, why)| why.starts_with("Extra inputs are not permitted"))
            .map(|(loc, _)| vec![top(loc)])
            .unwrap_or_default(),
        4001 if e.message == "invalid_params" => e
            .data
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(top)
            .collect(),
        _ => vec![],
    }
}

/// The one user-facing text for a failed request. The plain gateway's own wording blames
/// version skew and tells the user to run `hermes update`; when this client sent the bad
/// shape that advice is wrong, so a refused request is reported as a Hermes Desktop bug. Only a
/// Hermes that lacks a core method is called incompatible, with the release it needs.
pub fn explain(method: &str, e: &RpcError) -> String {
    let keys = refused_keys(method, e);
    let refused = !keys.is_empty() || (e.code == 4001 && e.message == "invalid_params");
    if refused {
        let which = if keys.is_empty() { String::new() } else { format!(" ({})", keys.join(", ")) };
        return format!(
            "Hermes refused Hermes Desktop's {method} request{which}. This is a Hermes Desktop \
             compatibility bug, not a problem with your Hermes, so updating Hermes won't fix it. \
             Please report it: {ISSUES}"
        );
    }
    if e.code == METHOD_NOT_FOUND {
        return if CORE_METHODS.contains(&method) {
            format!(
                "This Hermes server is too old for Hermes Desktop: it doesn't serve {method}. \
                 Hermes Desktop needs Hermes {MIN_HERMES} or newer (checked up to {CHECKED_HERMES})."
            )
        } else {
            format!("This Hermes doesn't offer {method} on this connection.")
        };
    }
    // Never pass on the version-skew advice, whatever else the message says.
    match e.message.split_once(" — the client and the Hermes backend are out of sync") {
        Some((head, _)) => head.to_owned(),
        None => e.message.clone(),
    }
}

/// Session sources that are not conversations with the bot: cron runs (the official desktop
/// lists them apart from its recents) and the gateway's internal sources
/// (`INTERNAL_LISTING_SOURCES` in `hermes_state_sessions.py`).
pub const NOT_CONVERSATIONS: [&str; 4] = ["cron", "kanban", "tool", "oneshot"];

/// Listing rows that are conversations: at least one message, from a human-facing source.
pub fn conversations(rows: Vec<Value>) -> Vec<Value> {
    rows.into_iter()
        .filter(|r| r.get("message_count").and_then(Value::as_u64).unwrap_or(0) > 0)
        .filter(|r| !NOT_CONVERSATIONS.contains(&r.get("source").and_then(Value::as_str).unwrap_or_default()))
        .collect()
}

/// A listing row's live id: the compression tip when the row names one.
pub fn live_id(row: &Value) -> Option<String> {
    ["resolved_id", "id", "session_id"]
        .iter()
        .find_map(|k| {
            row.get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_owned)
}

fn activity(row: &Value) -> f64 {
    ["last_active", "last_active_at", "started_at"]
        .iter()
        .find_map(|k| row.get(*k).and_then(Value::as_f64))
        .unwrap_or(0.0)
}

/// Thread-list rows in the server's order (most recently active first), normalised to
/// `{id, root_id, title, preview, source, message_count, last_active}`. `last_active` is null
/// when the source does not report activity (`session.list` rows carry only `started_at`, the
/// day a conversation began, which must not be shown as when it was last used).
pub fn thread_rows(rows: &[Value], has_activity: bool) -> Vec<Value> {
    rows.iter()
        .filter_map(|r| {
            let id = live_id(r)?;
            let root = r
                .get("_lineage_root_id")
                .or_else(|| r.get("lineage_root_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&id)
                .to_owned();
            let last = if has_activity {
                r.get("last_active")
                    .or_else(|| r.get("last_active_at"))
                    .and_then(Value::as_f64)
            } else {
                None
            };
            Some(json!({
                "id": id,
                "root_id": root,
                "row_id": r.get("id").and_then(Value::as_str).unwrap_or_default(),
                "title": r.get("title").and_then(Value::as_str).unwrap_or_default(),
                "preview": r.get("preview").and_then(Value::as_str).unwrap_or_default(),
                "source": r.get("source").and_then(Value::as_str).unwrap_or_default(),
                "message_count": r.get("message_count").and_then(Value::as_u64).unwrap_or(0),
                "last_active": last,
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort();
        k
    }

    use Mode::{Authority, Plain};

    #[test]
    fn create_uses_each_modes_retry_key() {
        let plain = create_params(Plain, "default", "k1");
        assert_eq!(plain["idempotency_key"], "k1");
        assert!(
            plain.get("request_id").is_none(),
            "a plain gateway refuses request_id (Hermes 0.22.5)"
        );
        let auth = create_params(Authority, "default", "k1");
        assert_eq!(auth["request_id"], "k1");
        assert!(auth.get("idempotency_key").is_none());
    }

    /// The reported bug: "invalid params for prompt.submit: submission_id: Extra inputs are not
    /// permitted". No plain `PromptSubmitParams` has ever declared `submission_id`, `input_id` or
    /// `attachments`, so a plain socket's submit must never carry them; the authority requires an
    /// id and gets the documented `submission_id`.
    #[test]
    fn submit_keys_match_each_contract() {
        let att = vec![json!({"path": "/u/a.png", "mime": "image/png"})];
        assert_eq!(
            keys(&submit_params(Plain, "rt", "hi", "p", "i1", att.clone())),
            ["profile", "session_id", "text"]
        );
        assert_eq!(
            keys(&submit_params(Authority, "rt", "hi", "p", "i1", att)),
            ["attachments", "profile", "session_id", "submission_id", "text"]
        );
        assert_eq!(
            keys(&submit_params(Authority, "rt", "hi", "p", "i1", vec![])),
            ["profile", "session_id", "submission_id", "text"]
        );
    }

    #[test]
    fn list_asks_the_authority_for_stored_rows() {
        assert!(list_params(Plain, "p", None, 13).get("scope").is_none());
        assert_eq!(
            list_params(Authority, "p", Some("Bot Chat"), 20)["scope"],
            "stored"
        );
        assert_eq!(toolsets_params(None), json!({}));
        assert_eq!(toolsets_params(Some("rt")), json!({"session_id": "rt"}));
    }

    #[test]
    fn mode_comes_from_gateway_ready() {
        assert_eq!(Mode::of_ready(&json!({"session_authority": true, "heartbeat": true})), Authority);
        assert_eq!(Mode::of_ready(&json!({"heartbeat": true})), Plain);
        assert_eq!(Mode::of_ready(&Value::Null), Plain);
    }

    /// Hermes's exact refusal texts (`registry.validate_params`, `session_controls.dispatch`).
    const PLAIN_REFUSAL: &str = "invalid params for prompt.submit: submission_id: Extra inputs are not permitted — the client and the Hermes backend are out of sync (different versions); run `hermes update` and restart both";

    fn err(code: i64, message: &str, data: Value) -> RpcError {
        RpcError { code, message: message.into(), data }
    }

    #[test]
    fn refusals_name_the_refused_keys() {
        assert_eq!(refused_keys("prompt.submit", &err(4000, PLAIN_REFUSAL, Value::Null)), ["submission_id"]);
        // Another method's refusal, or another error with code 4000, names nothing.
        assert!(refused_keys("session.create", &err(4000, PLAIN_REFUSAL, Value::Null)).is_empty());
        assert!(refused_keys("session.list", &err(4000, "subagent_id required", Value::Null)).is_empty());
        let nested = err(4000, "invalid params for session.create: client.label: Extra inputs are not permitted — x", Value::Null);
        assert_eq!(refused_keys("session.create", &nested), ["client"]);
        let canon = err(4001, "invalid_params", json!({"reason": "invalid_params", "fields": ["idempotency_key", "attachments.0.mime"]}));
        assert_eq!(refused_keys("session.create", &canon), ["idempotency_key", "attachments"]);
        assert!(refused_keys("x", &err(4001, "stale_generation", json!({"fields": ["a"]}))).is_empty());
    }

    #[test]
    fn explain_never_tells_users_to_update_hermes_for_a_client_bug() {
        let m = explain("prompt.submit", &err(4000, PLAIN_REFUSAL, Value::Null));
        assert!(!m.contains("hermes update") && !m.contains("out of sync"), "{m}");
        assert!(m.contains("submission_id") && m.contains("Hermes Desktop") && m.contains("won't fix"), "{m}");
        let m = explain("prompt.submit", &err(4001, "invalid_params", json!({"reason": "invalid_params"})));
        assert!(m.contains("Hermes Desktop compatibility bug"), "{m}");
        // Other errors pass through, minus any version-skew advice.
        assert_eq!(explain("prompt.submit", &err(4009, "session busy", Value::Null)), "session busy");
        assert_eq!(explain("x", &err(4000, "bad — the client and the Hermes backend are out of sync; run `hermes update`", Value::Null)), "bad");
    }

    #[test]
    fn only_a_missing_core_method_is_called_incompatible() {
        let nf = err(METHOD_NOT_FOUND, "unknown method", Value::Null);
        for m in CORE_METHODS {
            let text = explain(m, &nf);
            assert!(text.contains("too old") && text.contains(MIN_HERMES), "{text}");
        }
        assert_eq!(explain("connectors.list", &nf), "This Hermes doesn't offer connectors.list on this connection.");
    }

    #[test]
    fn nothing_the_chat_needs_is_droppable() {
        assert!(droppable("prompt.submit").is_empty());
        assert!(droppable("session.interrupt").is_empty());
        assert!(droppable("session.mutate").is_empty());
        for m in ["session.create", "session.resume", "session.list", "image.attach_bytes", "profiles.create"] {
            assert!(!droppable(m).contains(&"profile"), "{m}");
        }
    }

    /// Accepted params per method and mode, per Hermes release, read from
    /// `tui_gateway/contracts` (plain: `METHODS`; authority: `CANONICAL_METHODS` after
    /// `_PROFILE_IMPLICIT` strips `profile`). A key missing from a release's set is refused there.
    mod contracts {
        use super::*;

        pub struct Release {
            pub name: &'static str,
            pub mode: Mode,
            pub methods: &'static [(&'static str, &'static [&'static str])],
        }

        const RESUME_PLAIN: &[&str] = &["close_on_disconnect", "cols", "defer_history", "eager_build", "lazy", "omit_messages", "profile", "session_id", "source"];
        const SHARED: &[(&str, &[&str])] = &[
            ("image.attach_bytes", &["content_base64", "data", "ext", "filename", "profile", "session_id"]),
            ("file.attach", &["data_url", "name", "path", "profile", "session_id"]),
            ("slash.exec", &["command", "profile", "session_id"]),
            ("toolsets.list", &["session_id"]),
            ("profiles.create", &["clone_all", "clone_channels", "clone_from", "description", "mirror_credentials", "model", "name", "no_alias", "no_skills", "profile", "provider", "share_auth", "soul"]),
            ("profiles.configure", &["confirm_expensive_model", "description", "disabled_skills", "enabled_mcp_servers", "enabled_toolsets", "model", "name", "profile", "provider", "soul", "ui_meta", "ui_meta_expected_revisions"]),
        ];

        /// v2026.9.14 and v2026.9.24: the first strict releases (no `idempotency_key`).
        pub const PLAIN_2026_9: &[(&str, &[&str])] = &[
            ("session.create", &["close_on_disconnect", "cols", "cwd", "fast", "follow_profile_config", "hidden", "messages", "model", "parent_session_id", "profile", "provider", "reasoning_effort", "room_plumbing", "source", "title"]),
            ("session.resume", RESUME_PLAIN),
            ("session.list", &["include_hidden", "limit", "profile", "title"]),
            ("session.title", &["profile", "session_id", "title"]),
            ("session.compress", &["focus_topic", "profile", "session_id"]),
            ("session.interrupt", &["expected_hosted_task_id", "profile", "session_id"]),
            ("prompt.submit", &["confirm_empty_truncate", "confirm_truncate", "display_kind", "interrupted", "profile", "queued", "rebind_survivor_row_ids", "session_id", "surface", "text", "truncate_before_message_id", "truncate_before_row_id", "truncate_before_user_ordinal", "voice_context"]),
        ];
        /// v0.21.6 through v0.23.2 (identical for every key this client sends).
        pub const PLAIN_0_21: &[(&str, &[&str])] = &[
            ("session.create", &["close_on_disconnect", "cols", "cwd", "cwd_explicit", "fast", "follow_profile_config", "hidden", "idempotency_key", "messages", "model", "parent_session_id", "profile", "provider", "reasoning_effort", "room_plumbing", "service_tier", "source", "title"]),
            ("session.resume", &["close_on_disconnect", "cols", "defer_history", "eager_build", "inline_images", "lazy", "omit_messages", "profile", "session_id", "source"]),
            ("session.list", &["include_hidden", "limit", "profile", "title"]),
            ("session.title", &["profile", "session_id", "title"]),
            ("session.compress", &["focus_topic", "profile", "session_id"]),
            ("session.interrupt", &["expected_hosted_task_id", "profile", "session_id"]),
            ("prompt.submit", &["confirm_deep_truncate", "confirm_empty_truncate", "confirm_truncate", "display_kind", "interrupted", "profile", "queued", "rebind_survivor_row_ids", "session_id", "surface", "text", "title_preview", "truncate_before_message_id", "truncate_before_row_id", "truncate_before_user_ordinal", "voice_context", "voice_turn"]),
        ];
        /// v0.22.1 and v0.22.3 through v0.23.2 (later releases only add keys).
        pub const AUTHORITY_0_22: &[(&str, &[&str])] = &[
            ("session.create", &["accept_hooks", "api_key", "base_url", "checkpoints", "cwd", "cwd_explicit", "editor", "follow_profile_config", "hidden", "ignore_rules", "ignore_user_config", "max_turns", "model", "pass_session_id", "provider", "reasoning", "request_id", "row_cwd", "run_budget", "safe_mode", "service_tier", "skills", "source", "title", "toolsets", "verbose", "yolo"]),
            ("session.resume", &["api_key", "cols", "defer_history", "display_projection", "editor", "latest", "omit_messages", "session_id", "source", "title", "workspace"]),
            ("session.list", &["include_hidden", "limit", "scope", "title"]),
            ("session.interrupt", &["execution_generation", "session_id"]),
            ("session.mutate", &["expected_generation", "expected_revision", "operation", "payload", "request_id", "session_id"]),
            ("prompt.submit", &["api_text", "attachments", "clarify_batch", "display_kind", "finite", "input_id", "interrupted", "model_override", "queued", "session_id", "submission_id", "surface", "text", "title_preview", "turn_author", "unattended", "voice_context", "voice_turn"]),
        ];

        pub fn releases() -> Vec<Release> {
            vec![
                Release { name: "v2026.9.14", mode: Plain, methods: PLAIN_2026_9 },
                Release { name: "v0.21.6..v0.23.2 plain", mode: Plain, methods: PLAIN_0_21 },
                Release { name: "v0.22.1..v0.23.2 authority", mode: Authority, methods: AUTHORITY_0_22 },
            ]
        }

        pub fn accepted(r: &Release, method: &str) -> Option<Vec<&'static str>> {
            r.methods.iter().chain(SHARED).find(|(m, _)| *m == method).map(|(_, k)| k.to_vec())
        }
    }

    /// Every request the app builds, per mode.
    fn requests(mode: Mode) -> Vec<(&'static str, Value)> {
        let att = vec![json!({"path": "/c/a.png", "mime": "image/png"})];
        let mut v = vec![
            ("session.create", create_params(mode, "p", "k")),
            ("session.resume", resume_params("s", "p", true)),
            ("session.resume", resume_params("s", "p", false)),
            ("session.list", list_params(mode, "p", Some(BOT_CHAT_TITLE), 20)),
            ("session.list", list_params(mode, "p", None, 200)),
            ("prompt.submit", submit_params(mode, "s", "hi", "p", "id", att)),
            ("session.interrupt", interrupt_params(mode, "s", "p", json!(3))),
            ("image.attach_bytes", image_attach_params("s", "p", "a.png", "AAAA")),
            ("file.attach", file_attach_params("s", "p", "a.txt", "data:text/plain;base64,AA==")),
            ("slash.exec", slash_params("s", "p", "/help")),
            ("toolsets.list", toolsets_params(Some("s"))),
            ("profiles.create", profile_create_params("p", "d")),
            ("profiles.configure", profile_meta_params("p", "hermes-bots", json!({"title": "T"}), Some(2))),
        ];
        match mode {
            Plain => {
                v.push(("session.title", title_params("s", "p", BOT_CHAT_TITLE)));
                v.push(("session.compress", compress_params("s", "p", Some("f"))));
            }
            Authority => {
                v.push(("session.mutate", mutate_params("s", "p", "r", json!(1), None, "rename", json!({"title": BOT_CHAT_TITLE}))));
                v.push(("session.mutate", mutate_params("s", "p", "r", json!(1), Some(json!(2)), "compress", json!({}))));
            }
        }
        v
    }

    /// Each request, after dropping only what the release refuses and [`droppable`] allows,
    /// stays inside every checked release's contract for its mode. On the newest releases
    /// nothing is dropped at all.
    #[test]
    fn every_request_fits_every_checked_release() {
        for r in contracts::releases() {
            for (method, params) in requests(r.mode) {
                let Some(accepted) = contracts::accepted(&r, method) else {
                    panic!("{}: no contract recorded for {method}", r.name)
                };
                // `profile` is implicit on the authority's own session verbs (`_PROFILE_IMPLICIT`).
                let implicit = r.mode.is_authority() && (method.starts_with("session.") || method == "prompt.submit");
                let refused: Vec<&str> = keys(&params)
                    .into_iter()
                    .filter(|k| !accepted.contains(k) && !(implicit && *k == "profile"))
                    .collect();
                for k in &refused {
                    assert!(droppable(method).contains(k), "{}: {method} sends `{k}`, which it refuses and the client can't drop", r.name);
                }
                if r.name != "v2026.9.14" {
                    assert!(refused.is_empty(), "{}: {method} sends {refused:?}", r.name);
                }
            }
        }
    }

    #[test]
    fn the_oldest_strict_release_only_drops_the_create_retry_key() {
        let r = &contracts::releases()[0];
        let create = create_params(Plain, "p", "k");
        let accepted = contracts::accepted(r, "session.create").unwrap();
        let refused: Vec<&str> = keys(&create).into_iter().filter(|k| !accepted.contains(k)).collect();
        assert_eq!(refused, ["idempotency_key"]);
    }

    #[test]
    fn title_lookup_resumes_the_compression_tip() {
        let r = json!({"sessions": [{"id": "root", "resolved_id": "tip", "title": "Bot Chat", "started_at": 1.0}]});
        assert_eq!(canonical_from_list(&r).as_deref(), Some("tip"));
        let r = json!({"sessions": [{"id": "only", "title": "Bot Chat", "started_at": 1.0}]});
        assert_eq!(canonical_from_list(&r).as_deref(), Some("only"));
        assert_eq!(
            canonical_from_list(&json!({"sessions": [{"id": "x", "title": "Other"}]})),
            None
        );
    }

    #[test]
    fn empty_and_cron_sessions_are_not_conversations() {
        let rows = vec![
            json!({"id": "a", "source": "cron", "message_count": 4}),
            json!({"id": "b", "source": "tui", "message_count": 0}),
            json!({"id": "c", "source": "discord", "message_count": 3}),
        ];
        let kept: Vec<_> = conversations(rows).into_iter().map(|r| r["id"].clone()).collect();
        assert_eq!(kept, ["c"]);
    }

    #[test]
    fn thread_rows_never_pass_a_start_date_off_as_activity() {
        let rows = [
            json!({"id": "tip", "_lineage_root_id": "root", "title": "Plans", "started_at": 100.0, "last_active": 900.0, "message_count": 4}),
        ];
        let rest = thread_rows(&rows, true);
        assert_eq!(rest[0]["id"], "tip");
        assert_eq!(rest[0]["root_id"], "root");
        assert_eq!(rest[0]["last_active"], 900.0);
        let rpc = thread_rows(
            &[json!({"id": "s1", "title": "Plans", "started_at": 100.0})],
            false,
        );
        assert!(rpc[0]["last_active"].is_null());
    }
}
