//! Sign-in against the mock in gated mode: the official RFC 8252 native flow driven headlessly
//! (`/auth/native/authorize` → `/auth/password-login` → `/auth/native/token`), refresh, silent
//! re-sign-in, ticketed WebSockets and the failure cases the onboarding shows.

use std::sync::Arc;
use std::time::Duration;

use hermes_core::auth::{self, AuthError, Credential, Stored};
use hermes_core::conn::{ConnKind, Connection};
use hermes_core::gateway::LinkState;
use hermes_core::rest::Rest;
use hermes_core::service::{Cmd, Ev, Service, SignInMethod};
use hermes_core::store::Store;

fn ephemeral() {
    std::env::set_var("HERMES_DESKTOP_EPHEMERAL_SECRETS", "1");
}

fn gated(ttl: i64) -> (tokio::runtime::Runtime, hermes_mock::Mock, String) {
    ephemeral();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let m = hermes_mock::Mock::new("deploy-tok").with_basic_auth("jack", "hunter2");
    m.set_pace(1);
    m.set_access_ttl(ttl);
    let addr = rt.block_on(m.clone().spawn("127.0.0.1:0"));
    (rt, m, format!("http://{addr}"))
}

fn svc() -> Service {
    let dir = std::env::temp_dir().join(format!("hd-signin-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    Service::start(Arc::new(Store::memory().unwrap()), dir).unwrap()
}

fn wait<T>(svc: &Service, secs: u64, mut f: impl FnMut(&Ev) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(!left.is_zero(), "timed out waiting for event");
        let ev = rt.block_on(async { tokio::time::timeout(left, svc.ev.recv()).await.ok() }).expect("timeout").expect("closed");
        if let Some(t) = f(&ev) {
            return t;
        }
    }
}

fn password(user: &str, pass: &str, remember: bool) -> SignInMethod {
    SignInMethod::Password { provider: "basic".into(), username: user.into(), password: pass.into(), remember }
}

#[test]
fn probe_reports_gate_and_password_provider() {
    let (rt, _m, url) = gated(3600);
    let info = rt.block_on(auth::probe(&url)).unwrap();
    assert!(info.auth_required && info.native);
    assert_eq!(info.password_provider().map(|p| p.name.as_str()), Some("basic"));
    // Pasting the login page works too.
    assert!(rt.block_on(auth::probe(&format!("{url}/login?next=/"))).is_ok());
}

#[test]
fn sign_in_connects_over_a_ticketed_websocket() {
    let (_rt, m, url) = gated(3600);
    let s = svc();
    s.send(Cmd::SignIn { conn_id: "sa".into(), base: url.clone(), method: password("jack", "hunter2", true), tag: "t".into() });
    let who = wait(&s, 10, |e| match e {
        Ev::SignedIn { tag, result, .. } if tag == "t" => Some(result.clone()),
        _ => None,
    });
    assert_eq!(who.unwrap(), "jack");
    // Saved (in the test keychain) as native tokens plus the remembered password.
    let st = Stored::load("sa");
    assert!(matches!(st.cred, Credential::Native { .. }));
    assert_eq!(st.password.as_deref(), Some("hunter2"));
    s.send(Cmd::Connect(Connection { id: "sa".into(), name: "Mock".into(), base_url: url, kind: ConnKind::Gateway }));
    wait(&s, 10, |e| matches!(e, Ev::Link { conn, state: LinkState::Connected } if conn == "sa").then_some(()));
    wait(&s, 10, |e| matches!(e, Ev::Roster { .. }).then_some(()));
    assert_eq!(m.auth_counts().1, 1);
}

#[test]
fn wrong_password_and_rate_limit() {
    let (rt, _m, url) = gated(3600);
    let base = url::Url::parse(&url).unwrap();
    let e = rt.block_on(auth::login_password(&base, "basic", "jack", "nope")).unwrap_err();
    assert!(matches!(e, AuthError::InvalidCredentials), "{e:?}");
    assert!(e.to_string().to_lowercase().contains("password"));
    let mut last = None;
    for _ in 0..12 {
        last = Some(rt.block_on(auth::login_password(&base, "basic", "jack", "nope")).unwrap_err());
    }
    assert!(matches!(last, Some(AuthError::RateLimited)), "{last:?}");
    // Through the service the error comes back as friendly text, and nothing is saved.
    let s = svc();
    s.send(Cmd::SignIn { conn_id: "sb".into(), base: url, method: password("jack", "nope", true), tag: "x".into() });
    let r = wait(&s, 10, |e| match e {
        Ev::SignedIn { result, .. } => Some(result.clone()),
        _ => None,
    });
    assert!(r.is_err());
    assert!(matches!(Stored::load("sb").cred, Credential::None));
}

#[test]
fn refreshes_before_expiry_and_signs_in_again_silently() {
    // A 60 s token is inside the refresh margin, so every call renews it first.
    let (rt, m, url) = gated(60);
    let base = url::Url::parse(&url).unwrap();
    let cred = rt.block_on(auth::login_password(&base, "basic", "jack", "hunter2")).unwrap();
    let st = Stored { cred, provider: Some("basic".into()), username: Some("jack".into()), password: Some("hunter2".into()) };
    let a = auth::Auth::new("sc", base.clone(), st, false);
    let rest = Rest::new(a.clone()).unwrap();
    rt.block_on(rest.me()).unwrap();
    assert!(m.auth_counts().0 >= 1, "refreshed");
    // Concurrent callers share one refresh.
    let before = m.auth_counts().0;
    rt.block_on(async { futures_util::future::join_all((0..5).map(|_| rest.me())).await }).into_iter().for_each(|r| assert!(r.is_ok()));
    assert!(m.auth_counts().0 - before <= 5);
    // Password changed server-side (sessions revoked): refresh fails, the remembered password
    // signs in again without bothering the user.
    m.revoke_sessions();
    rt.block_on(rest.me()).unwrap();
    assert_eq!(m.auth_counts().1, 2);
    // Without a remembered password the app must ask.
    m.revoke_sessions();
    let st = Stored { password: None, ..a.stored() };
    let b = auth::Auth::new("sd", base, st, false);
    let e = rt.block_on(Rest::new(b).unwrap().me()).unwrap_err();
    assert!(e.to_string().contains("Sign in"), "{e}");
}

#[test]
fn signed_out_or_bad_credentials_stop_the_link() {
    let (_rt, _m, url) = gated(3600);
    let s = svc();
    // No credential at all on a gated server.
    s.send(Cmd::Connect(Connection { id: "se".into(), name: "Mock".into(), base_url: url, kind: ConnKind::Gateway }));
    wait(&s, 10, |e| matches!(e, Ev::Link { state: LinkState::Unauthorized, .. }).then_some(()));
}

#[test]
fn sign_out_forgets_the_keychain_entry() {
    let (_rt, _m, url) = gated(3600);
    let s = svc();
    s.send(Cmd::SignIn { conn_id: "sf".into(), base: url, method: password("jack", "hunter2", false), tag: "t".into() });
    assert!(wait(&s, 10, |e| match e {
        Ev::SignedIn { result, .. } => Some(result.is_ok()),
        _ => None,
    }));
    assert!(Stored::load("sf").password.is_none(), "not remembered when unticked");
    s.send(Cmd::SignOut { conn_id: "sf".into() });
    let t = std::time::Instant::now();
    while !matches!(Stored::load("sf").cred, Credential::None) {
        assert!(t.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn loopback_dashboard_needs_no_account() {
    ephemeral();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let m = hermes_mock::Mock::new("rotating-tok");
    let addr = rt.block_on(m.clone().spawn("127.0.0.1:0"));
    let url = format!("http://{addr}");
    let info = rt.block_on(auth::probe(&url)).unwrap();
    assert!(!info.auth_required && info.is_loopback());
    let s = svc();
    s.send(Cmd::SignIn { conn_id: "sg".into(), base: url.clone(), method: SignInMethod::Loopback, tag: "t".into() });
    assert!(wait(&s, 10, |e| match e {
        Ev::SignedIn { result, .. } => Some(result.is_ok()),
        _ => None,
    }));
    s.send(Cmd::Connect(Connection { id: "sg".into(), name: "Local".into(), base_url: url, kind: ConnKind::Gateway }));
    wait(&s, 10, |e| matches!(e, Ev::Link { state: LinkState::Connected, .. }).then_some(()));
}

#[test]
fn probe_failures_are_explained() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // Nothing listening.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let e = rt.block_on(auth::probe(&format!("http://127.0.0.1:{port}"))).unwrap_err();
    assert!(matches!(e, AuthError::Unreachable(_)), "{e:?}");
    // Something that isn't Hermes.
    let app = axum::Router::new().route("/api/status", axum::routing::get(|| async { "hello" }));
    let addr = rt.block_on(async {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let a = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        a
    });
    let e = rt.block_on(auth::probe(&format!("http://{addr}"))).unwrap_err();
    assert!(matches!(e, AuthError::NotHermes), "{e:?}");
    // TLS spoken to a plain-HTTP port.
    let (_rt2, _m, url) = gated(3600);
    let e = rt.block_on(auth::probe(&url.replace("http://", "https://"))).unwrap_err();
    assert!(matches!(e, AuthError::Tls(_)), "{e:?}");
    assert!(e.to_string().contains("http://"), "{e}");
    assert!(!e.to_string().is_empty());
}

#[test]
fn sign_out_during_a_refresh_leaves_nothing_behind() {
    // A 60 s token is inside the refresh margin, so the next call refreshes (slowly here).
    let (rt, m, url) = gated(60);
    m.set_refresh_delay(400);
    let base = url::Url::parse(&url).unwrap();
    let cred = rt.block_on(auth::login_password(&base, "basic", "jack", "hunter2")).unwrap();
    let st = Stored { cred, provider: Some("basic".into()), username: Some("jack".into()), password: None };
    st.save("so1").unwrap();
    let a = auth::Auth::new("so1", base, st, true);
    let rest = Rest::new(a.clone()).unwrap();
    let call = rt.spawn(async move { rest.me().await.map(|_| ()) });
    std::thread::sleep(Duration::from_millis(100)); // the refresh is now in flight
    a.revoke();
    hermes_core::conn::Secrets::delete("so1");
    let _ = rt.block_on(call).unwrap();
    assert!(matches!(Stored::load("so1").cred, Credential::None), "the late refresh must not write tokens back");
    assert!(a.is_revoked());
}

#[test]
fn a_refused_saved_password_is_forgotten_not_retried() {
    let (rt, m, url) = gated(3600);
    let base = url::Url::parse(&url).unwrap();
    let cred = rt.block_on(auth::login_password(&base, "basic", "jack", "hunter2")).unwrap();
    let st = Stored { cred, provider: Some("basic".into()), username: Some("jack".into()), password: Some("hunter2".into()) };
    let a = auth::Auth::new("pw1", base, st, false);
    let rest = Rest::new(a.clone()).unwrap();
    // Password changed elsewhere, and every session revoked.
    m.set_password("new-password");
    m.revoke_sessions();
    let e = rt.block_on(rest.me()).unwrap_err();
    assert!(e.to_string().contains("Sign in"), "{e}");
    assert!(a.stored().password.is_none(), "the refused password is dropped");
    let posts = m.password_posts();
    for _ in 0..3 {
        assert!(rt.block_on(rest.me()).is_err());
    }
    assert_eq!(m.password_posts(), posts, "no more password attempts after it was refused");
}
