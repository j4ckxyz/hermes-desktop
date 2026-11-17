//! Request shapes that differ between the two official gateway modes, and listing rows.
//!
//! A Hermes gateway WebSocket is either a plain gateway socket (`server.dispatch`, contracts in
//! `tui_gateway/contracts/*.py`) or a session-authority socket (`gateway.ready` says
//! `session_authority: true`; contracts in `tui_gateway/contracts/canonical*.py`). Both refuse
//! unknown keys: the plain socket answers `4000 invalid params for <method>: <key>: Extra inputs
//! are not permitted`, the authority `4001 invalid_params`. So every key sent here is one the
//! mode's contract declares (checked against hermes-agent v0.22.5).

use serde_json::{json, Value};

use crate::roster::BOT_CHAT_TITLE;

/// `session.create` for a canonical Bot Chat. The retry key is `request_id` on the authority
/// (`CanonicalCreateParams`) and `idempotency_key` on a plain socket (`SessionCreateParams`).
pub fn create_params(authority: bool, profile: &str, retry_key: &str) -> Value {
    let mut p = json!({"profile": profile, "title": BOT_CHAT_TITLE, "hidden": true, "follow_profile_config": true, "source": "gui"});
    p[if authority {
        "request_id"
    } else {
        "idempotency_key"
    }] = json!(retry_key);
    p
}

/// `session.list`: stored sessions, hidden ones included (canonical chats are born hidden).
/// The authority lists only live sessions unless asked for `scope: "stored"`; a plain socket
/// has no `scope` key and lists stored sessions.
pub fn list_params(authority: bool, profile: &str, title: Option<&str>, limit: usize) -> Value {
    let mut p = json!({"profile": profile, "limit": limit, "include_hidden": true});
    if let Some(t) = title {
        p["title"] = json!(t);
    }
    if authority {
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

/// `prompt.submit`. The authority's admission takes an `input_id` and staged `attachments`
/// (`CanonicalSubmitParams`); a plain socket's takes neither (`PromptSubmitParams`): images are
/// attached to the session beforehand and files ride in the text as `@file:` references.
pub fn submit_params(
    authority: bool,
    session_id: &str,
    text: &str,
    profile: &str,
    input_id: &str,
    attachments: Vec<Value>,
) -> Value {
    let mut p = json!({"session_id": session_id, "text": text, "profile": profile});
    if authority {
        p["input_id"] = json!(input_id);
        if !attachments.is_empty() {
            p["attachments"] = Value::Array(attachments);
        }
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

    #[test]
    fn create_uses_each_modes_retry_key() {
        let plain = create_params(false, "default", "k1");
        assert_eq!(plain["idempotency_key"], "k1");
        assert!(
            plain.get("request_id").is_none(),
            "a plain gateway refuses request_id (Hermes 0.22.5)"
        );
        let auth = create_params(true, "default", "k1");
        assert_eq!(auth["request_id"], "k1");
        assert!(auth.get("idempotency_key").is_none());
    }

    #[test]
    fn submit_keys_match_each_contract() {
        let att = vec![json!({"path": "/u/a.png", "mime": "image/png"})];
        assert_eq!(
            keys(&submit_params(false, "rt", "hi", "p", "i1", att.clone())),
            ["profile", "session_id", "text"]
        );
        assert_eq!(
            keys(&submit_params(true, "rt", "hi", "p", "i1", att)),
            ["attachments", "input_id", "profile", "session_id", "text"]
        );
        assert_eq!(
            keys(&submit_params(true, "rt", "hi", "p", "i1", vec![])),
            ["input_id", "profile", "session_id", "text"]
        );
    }

    #[test]
    fn list_asks_the_authority_for_stored_rows() {
        assert!(list_params(false, "p", None, 13).get("scope").is_none());
        assert_eq!(
            list_params(true, "p", Some("Bot Chat"), 20)["scope"],
            "stored"
        );
        assert_eq!(toolsets_params(None), json!({}));
        assert_eq!(toolsets_params(Some("rt")), json!({"session_id": "rt"}));
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
