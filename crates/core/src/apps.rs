//! The Apps page model, built only from Hermes' official per-profile catalogs:
//!
//! * hosted **connectors**: `connectors.catalog` (directory: slug, name, description, category,
//!   logo) joined with `connectors.list {owner:{type:"account"}}` (connection state). Connect is
//!   `connectors.connect`, which returns targets with a `connect_url` to open in the browser.
//! * curated **MCP servers**: `mcp.catalog` (presets with installed/enabled and required env keys).
//!   Install is `mcp.servers.add {name, preset}` and uninstall is `mcp.servers.remove`.
//! * built-in **toolsets**: `toolsets.list`, toggled with `tools.configure {action, names}`.
//! * **skills**: `skills.manage {action:"list"}` (installed skills by category).
//!
//! Wire source: `tui_gateway/contracts/connectors.py`, `connectors_operation.py`,
//! `tools_mcp_plugins.py`. Nothing here is invented: if a catalog call fails (for example hosted
//! connectors need a Nous account), that source is reported as unavailable.

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppKind {
    Connector,
    Mcp,
    Toolset,
    Skill,
}

impl AppKind {
    pub fn label(self) -> &'static str {
        match self {
            AppKind::Connector => "App",
            AppKind::Mcp => "MCP server",
            AppKind::Toolset => "Toolset",
            AppKind::Skill => "Skill",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppItem {
    pub kind: AppKind,
    /// Slug / preset / toolset / skill name: the identifier the official calls take.
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub logo_url: Option<String>,
    /// Connected (connector), installed (MCP), enabled (toolset), present (skill).
    pub connected: bool,
    /// The connection needs attention (expired, failed, revoked) or the MCP server is disabled.
    pub attention: Option<String>,
    /// MCP: env keys the preset needs.
    pub requires: Vec<String>,
}

/// What each source said, kept separate so the page can say which ones are unavailable.
#[derive(Clone, Debug, Default)]
pub struct AppsSnapshot {
    pub items: Vec<AppItem>,
    pub unavailable: Vec<(AppKind, String)>,
}

pub fn catalog_params(profile: &str) -> Value {
    json!({"profile": profile})
}
pub fn connectors_list_params(profile: &str) -> Value {
    json!({"profile": profile, "owner": {"type": "account"}})
}
pub fn connect_params(profile: &str, slug: &str) -> Value {
    json!({"profile": profile, "owner": {"type": "account"}, "connectors": [slug]})
}
pub fn mcp_add_params(profile: &str, preset: &str) -> Value {
    json!({"profile": profile, "name": preset, "preset": preset})
}
pub fn mcp_remove_params(profile: &str, name: &str) -> Value {
    json!({"profile": profile, "name": name})
}
pub fn toolset_params(profile: &str, name: &str, enable: bool) -> Value {
    json!({"profile": profile, "action": if enable { "enable" } else { "disable" }, "names": [name]})
}
pub fn skills_list_params(profile: &str) -> Value {
    json!({"profile": profile, "action": "list"})
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn titlecase(c: &str) -> String {
    let mut it = c.chars();
    match it.next() {
        Some(f) => f.to_uppercase().chain(it).collect::<String>().replace(['_', '-'], " "),
        None => String::new(),
    }
}

/// Join `connectors.catalog` with `connectors.list`.
pub fn connectors(catalog: &Value, list: Option<&Value>) -> Vec<AppItem> {
    let state: Vec<&Value> = list.and_then(|l| l.get("connectors")).and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
    let rows = catalog.get("connectors").and_then(Value::as_array).cloned().unwrap_or_default();
    rows.iter()
        .filter_map(|r| {
            let slug = s(r, "slug");
            if slug.is_empty() {
                return None;
            }
            let st = state.iter().find(|x| x.get("connector").and_then(Value::as_str) == Some(slug.as_str()));
            let connected = st.is_some_and(|x| x.get("connected").and_then(Value::as_bool).unwrap_or(false));
            let status = st.map(|x| s(x, "connection_status")).unwrap_or_default();
            let attention = match status.as_str() {
                "failed" | "expired" | "revoked" => Some(format!("Connection {status}")),
                _ => None,
            };
            let name = s(r, "name");
            Some(AppItem {
                kind: AppKind::Connector,
                name: if name.is_empty() { slug.clone() } else { name },
                id: slug,
                description: s(r, "description"),
                category: titlecase(&s(r, "category")),
                logo_url: r.get("logo_url").and_then(Value::as_str).map(str::to_owned).filter(|u| u.starts_with("https://")),
                connected,
                attention,
                requires: vec![],
            })
        })
        .collect()
}

pub fn mcp(catalog: &Value) -> Vec<AppItem> {
    catalog
        .get("servers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let installed = r.get("installed").and_then(Value::as_bool).unwrap_or(false);
                    let enabled = r.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                    AppItem {
                        kind: AppKind::Mcp,
                        id: s(r, "name"),
                        name: titlecase(&s(r, "name")),
                        description: s(r, "description"),
                        category: "MCP".into(),
                        logo_url: None,
                        connected: installed,
                        attention: (installed && !enabled).then(|| "Installed, disabled".to_owned()),
                        requires: r.get("requires").and_then(Value::as_array).map(|k| k.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn toolsets(list: &Value) -> Vec<AppItem> {
    list.get("toolsets")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| {
                    let n = r.get("tool_count").and_then(Value::as_u64).unwrap_or(0);
                    let desc = s(r, "description");
                    AppItem {
                        kind: AppKind::Toolset,
                        id: s(r, "name"),
                        name: titlecase(&s(r, "name")),
                        description: if desc.is_empty() { format!("{n} tools") } else { desc },
                        category: "Tools".into(),
                        logo_url: None,
                        connected: r.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                        attention: None,
                        requires: vec![],
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `skills.manage list` → `{skills: {category: [names]}}`.
pub fn skills(list: &Value) -> Vec<AppItem> {
    let mut out = vec![];
    if let Some(m) = list.get("skills").and_then(Value::as_object) {
        for (cat, names) in m {
            for n in names.as_array().into_iter().flatten().filter_map(Value::as_str) {
                out.push(AppItem {
                    kind: AppKind::Skill,
                    id: n.to_owned(),
                    name: titlecase(n),
                    description: format!("Skill · {}", titlecase(cat)),
                    category: "Skills".into(),
                    logo_url: None,
                    connected: true,
                    attention: None,
                    requires: vec![],
                });
            }
        }
    }
    out
}

/// The browser link a `connectors.connect` result asks the user to open, if any.
pub fn connect_url(result: &Value) -> Option<String> {
    result
        .get("targets")
        .and_then(Value::as_array)?
        .iter()
        .find_map(|t| t.get("connect_url").and_then(Value::as_str))
        .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
        .map(str::to_owned)
}

/// Category chips, in a stable order: connector categories first, then the built-in sources.
pub fn categories(items: &[AppItem]) -> Vec<String> {
    let mut cats: Vec<String> = vec![];
    for it in items.iter().filter(|i| i.kind == AppKind::Connector) {
        if !it.category.is_empty() && !cats.contains(&it.category) {
            cats.push(it.category.clone());
        }
    }
    cats.sort();
    for k in ["MCP", "Tools", "Skills"] {
        if items.iter().any(|i| i.category == k) {
            cats.push(k.into());
        }
    }
    cats
}

/// Filter by search text and category chip ("" = all).
pub fn filter<'a>(items: &'a [AppItem], query: &str, category: &str) -> Vec<&'a AppItem> {
    let q = query.trim().to_lowercase();
    items
        .iter()
        .filter(|i| category.is_empty() || i.category == category)
        .filter(|i| q.is_empty() || i.name.to_lowercase().contains(&q) || i.description.to_lowercase().contains(&q) || i.id.contains(&q))
        .collect()
}

/// "Recommended": hosted connectors and MCP presets the profile does not have yet.
pub fn is_recommended(i: &AppItem) -> bool {
    !i.connected && matches!(i.kind, AppKind::Connector | AppKind::Mcp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_connector_catalog_with_state() {
        let cat = json!({"connectors": [
            {"slug": "github", "name": "GitHub", "description": "Repos", "category": "developer", "logo_url": "https://x/gh.png"},
            {"slug": "gmail", "name": "Gmail", "description": "Mail", "category": "productivity", "logo_url": "javascript:alert(1)"},
            {"slug": "notion", "name": "Notion", "description": "Docs", "category": "productivity"}
        ]});
        let list = json!({"available": true, "connectors": [
            {"connector": "github", "enabled": true, "connected": true, "connection_status": "active", "status_reason": null, "gateway_disabled_tools": []},
            {"connector": "notion", "enabled": true, "connected": false, "connection_status": "expired", "status_reason": null, "gateway_disabled_tools": []}
        ]});
        let items = connectors(&cat, Some(&list));
        assert_eq!(items.len(), 3);
        assert!(items[0].connected && items[0].category == "Developer");
        assert_eq!(items[1].logo_url, None, "only https logos are loaded");
        assert_eq!(items[2].attention.as_deref(), Some("Connection expired"));
        assert_eq!(categories(&items), vec!["Developer", "Productivity"]);
        assert_eq!(filter(&items, "mail", "").len(), 1);
        assert_eq!(filter(&items, "", "Productivity").len(), 2);
        assert!(is_recommended(&items[1]) && !is_recommended(&items[0]));
    }

    #[test]
    fn parses_mcp_toolsets_skills_and_connect_url() {
        let m = mcp(&json!({"servers": [{"name": "linear", "description": "Issues", "installed": true, "enabled": false, "requires": ["LINEAR_API_KEY"], "transport": "http"}]}));
        assert_eq!(m[0].attention.as_deref(), Some("Installed, disabled"));
        assert_eq!(m[0].requires, vec!["LINEAR_API_KEY"]);
        let t = toolsets(&json!({"toolsets": [{"name": "web", "description": "", "tool_count": 3, "enabled": true}]}));
        assert_eq!((t[0].name.as_str(), t[0].description.as_str(), t[0].connected), ("Web", "3 tools", true));
        let sk = skills(&json!({"skills": {"research": ["arxiv", "paper_notes"]}}));
        assert_eq!(sk[1].name, "Paper notes");
        assert_eq!(connect_url(&json!({"targets": [{"name": "github", "connect_url": "https://connect.example/abc"}]})).as_deref(), Some("https://connect.example/abc"));
        assert_eq!(connect_url(&json!({"targets": [{"connect_url": "file:///etc/passwd"}]})), None);
        assert_eq!(toolset_params("coder", "web", false)["action"], "disable");
    }
}
