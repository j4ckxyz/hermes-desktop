//! Live check against a real Hermes server. Never runs by default (`#[ignore]`).
//!
//!   HERMES_LIVE_URL=https://host HERMES_LIVE_USER_FILE=… HERMES_LIVE_PASS_FILE=… \
//!     cargo test -p hermes-core --test live -- --ignored --nocapture
//!
//! Signs in with the dashboard's password provider (credentials kept in process memory only,
//! never in the keychain or on disk), then reads: the socket mode, the roster's canonical chat
//! ids, the thread list and the opened Bot Chat. The only write is one new session titled
//! "Hermes Desktop protocol test" with one short message. Prints ids, counts and times, never
//! message text or credentials.

use std::sync::Arc;
use std::time::Duration;

use hermes_core::conn::{ConnKind, Connection};
use hermes_core::gateway::LinkState;
use hermes_core::roster::BotKey;
use hermes_core::service::{Cmd, Ev, Service, SignInMethod};
use hermes_core::store::Store;
use hermes_core::wire;
use serde_json::{json, Value};

fn wait<T>(svc: &Service, secs: u64, what: &str, mut f: impl FnMut(&Ev) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "timed out waiting for {what}");
        let ev = rt.block_on(async { tokio::time::timeout(left, svc.ev.recv()).await.ok() }).expect("timeout").expect("closed");
        if let Ev::Error { message, .. } = &ev {
            println!("  service error: {message}");
        }
        if let Some(t) = f(&ev) {
            return t;
        }
    }
}

fn rpc(svc: &Service, conn: &str, method: &str, params: Value) -> Result<Value, String> {
    let tag = format!("live-{method}-{}", uuid_like());
    svc.send(Cmd::Rpc { conn: conn.into(), method: method.into(), params, tag: tag.clone() });
    wait(svc, 60, method, |e| match e {
        Ev::Rpc { tag: t, result, .. } if *t == tag => Some(result.clone()),
        _ => None,
    })
}

fn uuid_like() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
}

fn age(ts: Option<f64>) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    match ts {
        Some(t) if t > 0.0 => format!("{:.1} days ago", (now - t) / 86400.0),
        _ => "no time".into(),
    }
}

fn short(id: &str) -> String {
    id.chars().take(18).collect()
}

#[test]
#[ignore]
fn live_server() {
    let _ = env_logger::builder().is_test(true).filter_level(log::LevelFilter::Warn).try_init();
    let url = std::env::var("HERMES_LIVE_URL").expect("HERMES_LIVE_URL");
    let read = |k: &str| std::fs::read_to_string(std::env::var(k).unwrap_or_else(|_| panic!("{k}"))).unwrap().trim().to_owned();
    let (user, pass) = (read("HERMES_LIVE_USER_FILE"), read("HERMES_LIVE_PASS_FILE"));
    std::env::set_var("HERMES_DESKTOP_EPHEMERAL_SECRETS", "1");
    let dir = std::env::temp_dir().join(format!("hd-live-{}", uuid_like()));
    let svc = Service::start(Arc::new(Store::memory().unwrap()), dir).unwrap();
    let conn = "live";

    svc.send(Cmd::SignIn {
        conn_id: conn.into(),
        base: url.clone(),
        method: SignInMethod::Password { provider: "basic".into(), username: user, password: pass, remember: false },
        tag: "signin".into(),
    });
    let who = wait(&svc, 60, "sign-in", |e| match e {
        Ev::SignedIn { tag, result, .. } if tag == "signin" => Some(result.clone()),
        _ => None,
    });
    assert!(who.is_ok(), "sign-in failed: {:?}", who.err());
    println!("1. signed in");

    svc.send(Cmd::Connect(Connection { id: conn.into(), name: "live".into(), base_url: url, kind: ConnKind::Gateway }));
    wait(&svc, 60, "connection", |e| matches!(e, Ev::Link { conn: c, state: LinkState::Connected } if c == conn).then_some(()));
    let bots = wait(&svc, 60, "roster", |e| match e {
        Ev::Roster { bots, .. } => Some(bots.clone()),
        _ => None,
    });
    let main = bots.iter().find(|b| b.is_default).or(bots.first()).expect("a profile").clone();
    println!("2. connected; roster has {} profiles; main profile `{}`", bots.len(), main.name);

    // Socket mode, from a read-only probe: only the authority's session.list knows `scope`.
    let probe = rpc(&svc, conn, "session.list", json!({"profile": main.name, "limit": 1, "scope": "stored"}));
    let authority = probe.is_ok();
    println!("3. socket mode: {} ({})", if authority { "session authority" } else { "plain gateway" }, probe.err().unwrap_or_else(|| "scope accepted".into()));

    // Bug 2: the canonical chat's registry row versus its live tip.
    let canon = main.canonical.clone();
    match &canon {
        Some(c) => println!(
            "4. canonical Bot Chat: id {} resolved_id {} ({}); roster last_active {}",
            short(&c.id),
            c.resolved_id.as_deref().map(short).unwrap_or_else(|| "none".into()),
            if c.live_id() == c.id { "same session" } else { "compacted: tip differs" },
            age(c.last_active)
        ),
        None => println!("4. no canonical Bot Chat on `{}`", main.name),
    }
    if let Ok(r) = rpc(&svc, conn, "session.list", wire::list_params(authority, &main.name, Some("Bot Chat"), 20)) {
        let row = r["sessions"].get(0).cloned().unwrap_or(Value::Null);
        println!("   title lookup: id {} resolved_id {} started {}", short(row["id"].as_str().unwrap_or("-")), short(row["resolved_id"].as_str().unwrap_or("-")), age(row["started_at"].as_f64()));
    }

    // What the stream merges: the RPC listing's rows (ids, sources and counts only).
    if let Ok(r) = rpc(&svc, conn, "session.list", wire::list_params(authority, &main.name, None, 13)) {
        for row in r["sessions"].as_array().cloned().unwrap_or_default() {
            println!(
                "   listed {} source {} titled {} msgs {} started {} resolved {}",
                short(row["id"].as_str().unwrap_or("-")),
                row["source"].as_str().unwrap_or("-"),
                !row["title"].as_str().unwrap_or_default().is_empty(),
                row["message_count"],
                age(row["started_at"].as_f64()),
                row["resolved_id"].as_str().map(short).unwrap_or_else(|| "-".into())
            );
        }
    }

    // Open the bot as the app does (resume only; nothing is written) and date its newest message.
    if canon.is_some() {
        let key = BotKey::new(conn, &main.name);
        svc.send(Cmd::OpenBot(key.clone()));
        let msgs = wait(&svc, 120, "history", |e| match e {
            Ev::History { key: k, messages, cached: false } if *k == key => Some(messages.clone()),
            _ => None,
        });
        let last = msgs.iter().rev().find(|m| matches!(m.role, hermes_core::transcript::Role::User | hermes_core::transcript::Role::Assistant)).and_then(|m| m.timestamp);
        let markers = msgs.iter().filter(|m| m.id.starts_with("sess-")).count();
        println!("5. opened Bot Chat: {} messages in the stream from {} other conversations, newest {}", msgs.len(), markers, age(last));
    }

    // Thread list (REST, by last activity).
    svc.send(Cmd::Threads { key: BotKey::new(conn, &main.name), tag: "threads".into() });
    let rows = wait(&svc, 60, "threads", |e| match e {
        Ev::Rpc { tag, result, .. } if tag == "threads" => Some(result.clone()),
        _ => None,
    });
    match rows {
        Ok(v) => {
            let rows = v["sessions"].as_array().cloned().unwrap_or_default();
            let dated = rows.iter().filter(|r| r["last_active"].as_f64().is_some()).count();
            let ages: Vec<String> = rows.iter().take(4).map(|r| age(r["last_active"].as_f64())).collect();
            println!("6. thread list: {} rows, {} with last_active; newest: {}", rows.len(), dated, ages.join(", "));
            for r in rows.iter().take(6) {
                println!("   thread source {} msgs {} last active {}", r["source"].as_str().unwrap_or("-"), r["message_count"], age(r["last_active"].as_f64()));
            }
        }
        Err(e) => println!("6. thread list failed: {e}"),
    }

    if std::env::var_os("HERMES_LIVE_WRITE").is_none() {
        println!("7. read-only run (set HERMES_LIVE_WRITE=1 for the test session and message)");
        return;
    }
    // The one write: a clearly named test session, created with the exact keys the fixed client
    // sends (wire::create_params), then one short message through the app's send path.
    // Reuse the test session an earlier run created, if the gateway still holds it, so one run
    // after another never makes a second one.
    const TITLE: &str = "Hermes Desktop protocol test";
    let live = rpc(&svc, conn, "session.active_list", json!({"profile": main.name})).unwrap_or(Value::Null);
    let existing = live["sessions"].as_array().into_iter().flatten().find(|s| s["title"] == TITLE).and_then(|s| s["session_key"].as_str().map(str::to_owned));
    let stored = match existing {
        Some(k) => {
            println!("7. reusing test session {}", short(&k));
            k
        }
        None => {
            let mut params = wire::create_params(authority, &main.name, &format!("hd-live-{}", uuid_like()));
            params["title"] = json!(TITLE);
            params["hidden"] = json!(false);
            let created = rpc(&svc, conn, "session.create", params).expect("session.create");
            let k = created["stored_session_id"].as_str().or(created["session_id"].as_str()).unwrap().to_owned();
            println!("7. created test session {}", short(&k));
            k
        }
    };
    let key = BotKey::new(conn, &main.name).with_thread(Some(stored));
    svc.send(Cmd::OpenBot(key.clone()));
    wait(&svc, 120, "test session history", |e| matches!(e, Ev::History { key: k, cached: false, .. } if *k == key).then_some(()));
    svc.send(Cmd::Send { key: key.clone(), text: "Protocol test from Hermes Desktop 0.7.1. Please reply with just: OK".into(), files: vec![], refresh: None, slash: false });
    let (text, error) = wait(&svc, 300, "reply", |e| match e {
        Ev::TurnEnd { key: k, text, error } if *k == key => Some((text.clone(), error.clone())),
        _ => None,
    });
    assert_eq!(error, None, "send failed");
    println!("8. message sent; reply received ({} chars)", text.chars().count());
}
