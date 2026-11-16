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
