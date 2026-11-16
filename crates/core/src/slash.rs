//! Slash commands for the composer popover.
//!
//! Live source: the gateway's `commands.catalog {profile}` (`tui_gateway/command_discovery.py`),
//! which also lists the profile's skills, quick commands and plugin commands. Offline fallback:
//! `slash_registry.json`, generated from the official `hermes_cli/commands.py` registry by
//! `scripts/gen_slash_registry.py` (every command a messaging gateway such as Discord serves).
//! Commands run through `slash.exec` (see `service.rs`).

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SlashCmd {
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Argument hint, e.g. `[model|reset] [--global]`.
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub subcommands: Vec<String>,
    /// Only meaningful inside a messaging app (Telegram topics, `/sethome`…).
    #[serde(default)]
    pub gateway_only: bool,
    /// Official desktop disposition: `None` = offered, `hidden`, or a reason it isn't offered.
    #[serde(default)]
    pub desktop: Option<String>,
}

/// The embedded copy of the official registry.
/// Whether `text` invokes a known command: `/name` or `/name args`, where `name` (or an alias)
/// is in `cmds`. A message that merely starts with a slash, such as a path (`/etc/hosts is
/// empty`) or `/s` typo, stays an ordinary message.
pub fn is_command(cmds: &[SlashCmd], text: &str) -> bool {
    let Some(rest) = text.trim_start().strip_prefix('/') else { return false };
    let name = rest.split(char::is_whitespace).next().unwrap_or_default();
    !name.is_empty() && cmds.iter().any(|c| c.name.eq_ignore_ascii_case(name) || c.aliases.iter().any(|a| a.trim_start_matches('/').eq_ignore_ascii_case(name)))
}

pub fn builtin() -> &'static [SlashCmd] {
    static CMDS: OnceLock<Vec<SlashCmd>> = OnceLock::new();
    CMDS.get_or_init(|| serde_json::from_str(include_str!("slash_registry.json")).unwrap_or_default())
}

/// Parse a `commands.catalog` result: `categories: [{name, pairs: [["/key", desc]]}]` and
/// `commands: {"/key": {argument_mode, desktop}}`. Argument hints come from the registry when the
/// command is a built-in (the live description already ends with its usage).
pub fn from_catalog(v: &Value) -> Vec<SlashCmd> {
    let meta = v.get("commands").and_then(Value::as_object);
    let mut out = Vec::new();
    let mut push = |key: &str, desc: &str, cat: &str| {
        let name = key.trim_start_matches('/').to_owned();
        if name.is_empty() || out.iter().any(|c: &SlashCmd| c.name == name) {
            return;
        }
        let reg = builtin().iter().find(|c| c.name == name);
        let desktop = meta.and_then(|m| m.get(key)).and_then(|m| m.get("desktop")).and_then(Value::as_str).map(str::to_owned);
        out.push(SlashCmd {
            description: strip_usage(desc, &name),
            category: cat.to_owned(),
            aliases: reg.map(|r| r.aliases.clone()).unwrap_or_default(),
            args: reg.map(|r| r.args.clone()).unwrap_or_default(),
            subcommands: reg.map(|r| r.subcommands.clone()).unwrap_or_default(),
            gateway_only: false,
            desktop: desktop.or_else(|| reg.and_then(|r| r.desktop.clone())),
            name,
        });
    };
    if let Some(cats) = v.get("categories").and_then(Value::as_array) {
        for c in cats {
            let cat = c.get("name").and_then(Value::as_str).unwrap_or("Commands");
            for pair in c.get("pairs").and_then(Value::as_array).into_iter().flatten() {
                if let (Some(k), Some(d)) = (pair.get(0).and_then(Value::as_str), pair.get(1).and_then(Value::as_str)) {
                    push(k, d, cat);
                }
            }
        }
    }
    // Skills arrive in `pairs` only.
    for pair in v.get("pairs").and_then(Value::as_array).into_iter().flatten() {
        if let (Some(k), Some(d)) = (pair.get(0).and_then(Value::as_str), pair.get(1).and_then(Value::as_str)) {
            push(k, d, "Skills");
        }
    }
    out
}

/// Live catalog first, then registry commands it does not list (the gateway-only ones a
/// messaging app offers), so the popover shows the full gateway set.
pub fn merged(live: &[SlashCmd]) -> Vec<SlashCmd> {
    let mut out = live.to_vec();
    for c in builtin() {
        if !out.iter().any(|x| x.name == c.name || x.aliases.contains(&c.name)) {
            out.push(c.clone());
        }
    }
    out
}

/// Drop a trailing "usage" clause the CLI description adds (`… Usage: /name <args>`).
fn strip_usage(desc: &str, name: &str) -> String {
    for marker in [" — usage: ", " (usage: ", " Usage: /", " usage: /"] {
        if let Some(i) = desc.find(marker) {
            return desc[..i].trim().to_owned();
        }
    }
    let _ = name;
    desc.trim().to_owned()
}

/// Commands matching what the user typed after `/`: name prefix first, then alias prefix,
/// then description words. Every command is offered, including ones the official desktop hides
/// behind its own pickers (Hermes still runs them).
pub fn filter<'a>(cmds: &'a [SlashCmd], query: &str) -> Vec<&'a SlashCmd> {
    let q = query.trim().to_lowercase();
    let visible = |_: &&SlashCmd| true;
    let mut a: Vec<&SlashCmd> = cmds.iter().filter(visible).filter(|c| c.name.to_lowercase().starts_with(&q)).collect();
    let b = cmds.iter().filter(visible).filter(|c| !a.contains(c) && c.aliases.iter().any(|x| x.to_lowercase().starts_with(&q)));
    let b: Vec<&SlashCmd> = b.collect();
    let c: Vec<&SlashCmd> = if q.len() >= 2 {
        cmds.iter().filter(visible).filter(|c| !a.contains(c) && !b.contains(c) && c.description.to_lowercase().contains(&q)).collect()
    } else {
        vec![]
    };
    a.extend(b);
    a.extend(c);
    a
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_known_commands_are_commands() {
        let cmds = builtin();
        assert!(is_command(cmds, "/model"));
        assert!(is_command(cmds, "/model gpt-5 --global"));
        assert!(is_command(cmds, "/MODEL"));
        for not in ["/etc/hosts is empty", "/usr/bin/env python", "/", "/ model", "hello /model", "//model", "/notacommand"] {
            assert!(!is_command(cmds, not), "{not}");
        }
    }

    use super::*;
    use serde_json::json;

    #[test]
    fn registry_and_catalog() {
        let reg = builtin();
        assert!(reg.len() > 50, "embedded registry loads");
        assert!(reg.iter().any(|c| c.name == "status"));
        assert!(reg.iter().any(|c| c.name == "sethome" && c.gateway_only));
        let cat = json!({
            "pairs": [["/status", "Show session info"], ["/arxiv", "Search arXiv papers"]],
            "categories": [{"name": "Session", "pairs": [["/status", "Show session info"]]}],
            "commands": {"/status": {"argument_mode": null, "desktop": null}},
        });
        let live = from_catalog(&cat);
        assert_eq!(live[0].name, "status");
        assert_eq!(live[1].category, "Skills");
        let all = merged(&live);
        assert!(all.iter().any(|c| c.name == "sethome"));
        assert_eq!(filter(&all, "sta")[0].name, "status");
        assert!(filter(&all, "").len() > 50);
        assert_eq!(filter(&all, "mod")[0].name, "model");
    }
}
