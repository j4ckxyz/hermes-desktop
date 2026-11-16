//! Unit tests for app state logic that doesn't need a window.

use hermes_core::roster::BotKey;

use crate::app::Approval;

fn approval(conn: &str, key: Option<BotKey>) -> Approval {
    Approval { conn: conn.into(), key, request_id: "r".into(), command: "rm -rf /".into(), description: String::new(), choices: vec![] }
}

#[test]
fn approve_only_reaches_the_visible_chat() {
    let a = BotKey::new("s1", "coder");
    let b = BotKey::new("s1", "scribe");
    let other_server = BotKey::new("s2", "coder");
    let for_a = approval("s1", Some(a.clone()));
    assert!(for_a.belongs_to(&a));
    assert!(!for_a.belongs_to(&b), "bot B's /approve must not answer bot A");
    assert!(!for_a.belongs_to(&other_server));
    assert!(for_a.waiting_on(&a) && !for_a.waiting_on(&b));
    // Not tied to a session: answerable from any chat on its own server only.
    let loose = approval("s1", None);
    assert!(loose.belongs_to(&a) && loose.belongs_to(&b) && !loose.belongs_to(&other_server));
    assert!(!loose.waiting_on(&a));
}

#[test]
fn long_break_needs_fresh_history() {
    use hermes_core::conn::{GapAction, Prefs};
    use hermes_core::transcript::{ChatMessage, Role};
    let p = Prefs { gap_action: GapAction::Compact, gap_hours: 6, ..Default::default() };
    let old = vec![crate::app::MsgVm { m: ChatMessage::new("1", Role::User, "hi", Some(0.0)), md: None }];
    let now = 7.0 * 3600.0;
    // Cached-only history might be stale: no refresh.
    assert_eq!(crate::app::gap_due(&old, false, &p, now), None);
    assert_eq!(crate::app::gap_due(&old, true, &p, now).map(|g| g.0), Some(GapAction::Compact));
    assert_eq!(crate::app::gap_due(&old, true, &p, 3600.0), None);
    let off = Prefs { gap_action: GapAction::Off, ..p };
    assert_eq!(crate::app::gap_due(&old, true, &off, now), None);
}

#[test]
fn history_refresh_merges_instead_of_replacing() {
    use crate::app::{merge_history, MsgVm};
    use hermes_core::transcript::{ChatMessage, LocalEvent, Role};
    let vm = |id: &str, role: Role, ts: f64| MsgVm { m: ChatMessage::new(id, role, id, Some(ts)), md: None };
    let chip = Role::Local(LocalEvent { icon: "file".into(), detail: None, code: None, path: None });
    let old = vec![vm("earlier", Role::User, 1.0), vm("a", Role::User, 10.0), vm("saved", chip, 11.0), vm("b", Role::Assistant, 12.0)];
    let fresh = vec![ChatMessage::new("a", Role::User, "a", Some(10.0)), ChatMessage::new("b", Role::Assistant, "b", Some(12.0))];
    let (out, tail) = merge_history(old, fresh);
    let ids: Vec<&str> = out.iter().map(|m| m.m.id.as_str()).collect();
    assert_eq!(ids, ["earlier", "a", "saved", "b"], "chips and older pages survive a refetch");
    assert!(!tail, "same last message: the view doesn't jump");
    let (out, tail) = merge_history(out, vec![ChatMessage::new("b", Role::Assistant, "b", Some(12.0)), ChatMessage::new("c", Role::User, "c", Some(13.0))]);
    assert_eq!(out.last().unwrap().m.id, "c");
    assert!(tail);
}

#[test]
fn every_icon_button_has_a_name() {
    // Each icon used for an icon-only button in the UI gets a tooltip / accessible label.
    for name in ["x", "arrow-left", "chart-column", "download", "external-link", "eye", "eye-off", "layout-grid", "panel-left", "panel-right", "plus", "search", "settings", "share", "minus", "square", "copy"] {
        assert_ne!(crate::ui::icon_label(name), "Button", "{name}");
    }
}

#[test]
fn rpc_tags_survive_colons_in_ids() {
    let tag = crate::app::rpc_tag(&["apps-act", "mcp:github:prod", "connect"]);
    let mut p = tag.splitn(3, '\u{1f}');
    assert_eq!((p.next(), p.next(), p.next()), (Some("apps-act"), Some("mcp:github:prod"), Some("connect")));
}

#[test]
fn mention_lookup_never_slices_mid_character() {
    use crate::app::mention_query;
    assert_eq!(mention_query("hi @res", 7), Some((3, 7, "res")));
    let t = "ask @研究"; // each CJK char is 3 bytes
    for c in 0..=t.len() + 2 {
        let _ = mention_query(t, c); // no panic at any byte offset
    }
    assert_eq!(mention_query(t, 9), Some((4, 8, "研")));
    assert_eq!(mention_query("mail a@b", 8), None, "not at a word start");
}

#[test]
fn usage_series_includes_the_servers_today() {
    use hermes_core::usage::{Day, Totals};
    let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
    // The server (UTC) is already on the 16th.
    let daily = vec![Day { day: "2026-11-16".into(), t: Totals { input_tokens: 7, ..Default::default() } }];
    let s = crate::overlays::day_series(&daily, 3, today);
    assert_eq!(s.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>(), ["2026-11-14", "2026-11-15", "2026-11-16"]);
    assert_eq!(s.last().unwrap().1, 7);
    assert_eq!(crate::overlays::day_series(&[], 2, today).last().unwrap().0, "2026-11-15");
}
