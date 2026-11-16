//! Regression tests for the 0.6.0 review fixes, against the mock over real HTTP/WebSocket.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hermes_core::auth::{Auth, Credential, Stored};
use hermes_core::rest::Rest;

fn mock() -> (tokio::runtime::Runtime, hermes_mock::Mock, String) {
    std::env::set_var("HERMES_DESKTOP_EPHEMERAL_SECRETS", "1");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let m = hermes_mock::Mock::new("tok-123");
    m.set_pace(1);
    let addr = rt.block_on(m.clone().spawn("127.0.0.1:0"));
    (rt, m, format!("http://{addr}"))
}

fn rest(url: &str) -> Rest {
    let st = Stored { cred: Credential::Token { token: "tok-123".into() }, ..Default::default() };
    Rest::new(Auth::new("fx", url::Url::parse(url).unwrap(), st, false)).unwrap()
}

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hd-{tag}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn no_parts(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".part"))
}

#[test]
fn downloads_are_unique_flagged_and_clean_up() {
    let (rt, _m, url) = mock();
    let r = rest(&url);
    let dir = tmpdir("dl");
    let a = rt.block_on(r.download("/home/hermes/out/report.pdf", "default", &dir, None, |_, _| {})).unwrap();
    let b = rt.block_on(r.download("/home/hermes/out/report.pdf", "default", &dir, None, |_, _| {})).unwrap();
    assert_eq!(a.file_name().unwrap(), "report.pdf");
    assert_eq!(b.file_name().unwrap(), "report (2).pdf");
    assert!(hermes_core::files::is_marked(&a) && hermes_core::files::is_marked(&b));
    // Save dialog path: replaces exactly what the user chose.
    let chosen = dir.join("my name.pdf");
    std::fs::write(&chosen, b"old").unwrap();
    let c = rt.block_on(r.download("/home/hermes/out/report.pdf", "default", &dir, Some(&chosen), |_, _| {})).unwrap();
    assert_eq!(c, chosen);
    assert_ne!(std::fs::read(&chosen).unwrap(), b"old");
    // A failed download leaves nothing behind.
    assert!(rt.block_on(r.download("/nope/missing.bin", "default", &dir, None, |_, _| {})).is_err());
    assert!(no_parts(&dir));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn media_cache_is_per_server_and_profile() {
    let base = Path::new("/c");
    let a = hermes_core::service::media_cache_path(base, "s1", "default", "/tmp/chart.png");
    let b = hermes_core::service::media_cache_path(base, "s2", "default", "/tmp/chart.png");
    let c = hermes_core::service::media_cache_path(base, "s1", "coder", "/tmp/chart.png");
    assert!(a != b && a != c && b != c);
    assert_eq!(a.extension().unwrap(), "png");
    assert_eq!(hermes_core::service::media_cache_path(base, "s", "p", "/x/evil.../../a").extension().unwrap(), "img");
    let _ = (Arc::new(()), Duration::ZERO);
}

// ---- service-level helpers --------------------------------------------------------------------

use hermes_core::conn::{ConnKind, Connection};
use hermes_core::gateway::LinkState;
use hermes_core::roster::BotKey;
use hermes_core::service::{Cmd, Ev, Service};
use hermes_core::store::Store;

fn start(url: &str, id: &str) -> Service {
    std::env::set_var(format!("HERMES_TOKEN_{}", id.to_uppercase()), "tok-123");
    let svc = Service::start(Arc::new(Store::memory().unwrap()), tmpdir("svc")).unwrap();
    svc.send(Cmd::Connect(Connection { id: id.into(), name: "Mock".into(), base_url: url.into(), kind: ConnKind::Gateway }));
    svc
}

fn wait<T>(svc: &Service, secs: u64, mut f: impl FnMut(&Ev) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "timed out waiting for event");
        let ev = rt.block_on(async { tokio::time::timeout(left, svc.ev.recv()).await.ok() }).expect("timeout").expect("closed");
        if std::env::var_os("TRACE_EV").is_some() {
            eprintln!("EV {ev:?}");
        }
        if let Some(t) = f(&ev) {
            return t;
        }
    }
}

fn ready(svc: &Service, conn: &str) {
    wait(svc, 10, |e| matches!(e, Ev::Roster { conn: c, .. } if c == conn).then_some(()));
}

#[test]
fn a_reply_survives_a_dropped_connection() {
    let (_rt, m, url) = mock();
    m.set_pace(40);
    let svc = start(&url, "r1");
    ready(&svc, "r1");
    let key = BotKey::new("r1", "default");
    svc.send(Cmd::OpenBot(key.clone()));
    wait(&svc, 10, |e| matches!(e, Ev::History { cached: false, .. }).then_some(()));
    svc.send(Cmd::Send { key: key.clone(), text: "hello there".into(), files: vec![], refresh: None, slash: false });
    wait(&svc, 10, |e| matches!(e, Ev::Delta { .. }).then_some(()));
    m.drop_sockets();
    wait(&svc, 10, |e| matches!(e, Ev::Link { state: LinkState::Reconnecting { .. }, .. }).then_some(()));
    // After the reconnect the turn is replayed from `inflight` and then finishes normally.
    let mut replayed = String::new();
    let mut connected = false;
    let full = wait(&svc, 20, |e| match e {
        Ev::Link { state: LinkState::Connected, .. } => {
            connected = true;
            None
        }
        Ev::Delta { text, .. } if connected => {
            replayed.push_str(text);
            None
        }
        Ev::TurnEnd { text, error: None, .. } => Some(text.clone()),
        _ => None,
    });
    assert!(connected, "reconnected before the turn ended");
    assert!(full.starts_with("You said: *hello there*"), "{full}");
    assert!(replayed.starts_with("You said:"), "inflight text was replayed: {replayed}");
}

#[test]
fn pending_approval_comes_back_after_a_drop() {
    let (_rt, m, url) = mock();
    let svc = start(&url, "r2");
    ready(&svc, "r2");
    let key = BotKey::new("r2", "default");
    svc.send(Cmd::OpenBot(key.clone()));
    wait(&svc, 10, |e| matches!(e, Ev::History { cached: false, .. }).then_some(()));
    svc.send(Cmd::Send { key: key.clone(), text: "please rm -rf build".into(), files: vec![], refresh: None, slash: false });
    let first = wait(&svc, 10, |e| match e {
        Ev::Approval { request_id, .. } => Some(request_id.clone()),
        _ => None,
    });
    m.drop_sockets();
    let again = wait(&svc, 20, |e| match e {
        Ev::Approval { request_id, key: k, .. } => Some((request_id.clone(), k.clone())),
        _ => None,
    });
    assert_eq!(again.0, first, "the same request is replayed from open_requests");
    assert_eq!(again.1.as_ref().map(|k| k.profile.as_str()), Some("default"));
    svc.send(Cmd::Approve { conn: "r2".into(), request_id: again.0, choice: "once".into() });
    let text = wait(&svc, 10, |e| match e {
        Ev::TurnEnd { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert!(text.contains("Removed"), "{text}");
}

#[test]
fn concurrent_opens_create_one_bot_chat() {
    let (_rt, m, url) = mock();
    let svc = start(&url, "r3");
    ready(&svc, "r3");
    assert_eq!(m.bot_chats("scribe"), 0);
    let key = BotKey::new("r3", "scribe");
    for _ in 0..3 {
        svc.send(Cmd::OpenBot(key.clone()));
    }
    let mut n = 0;
    wait(&svc, 10, |e| {
        if matches!(e, Ev::History { cached: false, .. }) {
            n += 1;
        }
        (n == 3).then_some(())
    });
    assert_eq!(m.bot_chats("scribe"), 1);
}

#[test]
fn a_message_that_starts_with_a_path_is_sent_as_text() {
    let (_rt, _m, url) = mock();
    let svc = start(&url, "sl");
    ready(&svc, "sl");
    let key = BotKey::new("sl", "default");
    svc.send(Cmd::OpenBot(key.clone()));
    wait(&svc, 10, |e| matches!(e, Ev::History { cached: false, .. }).then_some(()));
    svc.send(Cmd::Send { key: key.clone(), text: "/etc/hosts is empty".into(), files: vec![], refresh: None, slash: false });
    let full = wait(&svc, 10, |e| match e {
        Ev::Slash { command, .. } => panic!("ran as a command: {command}"),
        Ev::TurnEnd { text, error: None, .. } if !text.is_empty() => Some(text.clone()),
        _ => None,
    });
    assert!(full.contains("/etc/hosts is empty"), "{full}");
}
