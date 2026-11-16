//! The Bot Mode roster: a bot is a Hermes profile, as `profiles.list` reports it.
//!
//! Wire source: `tui_gateway/methods_profiles.py` (profiles.list rows) and the official
//! desktop Bots plugin (`ui_meta["hermes-bots"]`). Docs: *Bot Mode*.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The exact title that makes a session a canonical Bot Chat (docs: *What actually makes a chat
/// a Bot Chat*).
pub const BOT_CHAT_TITLE: &str = "Bot Chat";
/// `ui_meta` key the official Bots plugin owns.
pub const UI_META_KEY: &str = "hermes-bots";

/// Identifies a bot across connections.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct BotKey {
    pub conn: String,
    pub profile: String,
}

impl BotKey {
    pub fn new(conn: impl Into<String>, profile: impl Into<String>) -> Self {
        Self { conn: conn.into(), profile: profile.into() }
    }
}

/// Display metadata from `ui_meta["hermes-bots"]`. Unknown keys are preserved in `extra` so a
/// write never drops fields the official desktop set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BotMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    #[serde(default, rename = "sectionId", skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    #[serde(default, rename = "sectionName", skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// A conversation summary as profiles.list reports it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub last_active: Option<f64>,
    #[serde(default)]
    pub message_count: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bot {
    pub key: BotKey,
    /// Profile name: the stable handle.
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub model: Option<String>,
    pub is_default: bool,
    pub meta: BotMeta,
    pub meta_revision: Option<i64>,
    pub has_avatar: bool,
    pub canonical: Option<SessionSummary>,
    pub last_session: Option<SessionSummary>,
    /// Name of the connection, used to qualify handles that collide across machines.
    pub conn_name: String,
}

fn summary(v: Option<&Value>) -> Option<SessionSummary> {
    let v = v?;
    if v.is_null() {
        return None;
    }
    let mut s: SessionSummary = serde_json::from_value(v.clone()).ok()?;
    if s.last_active.is_none() {
        s.last_active = v.get("last_active_at").and_then(Value::as_f64);
    }
    Some(s)
}

impl Bot {
    /// Parse one row of `profiles.list`.
    pub fn from_row(conn: &str, conn_name: &str, row: &Value) -> Option<Bot> {
        let name = row.get("name")?.as_str()?.to_owned();
        let ui_meta = row.get("ui_meta").and_then(|m| m.get(UI_META_KEY));
        let meta: BotMeta = ui_meta.and_then(|m| serde_json::from_value(m.clone()).ok()).unwrap_or_default();
        let str_opt = |k: &str| row.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_owned);
        Some(Bot {
            key: BotKey::new(conn, &name),
            display_name: str_opt("display_name"),
            description: str_opt("description"),
            model: str_opt("model"),
            is_default: row.get("is_default").and_then(Value::as_bool).unwrap_or(name == "default"),
            meta,
            meta_revision: row.get("ui_meta_revisions").and_then(|r| r.get(UI_META_KEY)).and_then(Value::as_i64),
            has_avatar: row.get("has_avatar").and_then(Value::as_bool).unwrap_or(false),
            canonical: summary(row.get("canonical_session")),
            last_session: summary(row.get("last_session")),
            conn_name: conn_name.to_owned(),
            name,
        })
    }

    /// The friendly name: Bot Mode title, then the profile display name, then the profile name.
    pub fn title(&self) -> String {
        self.meta
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| self.display_name.clone())
            .unwrap_or_else(|| {
                if self.is_default {
                    "Hermes".to_owned()
                } else {
                    let mut c = self.name.chars();
                    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
                }
            })
    }

    pub fn hidden(&self) -> bool {
        self.meta.hidden.unwrap_or(false)
    }

    /// The `@`-tag the composer inserts: `@hermes` for the default profile, else the title slug
    /// when the bot has been renamed, else the profile name.
    pub fn handle(&self) -> String {
        if self.is_default {
            return "hermes".into();
        }
        let has_title = self.meta.title.as_ref().is_some_and(|t| !t.trim().is_empty())
            || self.display_name.is_some();
        if has_title {
            let s = slug(&self.title(), true);
            if !s.is_empty() {
                return s;
            }
        }
        self.name.clone()
    }

    /// Every tag that resolves to this bot (docs: *Renamed Bots keep their tags in sync*).
    pub fn tags(&self) -> Vec<String> {
        let mut tags = vec![self.name.to_lowercase()];
        if self.is_default {
            tags.push("hermes".into());
        }
        let title = self.title();
        for hyphen in [true, false] {
            let s = slug(&title, hyphen);
            if !s.is_empty() && !tags.contains(&s) {
                tags.push(s);
            }
        }
        tags
    }

    /// The most recent activity, from whichever of the canonical chat and the last session is
    /// newer (as the official Bots plugin computes it).
    pub fn last_active(&self) -> f64 {
        let a = self.canonical.as_ref().and_then(|s| s.last_active).unwrap_or(0.0);
        let b = self.last_session.as_ref().and_then(|s| s.last_active).unwrap_or(0.0);
        a.max(b)
    }

    /// One-line preview of the latest message, with an agent-notice prefix replaced by the
    /// sender's name.
    pub fn preview(&self) -> String {
        let has = |s: &&SessionSummary| s.preview.as_ref().is_some_and(|p| !p.trim().is_empty());
        let c = self.canonical.as_ref().filter(has);
        let l = self.last_session.as_ref().filter(has);
        let pick = match (c, l) {
            (Some(c), Some(l)) if l.last_active.unwrap_or(0.0) > c.last_active.unwrap_or(0.0) => Some(l),
            (Some(c), _) => Some(c),
            (None, l) => l,
        };
        let raw = strip_inline_md(&pick.and_then(|s| s.preview.clone()).unwrap_or_default());
        let line = match parse_agent_notice(&raw) {
            Some(n) => format!("🤖 {}: {}", n.name, n.body),
            None => raw,
        };
        one_line(&line, 90)
    }

    /// Stable colour index for the avatar when `ui_meta.color` is not a usable value.
    pub fn color_seed(&self) -> u32 {
        if let Some(Value::Number(n)) = &self.meta.color {
            if let Some(n) = n.as_u64() {
                return n as u32;
            }
        }
        self.name.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619))
    }

    /// An explicit `#rrggbb` colour from ui_meta, if any.
    pub fn color_hex(&self) -> Option<u32> {
        match &self.meta.color {
            Some(Value::String(s)) => u32::from_str_radix(s.trim_start_matches('#'), 16).ok().filter(|_| s.len() >= 6),
            _ => None,
        }
    }

    pub fn initials(&self) -> String {
        let t = self.title();
        let mut words = t.split_whitespace().filter_map(|w| w.chars().next());
        let a = words.next();
        let b = words.next();
        match (a, b) {
            (Some(a), Some(b)) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
            (Some(a), None) => a.to_uppercase().to_string(),
            _ => "?".into(),
        }
    }
}

/// `Research Buddy` → `research-buddy` (or `researchbuddy` without hyphens).
pub fn slug(s: &str, hyphen: bool) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if dash && hyphen && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c.to_ascii_lowercase());
        } else {
            dash = true;
        }
    }
    out
}

/// Drop inline Markdown markers so previews read as plain text.
pub fn strip_inline_md(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '*' | '`' | '_' | '#' | '>')).collect()
}

pub fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    } else {
        flat
    }
}

/// An inter-agent message: `Message from 🤖 <Name> (@<handle>[@conn]): body`.
/// Source: `tools/bot_mode_dm.py` adds the prefix server-side, and the official desktop detects
/// it at render time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNotice {
    pub name: String,
    pub handle: String,
    pub body: String,
}

fn notice_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)^\s*Message from 🤖 (.+?) \(@([^)\s]+)\):[ \t]*\n?(.*)$").unwrap())
}

pub fn parse_agent_notice(text: &str) -> Option<AgentNotice> {
    let c = notice_re().captures(text)?;
    Some(AgentNotice { name: c[1].trim().to_owned(), handle: c[2].to_owned(), body: c[3].trim().to_owned() })
}

fn mention_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(^|[^A-Za-z0-9_.@])@([A-Za-z0-9][A-Za-z0-9_.-]*(?:@[A-Za-z0-9][A-Za-z0-9_.-]*)?)").unwrap())
}

/// The `@tags` in a message, in order, without duplicates. E-mail addresses do not match.
pub fn mentions(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in mention_re().captures_iter(text) {
        let tag = c[2].trim_end_matches(['.', '-']).to_lowercase();
        if !out.contains(&tag) {
            out.push(tag);
        }
    }
    out
}

/// A tag resolved against the live roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedMention {
    pub tag: String,
    pub key: BotKey,
    pub title: String,
    /// The `message_agent` target: the profile name, qualified with the connection when the
    /// bot lives on another one.
    pub target: String,
}

/// Resolve tags against the roster. Exact profile names win over titles (docs: "A profile name
/// is matched first, so it can never be hijacked by another Bot's friendly name"). A tag that
/// matches several bots by title is left unresolved instead of guessed.
pub fn resolve_mentions(text: &str, roster: &[Bot], home_conn: &str) -> Vec<ResolvedMention> {
    let mut out = Vec::new();
    for tag in mentions(text) {
        if matches!(tag.as_str(), "all" | "everyone" | "user") {
            continue;
        }
        let (base, conn_q) = match tag.split_once('@') {
            Some((b, c)) => (b.to_owned(), Some(c.to_owned())),
            None => (tag.clone(), None),
        };
        let candidates: Vec<&Bot> = roster
            .iter()
            .filter(|b| match &conn_q {
                Some(c) => slug(&b.conn_name, true) == *c || b.key.conn.eq_ignore_ascii_case(c),
                None => true,
            })
            .collect();
        let by_name: Vec<&&Bot> = candidates.iter().filter(|b| b.name.eq_ignore_ascii_case(&base)).collect();
        let pick: Option<&Bot> = if by_name.len() == 1 {
            Some(by_name[0])
        } else if by_name.len() > 1 {
            by_name.iter().find(|b| b.key.conn == home_conn).map(|b| **b)
        } else {
            let by_tag: Vec<&&Bot> = candidates.iter().filter(|b| b.tags().contains(&base)).collect();
            if by_tag.len() == 1 {
                Some(by_tag[0])
            } else {
                // Several bots share the tag: a single one on this connection still wins,
                // otherwise refuse rather than guess.
                let local: Vec<&&&Bot> = by_tag.iter().filter(|b| b.key.conn == home_conn).collect();
                if by_tag.len() > 1 && local.len() == 1 && by_tag.len() - 1 == by_tag.iter().filter(|b| b.key.conn != home_conn).count() {
                    Some(**local[0])
                } else {
                    None
                }
            }
        };
        if let Some(bot) = pick {
            let target = if bot.key.conn == home_conn { bot.name.clone() } else { format!("{}@{}", bot.name, slug(&bot.conn_name, true)) };
            out.push(ResolvedMention { tag, key: bot.key.clone(), title: bot.title(), target });
        }
    }
    out
}

/// The note the official composer middleware appends so the active bot knows who is meant and
/// sends its own `message_agent` call. The user's text is never forwarded verbatim.
pub fn mention_note(resolved: &[ResolvedMention]) -> Option<String> {
    if resolved.is_empty() {
        return None;
    }
    let parts: Vec<String> = resolved
        .iter()
        .map(|m| format!("@{} = agent profile \"{}\" (\"{}\") (message_agent target: \"{}\")", m.tag, m.key.profile, m.title, m.target))
        .collect();
    Some(format!(
        "\n\n[@mentions resolved from the Bot Mode roster — the user is referring to: {}. If they want one of these agents contacted, compose your own message and send it with your message_agent tool; otherwise just answer, and if you can't reach them, say so.]",
        parts.join("; ")
    ))
}

/// Autocomplete: bots whose tags start with (or contain) `query`, best matches first.
pub fn complete_mention<'a>(query: &str, roster: &'a [Bot]) -> Vec<&'a Bot> {
    let q = query.to_lowercase();
    let mut scored: Vec<(u8, &Bot)> = roster
        .iter()
        .filter_map(|b| {
            let tags = b.tags();
            let title = b.title().to_lowercase();
            if tags.iter().any(|t| t.starts_with(&q)) {
                Some((0, b))
            } else if title.starts_with(&q) {
                Some((1, b))
            } else if tags.iter().any(|t| t.contains(&q)) || title.contains(&q) {
                Some((2, b))
            } else {
                None
            }
        })
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.hidden().cmp(&b.1.hidden())).then_with(|| a.1.title().cmp(&b.1.title())));
    scored.into_iter().map(|(_, b)| b).collect()
}

/// Bot Chat composer rewrite: `/new` and `/reset` compact instead of forking the forever-chat.
pub fn rewrite_bot_chat_command(text: &str) -> Option<String> {
    let t = text.trim();
    let (cmd, rest) = t.split_once(char::is_whitespace).unwrap_or((t, ""));
    match cmd {
        "/new" | "/reset" => Some(format!("/compact {}", rest.trim()).trim().to_owned()),
        _ => None,
    }
}

/// Activity-ordered roster: pinned first, then most recent activity.
pub fn sort_roster(bots: &mut [Bot]) {
    bots.sort_by(|a, b| {
        b.meta.pinned.unwrap_or(false).cmp(&a.meta.pinned.unwrap_or(false))
            .then(b.last_active().partial_cmp(&a.last_active()).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.title().cmp(&b.title()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(name: &str, title: Option<&str>) -> Bot {
        let mut row = json!({"name": name, "is_default": name == "default"});
        if let Some(t) = title {
            row["ui_meta"] = json!({"hermes-bots": {"title": t, "sectionId": "s1", "face": {"lock": true}}});
            row["ui_meta_revisions"] = json!({"hermes-bots": 3});
        }
        Bot::from_row("local", "Mac", &row).unwrap()
    }

    #[test]
    fn parses_profiles_list_row() {
        let row = json!({
            "name":"researcher","is_default":false,"model":"m","description":"digs",
            "canonical_session":{"id":"s1","title":"Bot Chat","preview":"hi there","last_active":20.0},
            "last_session":{"id":"s2","title":"x","preview":"older","last_active":10.0},
            "ui_meta":{"hermes-bots":{"title":"Research Buddy","hidden":false,"unknownKey":1}},
            "ui_meta_revisions":{"hermes-bots":7},"has_avatar":true
        });
        let b = Bot::from_row("c", "Mac", &row).unwrap();
        assert_eq!(b.title(), "Research Buddy");
        assert_eq!(b.handle(), "research-buddy");
        assert_eq!(b.meta_revision, Some(7));
        assert_eq!(b.preview(), "hi there");
        assert_eq!(b.last_active(), 20.0);
        assert!(b.meta.extra.contains_key("unknownKey"));
        assert!(b.tags().contains(&"researchbuddy".to_string()));
    }

    #[test]
    fn unknown_meta_round_trips() {
        let b = bot("x", Some("X Ray"));
        let v = serde_json::to_value(&b.meta).unwrap();
        assert_eq!(v["face"]["lock"], json!(true));
        assert_eq!(v["sectionId"], json!("s1"));
    }

    #[test]
    fn default_is_hermes() {
        let b = bot("default", None);
        assert_eq!(b.handle(), "hermes");
        assert_eq!(b.title(), "Hermes");
        assert!(b.tags().contains(&"hermes".into()));
    }

    #[test]
    fn agent_notice_parses() {
        let n = parse_agent_notice("Message from 🤖 Research Buddy (@research-buddy): Found 3 papers.\nMore.").unwrap();
        assert_eq!(n.name, "Research Buddy");
        assert_eq!(n.handle, "research-buddy");
        assert_eq!(n.body, "Found 3 papers.\nMore.");
        let n = parse_agent_notice("Message from 🤖 hermes (@hermes@homelab): yo").unwrap();
        assert_eq!(n.handle, "hermes@homelab");
        assert!(parse_agent_notice("I got a Message from 🤖 X (@x): quoted").is_none());
    }

    #[test]
    fn mentions_skip_emails() {
        assert_eq!(mentions("hey @Scribe and @dr-foo, mail a@b.com @x@homelab."), vec!["scribe", "dr-foo", "x@homelab"]);
    }

    #[test]
    fn resolve_prefers_profile_name() {
        let roster = vec![bot("scribe", None), bot("writer", Some("Scribe"))];
        let r = resolve_mentions("ask @scribe", &roster, "local");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key.profile, "scribe");
        let r = resolve_mentions("ask @research-buddy and @nobody", &[bot("rb", Some("Research Buddy"))], "local");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].target, "rb");
        let note = mention_note(&r).unwrap();
        assert!(note.contains("message_agent target: \"rb\""));
    }

    #[test]
    fn ambiguous_title_is_not_guessed() {
        let mut a = bot("a", Some("Twin"));
        a.key.conn = "other".into();
        let b = bot("b", Some("Twin"));
        let mut c = b.clone();
        c.name = "c".into();
        c.key.profile = "c".into();
        assert!(resolve_mentions("@twin", &[b, c], "local").is_empty());
        let _ = a;
    }

    #[test]
    fn new_becomes_compact() {
        assert_eq!(rewrite_bot_chat_command("/new").as_deref(), Some("/compact"));
        assert_eq!(rewrite_bot_chat_command("/reset focus on X").as_deref(), Some("/compact focus on X"));
        assert_eq!(rewrite_bot_chat_command("/newish"), None);
        assert_eq!(rewrite_bot_chat_command("hello"), None);
    }

    #[test]
    fn completion_orders_prefix_first() {
        let roster = vec![bot("alpha", None), bot("beta", Some("Alpine Guide"))];
        let r = complete_mention("al", &roster);
        assert_eq!(r.len(), 2);
        let r = complete_mention("guide", &roster);
        assert_eq!(r[0].name, "beta");
    }
}
