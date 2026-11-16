//! Chat transcript model: stored history rows and live streamed turns.
//!
//! History rows come from `GET /api/sessions/{id}/messages` (source
//! `hermes_cli/web_routers/sessions.py`): `role, content, timestamp, id, display_kind,
//! tool_calls, tool_name, …`. Live turns are assembled from gateway events.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::roster::{parse_agent_notice, AgentNotice};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    /// A `Message from 🤖 …` inter-agent message. It is a `user` row on the wire but is never the
    /// user's own text.
    Agent(AgentNotice),
    /// Timeline markers (model switch, compaction, process completion…).
    System,
    /// Something this desktop did that Hermes history does not record (an approval answered, a
    /// file saved). Shown for the current session only; never sent anywhere.
    Local(LocalEvent),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalEvent {
    /// Lucide icon name for the chip.
    pub icon: String,
    /// Prose shown when the chip is expanded (what an approval was for…).
    pub detail: Option<String>,
    /// Shown as code when expanded (the approved command, a saved path).
    #[serde(default)]
    pub code: Option<String>,
    /// A local file the expanded chip can reveal.
    pub path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttachmentKind {
    Image,
    Audio,
    Video,
    File,
}

/// A file in a message: either on the Hermes host (`remote_path`) or local (`local_path`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub kind: AttachmentKind,
    pub remote_path: Option<String>,
    pub local_path: Option<String>,
    pub size: Option<u64>,
}

impl Attachment {
    pub fn from_remote(path: &str) -> Self {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_owned();
        let mime = mime_guess::from_path(&name).first_or_octet_stream().essence_str().to_owned();
        Self { kind: kind_of(&mime), name, mime, remote_path: Some(path.to_owned()), local_path: None, size: None }
    }
}

pub fn kind_of(mime: &str) -> AttachmentKind {
    if mime.starts_with("image/") {
        AttachmentKind::Image
    } else if mime.starts_with("audio/") {
        AttachmentKind::Audio
    } else if mime.starts_with("video/") {
        AttachmentKind::Video
    } else {
        AttachmentKind::File
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

/// One tool call or delegated subagent shown as a card in the turn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub id: String,
    pub is_subagent: bool,
    pub name: String,
    pub title: String,
    pub detail: Option<String>,
    pub summary: Option<String>,
    pub state: ToolState,
    pub duration_s: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub timestamp: Option<f64>,
    pub attachments: Vec<Attachment>,
    pub activity: Vec<Activity>,
    pub streaming: bool,
    pub error: Option<String>,
}

impl ChatMessage {
    pub fn new(id: impl Into<String>, role: Role, text: impl Into<String>, ts: Option<f64>) -> Self {
        Self { id: id.into(), role, text: text.into(), timestamp: ts, attachments: vec![], activity: vec![], streaming: false, error: None }
    }
}

fn media_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)MEDIA:\s*(\S+)").unwrap())
}

/// Lift `MEDIA:/path` tags out of a reply into attachments (docs: *Deliverable mode*). Tags in
/// fenced code are left alone, since they are examples.
pub fn extract_media(text: &str) -> (String, Vec<Attachment>) {
    let mut out = String::with_capacity(text.len());
    let mut atts = Vec::new();
    let mut in_fence = false;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if in_fence || !line.contains("MEDIA:") {
            out.push_str(line);
            continue;
        }
        let mut rest = line.to_owned();
        for c in media_re().captures_iter(line) {
            let path = c[1].trim_end_matches(['.', ',', ')', '`', '"', '\'']);
            atts.push(Attachment::from_remote(path));
            rest = rest.replace(&c[0], "");
        }
        if !rest.trim().is_empty() {
            out.push_str(&rest);
        }
    }
    (out.trim_end().to_owned(), atts)
}

fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str).or_else(|| p.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The text the server says should be shown for a silent reply (docs: *intentional silence
/// tokens*).
pub fn is_silent(text: &str) -> bool {
    matches!(text.trim(), "[SILENT]" | "NO_REPLY" | "[NO_REPLY]" | "NO REPLY")
}

/// Convert stored rows into display messages, oldest first. Tool rows are folded into the
/// preceding assistant turn as activity; hidden rows are dropped.
pub fn from_history(rows: &[Value]) -> Vec<ChatMessage> {
    let mut rows: Vec<&Value> = rows.iter().collect();
    rows.sort_by(|a, b| {
        let ta = a.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        let tb = b.get("timestamp").and_then(Value::as_f64).unwrap_or(0.0);
        let ia = a.get("id").and_then(Value::as_i64).unwrap_or(0);
        let ib = b.get("id").and_then(Value::as_i64).unwrap_or(0);
        ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal).then(ia.cmp(&ib))
    });
    let mut out: Vec<ChatMessage> = Vec::new();
    for row in rows {
        let role = row.get("role").and_then(Value::as_str).unwrap_or("");
        let kind = row.get("display_kind").and_then(Value::as_str).unwrap_or("");
        if kind == "hidden" {
            continue;
        }
        let id = row.get("id").or_else(|| row.get("row_id")).map(|v| v.to_string()).unwrap_or_default();
        let ts = row.get("timestamp").and_then(Value::as_f64);
        let text = row.get("text").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| content_text(row.get("content").unwrap_or(&Value::Null)));
        match role {
            "user" if kind.is_empty() => {
                let role = match parse_agent_notice(&text) {
                    Some(n) => Role::Agent(n),
                    None => Role::User,
                };
                let (text, atts) = if matches!(role, Role::User) { (text, vec![]) } else { extract_media(&text) };
                let mut m = ChatMessage::new(id, role, text, ts);
                m.attachments = atts;
                out.push(m);
            }
            "assistant" => {
                let calls = row.get("tool_calls").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut acts: Vec<Activity> = calls
                    .iter()
                    .map(|c| {
                        let name = c.pointer("/function/name").or_else(|| c.get("name")).and_then(Value::as_str).unwrap_or("tool").to_owned();
                        // OpenAI-style rows carry `arguments` as a JSON string.
                        let args = c.pointer("/function/arguments").and_then(|a| match a {
                            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
                            v => Some(v.clone()),
                        });
                        Activity {
                            id: c.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                            is_subagent: name == "delegate_task",
                            title: tool_title(&name, args.as_ref()).unwrap_or_else(|| humanize(&name)),
                            name,
                            detail: None,
                            summary: None,
                            state: ToolState::Done,
                            duration_s: None,
                        }
                    })
                    .collect();
                if text.trim().is_empty() || is_silent(&text) {
                    // A pure tool-call step: attach its calls to the turn that follows.
                    if let Some(last) = out.last_mut().filter(|m| m.role == Role::Assistant) {
                        last.activity.append(&mut acts);
                    } else if !acts.is_empty() {
                        let mut m = ChatMessage::new(id, Role::Assistant, "", ts);
                        m.activity = acts;
                        out.push(m);
                    }
                    continue;
                }
                let (text, atts) = extract_media(&text);
                // Merge with a preceding text-less tool step of the same turn.
                if let Some(last) = out.last_mut().filter(|m| m.role == Role::Assistant && m.text.is_empty()) {
                    last.text = text;
                    last.attachments.extend(atts);
                    last.activity.append(&mut acts);
                    last.id = id;
                    continue;
                }
                let mut m = ChatMessage::new(id, Role::Assistant, text, ts);
                m.attachments = atts;
                m.activity = acts;
                out.push(m);
            }
            "tool" => {}
            _ if !kind.is_empty() => {
                let shown = row.pointer("/display_metadata/display_text").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| humanize(kind));
                out.push(ChatMessage::new(id, Role::System, shown, ts));
            }
            _ => {}
        }
    }
    out
}

/// A readable title for tools whose arguments name something the user cares about. Routines
/// are Hermes cron jobs, managed by the `cronjob` tool (`tools/cronjob_tools.py`:
/// `action` = create | list | update | pause | resume | remove | run | cancel, plus `name`/`job_id`).
pub fn tool_title(name: &str, args: Option<&Value>) -> Option<String> {
    match name {
        "cronjob" => {
            let a = args?;
            let verb = match a.get("action")?.as_str()? {
                "create" => "Created routine",
                "update" => "Updated routine",
                "pause" => "Paused routine",
                "resume" => "Resumed routine",
                "remove" => "Removed routine",
                "run" => "Ran routine",
                "cancel" => "Cancelled routine",
                "list" => return Some("Checked routines".into()),
                _ => return None,
            };
            let what = a.get("name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).or_else(|| a.get("job_id").and_then(Value::as_str));
            Some(match what {
                Some(w) => format!("{verb} · {w}"),
                None => verb.to_owned(),
            })
        }
        _ => None,
    }
}

/// Lucide icon for a tool chip.
pub fn tool_icon(name: &str, is_subagent: bool) -> &'static str {
    match name {
        _ if is_subagent => "users",
        "cronjob" => "clock",
        "web_search" | "web_extract" | "browser_navigate" => "globe",
        "terminal" | "process" => "terminal",
        "read_file" | "write_file" | "patch" | "search_files" => "file-text",
        "send_message" => "send",
        "image_generate" => "image",
        "memory" => "brain",
        _ => "wrench",
    }
}

pub fn humanize(name: &str) -> String {
    let s = name.replace(['_', '.'], " ");
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Render a transcript as Markdown for Share / copy.
pub fn to_markdown(title: &str, msgs: &[ChatMessage]) -> String {
    let mut s = format!("# {title}\n\n");
    for m in msgs {
        let who = match &m.role {
            Role::User => "**You**".to_owned(),
            Role::Assistant => format!("**{title}**"),
            Role::Agent(n) => format!("**🤖 {} (@{})**", n.name, n.handle),
            Role::System | Role::Local(_) => "_system_".to_owned(),
        };
        s.push_str(&format!("{who}\n\n{}\n\n", m.text));
        for a in &m.attachments {
            s.push_str(&format!("📎 {}\n\n", a.remote_path.as_deref().or(a.local_path.as_deref()).unwrap_or(&a.name)));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn media_tags_become_attachments() {
        let (t, a) = extract_media("Here you go.\nMEDIA:/tmp/chart.png\n```\nMEDIA:/example.txt\n```");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].kind, AttachmentKind::Image);
        assert_eq!(a[0].name, "chart.png");
        assert!(t.contains("MEDIA:/example.txt"));
        assert!(!t.contains("chart.png"));
    }

    #[test]
    fn history_folds_tools_and_notices() {
        let rows = vec![
            json!({"id":1,"role":"user","content":"hi","timestamp":1.0}),
            json!({"id":2,"role":"assistant","content":"","tool_calls":[{"id":"t1","function":{"name":"web_search"}}],"timestamp":2.0}),
            json!({"id":3,"role":"tool","content":"…","timestamp":3.0}),
            json!({"id":4,"role":"assistant","content":"Answer MEDIA:/a/b.pdf","timestamp":4.0}),
            json!({"id":5,"role":"user","content":"Message from 🤖 Scribe (@scribe): done","timestamp":5.0}),
            json!({"id":6,"role":"user","content":"secret","display_kind":"hidden","timestamp":6.0}),
            json!({"id":7,"role":"user","content":"x","display_kind":"model_switch","display_metadata":{"display_text":"Model → m2"},"timestamp":7.0}),
        ];
        let m = from_history(&rows);
        assert_eq!(m.len(), 4, "{m:#?}");
        assert_eq!(m[1].text, "Answer");
        assert_eq!(m[1].activity.len(), 1);
        assert_eq!(m[1].attachments[0].name, "b.pdf");
        assert!(matches!(&m[2].role, Role::Agent(n) if n.handle == "scribe"));
        assert_eq!(m[3].role, Role::System);
        assert_eq!(m[3].text, "Model → m2");
    }

    #[test]
    fn routine_tool_titles() {
        let a = json!({"action": "update", "name": "News watch", "job_id": "j1"});
        assert_eq!(tool_title("cronjob", Some(&a)).as_deref(), Some("Updated routine · News watch"));
        assert_eq!(tool_title("cronjob", Some(&json!({"action": "run", "job_id": "j1"}))).as_deref(), Some("Ran routine · j1"));
        assert_eq!(tool_title("web_search", Some(&a)), None);
        let rows = vec![
            json!({"id": 1, "role": "user", "content": "every morning please", "timestamp": 1.0}),
            json!({"id": 2, "role": "assistant", "content": "", "timestamp": 2.0,
                "tool_calls": [{"id": "c1", "function": {"name": "cronjob", "arguments": "{\"action\":\"create\",\"name\":\"News watch\"}"}}]}),
            json!({"id": 3, "role": "assistant", "content": "Done.", "timestamp": 3.0}),
        ];
        let m = from_history(&rows);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].activity[0].title, "Created routine · News watch");
    }

    #[test]
    fn silent_replies() {
        assert!(is_silent(" [SILENT] "));
        assert!(!is_silent("[SILENT] but more"));
    }
}
