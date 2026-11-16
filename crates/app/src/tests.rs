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
