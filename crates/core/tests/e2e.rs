//! End to end: the service against the mock backend over real HTTP and WebSocket.

use std::sync::Arc;
use std::time::Duration;

use hermes_core::conn::{ConnKind, Connection};
use hermes_core::roster::BotKey;
use hermes_core::service::{Cmd, Ev, LocalFile, Service};
use hermes_core::store::Store;
use hermes_core::transcript::Role;

fn mock_rt() -> (tokio::runtime::Runtime, hermes_mock::Mock, String) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let m = hermes_mock::Mock::new("tok-123");
    m.set_pace(1);
    let addr = rt.block_on(m.clone().spawn("127.0.0.1:0"));
    (rt, m, format!("http://{addr}"))
}

fn start(url: &str, id: &str, token: &str) -> Service {
    std::env::set_var(format!("HERMES_TOKEN_{}", id.to_uppercase()), token);
    let dir = std::env::temp_dir().join(format!("hd-test-{}", uuid_like()));
    let svc = Service::start(Arc::new(Store::memory().unwrap()), dir).unwrap();
    svc.send(Cmd::Connect(Connection { id: id.into(), name: "Mock".into(), base_url: url.into(), kind: ConnKind::Gateway }));
    svc
}

fn uuid_like() -> String {
    format!("{:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
}

/// Wait for an event matching `f`, failing after a timeout.
fn wait<T>(svc: &Service, secs: u64, mut f: impl FnMut(&Ev) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "timed out waiting for event");
        let ev = futures_block(svc.ev.recv(), left).expect("timeout").expect("channel closed");
        if let Ev::Error { message, .. } = &ev {
            eprintln!("service error: {message}");
        }
        if let Some(t) = f(&ev) {
            return t;
        }
    }
}

fn futures_block<F: std::future::Future>(f: F, d: Duration) -> Option<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    rt.block_on(async { tokio::time::timeout(d, f).await.ok() })
}

#[test]
fn roster_chat_stream_mentions_files_compact() {
    let (_rt, mock, url) = mock_rt();
    let svc = start(&url, "m1", "tok-123");

    // Roster from profiles.list, with Bot Mode titles.
    let bots = wait(&svc, 10, |e| match e {
        Ev::Roster { bots, .. } => Some(bots.clone()),
        _ => None,
    });
    let titles: Vec<String> = bots.iter().map(|b| b.title()).collect();
    assert!(titles.contains(&"Research Buddy".to_string()), "{titles:?}");
    assert!(bots.iter().any(|b| b.name == "archivist" && b.hidden()));
    // Most recent activity first.
    assert_eq!(bots[0].name, "default");
    assert_eq!(bots[1].name, "coder");

    // Open the default bot's canonical Bot Chat: history over REST, notice detected.
    let key = BotKey::new("m1", "default");
    svc.send(Cmd::OpenBot(key.clone()));
    let msgs = wait(&svc, 10, |e| match e {
        Ev::History { key: k, messages, cached: false } if *k == key => Some(messages.clone()),
        _ => None,
    });
    assert_eq!(msgs.len(), 4);
    assert!(matches!(&msgs[2].role, Role::Agent(n) if n.name == "Research Buddy"));

    // Stream a reply with tool activity.
    svc.send(Cmd::Send { key: key.clone(), text: "please search for gpui".into(), files: vec![] });
    let mut deltas = String::new();
    let mut saw_tool = false;
    let end = wait(&svc, 10, |e| match e {
        Ev::Delta { text, .. } => {
            deltas.push_str(text);
            None
        }
        Ev::Activity { activity, .. } if activity.name == "web_search" => {
            saw_tool = true;
            None
        }
        Ev::TurnEnd { key: k, text, error } if *k == key => Some((text.clone(), error.clone())),
        _ => None,
    });
    assert!(saw_tool);
    assert_eq!(end.1, None);
    assert_eq!(deltas, end.0);
    assert!(end.0.contains("You said"));

    // @mention: note appended for message_agent, and the teammate's reply comes back as an
    // attributed agent notice.
    svc.send(Cmd::Send { key: key.clone(), text: "ask @research-buddy about memory".into(), files: vec![] });
    let notice = wait(&svc, 10, |e| match e {
        Ev::Incoming { message, .. } => match &message.role {
            Role::Agent(n) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(notice.handle, "research-buddy");
    let sent = mock.messages_of("default");
    let last_user = sent.iter().rev().find(|m| m["role"] == "user" && !m["content"].as_str().unwrap().starts_with("Message from")).unwrap();
    assert!(last_user["content"].as_str().unwrap().contains("message_agent target: \"researcher\""));

    // Files: an image goes through /api/chat/image-upload, others through file.attach.
    let dir = std::env::temp_dir().join(format!("hd-files-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = dir.join("pic.png");
    std::fs::write(&png, hermes_mock::demo_png(8, 8)).unwrap();
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, "hello").unwrap();
    let files = vec![LocalFile::from_path(png).unwrap(), LocalFile::from_path(txt).unwrap()];
    svc.send(Cmd::Send { key: key.clone(), text: "see attached".into(), files });
    wait(&svc, 10, |e| matches!(e, Ev::TurnEnd { error: None, .. }).then_some(()));
    let sent = mock.messages_of("default");
    let u = sent.iter().rev().find(|m| m["role"] == "user").unwrap();
    assert!(u["content"].as_str().unwrap().contains("@file:/home/hermes/uploads/notes.txt"));
    assert!(u["attachments"][0]["path"].as_str().unwrap().starts_with("/home/hermes/uploads/"));

    // /new in a Bot Chat compacts instead of forking.
    svc.send(Cmd::Send { key: key.clone(), text: "/new".into(), files: vec![] });
    wait(&svc, 10, |e| matches!(e, Ev::Info { message } if message.contains("compacted")).then_some(()));
    assert!(mock.messages_of("default").iter().any(|m| m["display_kind"] == "context_compaction"));
}

#[test]
fn creates_bot_chat_and_writes_meta() {
    let (_rt, mock, url) = mock_rt();
    let svc = start(&url, "m2", "tok-123");
    wait(&svc, 10, |e| matches!(e, Ev::Roster { .. }).then_some(()));
    // scribe has no Bot Chat yet: it is created (hidden, titled "Bot Chat").
    let key = BotKey::new("m2", "scribe");
    svc.send(Cmd::OpenBot(key.clone()));
    let msgs = wait(&svc, 10, |e| match e {
        Ev::History { key: k, messages, cached: false } if *k == key => Some(messages.clone()),
        _ => None,
    });
    assert!(msgs.is_empty());
    svc.send(Cmd::Send { key: key.clone(), text: "hello".into(), files: vec![] });
    wait(&svc, 10, |e| matches!(e, Ev::TurnEnd { .. }).then_some(()));
    assert_eq!(mock.messages_of("scribe").len(), 2);

    // ui_meta write is a CAS merge that keeps other keys.
    svc.send(Cmd::SetMeta { key: key.clone(), patch: serde_json::json!({"hidden": true}) });
    wait(&svc, 10, |e| match e {
        Ev::Roster { bots, .. } => bots.iter().find(|b| b.key == key && b.hidden()).map(|_| ()),
        _ => None,
    });
    let meta = mock.ui_meta_of("scribe");
    assert_eq!(meta["hermes-bots"]["title"], "Scribe");
    assert_eq!(meta["hermes-bots"]["hidden"], true);

    // Remote bot creation through the official profiles.create RPC.
    svc.send(Cmd::CreateBot { conn: "m2".into(), name: "planner".into(), title: "Trip Planner".into(), description: "Plans trips".into() });
    wait(&svc, 10, |e| match e {
        Ev::Roster { bots, .. } => bots.iter().find(|b| b.name == "planner" && b.title() == "Trip Planner").map(|_| ()),
        _ => None,
    });
}

#[test]
fn bad_token_is_unauthorized() {
    let (_rt, _mock, url) = mock_rt();
    let svc = start(&url, "m3", "wrong");
    wait(&svc, 10, |e| matches!(e, Ev::Link { state: hermes_core::gateway::LinkState::Unauthorized, .. }).then_some(()));
}

#[test]
fn approval_round_trip() {
    let (_rt, _mock, url) = mock_rt();
    let svc = start(&url, "m4", "tok-123");
    wait(&svc, 10, |e| matches!(e, Ev::Roster { .. }).then_some(()));
    let key = BotKey::new("m4", "coder");
    svc.send(Cmd::OpenBot(key.clone()));
    svc.send(Cmd::Send { key: key.clone(), text: "run rm -rf build".into(), files: vec![] });
    let (conn, id) = wait(&svc, 10, |e| match e {
        Ev::Approval { conn, request_id, command, .. } => {
            assert_eq!(command, "rm -rf build/");
            Some((conn.clone(), request_id.clone()))
        }
        _ => None,
    });
    svc.send(Cmd::Approve { conn, request_id: id, choice: "deny".into() });
    let t = wait(&svc, 10, |e| match e {
        Ev::TurnEnd { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert!(t.contains("won't"));
}

fn rpc(svc: &Service, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let tag = format!("t-{method}-{}", uuid_like());
    svc.send(Cmd::Rpc { conn: "m5".into(), method: method.into(), params, tag: tag.clone() });
    wait(svc, 10, |e| match e {
        Ev::Rpc { tag: t, result, .. } if *t == tag => Some(result.clone()),
        _ => None,
    })
}

#[test]
fn apps_page_uses_official_catalogs() {
    use hermes_core::apps;
    let (_rt, _mock, url) = mock_rt();
    let svc = start(&url, "m5", "tok-123");
    wait(&svc, 10, |e| matches!(e, Ev::Roster { .. }).then_some(()));

    let cat = rpc(&svc, "connectors.catalog", apps::catalog_params("coder")).unwrap();
    let list = rpc(&svc, "connectors.list", apps::connectors_list_params("coder")).unwrap();
    let items = apps::connectors(&cat, Some(&list));
    assert!(items.iter().any(|i| i.id == "github" && i.connected));
    assert!(items.iter().any(|i| i.id == "gmail" && !i.connected && apps::is_recommended(i)));

    let r = rpc(&svc, "connectors.connect", apps::connect_params("coder", "gmail")).unwrap();
    assert_eq!(apps::connect_url(&r).as_deref(), Some("https://connect.example.invalid/gmail"));

    let mcp = apps::mcp(&rpc(&svc, "mcp.catalog", apps::catalog_params("coder")).unwrap());
    assert!(mcp.iter().any(|i| i.id == "brave_search" && i.requires == vec!["BRAVE_API_KEY"]));
    rpc(&svc, "mcp.servers.add", apps::mcp_add_params("coder", "playwright")).unwrap();
    let mcp = apps::mcp(&rpc(&svc, "mcp.catalog", apps::catalog_params("coder")).unwrap());
    assert!(mcp.iter().any(|i| i.id == "playwright" && i.connected));

    rpc(&svc, "tools.configure", apps::toolset_params("coder", "web", false)).unwrap();
    let ts = apps::toolsets(&rpc(&svc, "toolsets.list", apps::catalog_params("coder")).unwrap());
    assert!(ts.iter().any(|i| i.id == "web" && !i.connected));
    assert!(!apps::skills(&rpc(&svc, "skills.manage", apps::skills_list_params("coder")).unwrap()).is_empty());

    // Unknown methods come back as a readable error, never a hang.
    let e = rpc(&svc, "nonexistent.method", serde_json::json!({})).unwrap_err();
    assert!(e.contains("nonexistent.method"), "{e}");
}

#[test]
fn threads_open_their_own_session() {
    let (_rt, mock, url) = mock_rt();
    let svc = start(&url, "m6", "tok-123");
    wait(&svc, 10, |e| matches!(e, Ev::Roster { .. }).then_some(()));
    let tag = "threads".to_owned();
    svc.send(Cmd::Rpc { conn: "m6".into(), method: "session.list".into(), params: serde_json::json!({"profile": "coder", "limit": 30}), tag: tag.clone() });
    let rows = wait(&svc, 10, |e| match e {
        Ev::Rpc { tag: t, result: Ok(v), .. } if *t == tag => Some(v["sessions"].as_array().cloned().unwrap_or_default()),
        _ => None,
    });
    let ids: Vec<&str> = rows.iter().filter_map(|r| r["id"].as_str()).collect();
    assert_eq!(ids, vec!["s-coder-old", "s-coder-tg"], "most recent first, Bot Chat excluded");

    let key = BotKey::new("m6", "coder").with_thread(Some("s-coder-tg".into()));
    svc.send(Cmd::OpenBot(key.clone()));
    let msgs = wait(&svc, 10, |e| match e {
        Ev::History { key: k, messages, cached: false } if *k == key => Some(messages.clone()),
        _ => None,
    });
    assert!(msgs.iter().any(|m| m.text.contains("bump the version")));

    svc.send(Cmd::Send { key: key.clone(), text: "and after that?".into(), files: vec![] });
    wait(&svc, 10, |e| matches!(e, Ev::TurnEnd { key: k, .. } if *k == key).then_some(()));
    assert!(mock.messages_of("coder").iter().all(|m| m["content"] != "and after that?"), "the Bot Chat was not touched");
}
