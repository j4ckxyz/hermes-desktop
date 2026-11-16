//! `hermes-mock [--port 9119] [--token TOKEN] [--pace MS]`: run the stand-in Hermes backend.
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let port = get("--port").unwrap_or_else(|| "9119".into());
    let token = get("--token").unwrap_or_else(|| "dev-token".into());
    let m = hermes_mock::Mock::new(&token);
    if let Some(p) = get("--pace").and_then(|p| p.parse().ok()) {
        m.set_pace(p);
    }
    let addr = m.spawn(&format!("127.0.0.1:{port}")).await;
    println!("hermes-mock listening on http://{addr} (session token: {token})");
    tokio::signal::ctrl_c().await.ok();
}
