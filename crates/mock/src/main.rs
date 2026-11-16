//! `hermes-mock [--port 9119] [--token TOKEN] [--pace MS] [--basic-auth USER:PASS] [--access-ttl SECS]`:
//! run the stand-in Hermes backend. `--basic-auth` gates it like a dashboard on a public bind.
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let port = get("--port").unwrap_or_else(|| "9119".into());
    let token = get("--token").unwrap_or_else(|| "dev-token".into());
    let mut m = hermes_mock::Mock::new(&token);
    if let Some((u, p)) = get("--basic-auth").as_deref().and_then(|a| a.split_once(':')) {
        m = m.with_basic_auth(u, p);
        println!("sign-in required: username {u}");
    }
    if let Some(t) = get("--access-ttl").and_then(|t| t.parse().ok()) {
        m.set_access_ttl(t);
    }
    if let Some(p) = get("--pace").and_then(|p| p.parse().ok()) {
        m.set_pace(p);
    }
    let addr = m.spawn(&format!("127.0.0.1:{port}")).await;
    println!("hermes-mock listening on http://{addr} (session token: {token})");
    tokio::signal::ctrl_c().await.ok();
}
