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
