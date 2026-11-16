//! JSON-RPC 2.0 over the gateway WebSocket `/api/ws` (`hermes-gateway-v1`).
//!
//! Docs: *Programmatic Integration → TUI Gateway JSON-RPC*, *Server→client requests*.
//! Frames: requests/responses by id; `{"method":"event","params":{type,session_id,payload}}`
//! notifications; server→client requests that carry a string id and any other method.
//!
//! A single task owns the socket. It reconnects with backoff, sends `gateway.ping` when the
//! server advertises a JSON heartbeat, and fails every in-flight call when the socket drops so
//! nothing hangs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, Message};

pub const METHOD_NOT_FOUND: i64 = -32601;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
const HEARTBEAT_DEADLINE: Duration = Duration::from_secs(45);
const BACKOFF_MS: [u64; 6] = [400, 1_000, 2_000, 4_000, 8_000, 15_000];

#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Value,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}
impl std::error::Error for RpcError {}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    Connecting,
    Connected,
    Reconnecting { attempt: u32, reason: String },
    Unauthorized,
    Stopped,
}

/// Things the gateway pushes to us.
#[derive(Debug, Clone)]
pub enum Inbound {
    Link(LinkState),
    /// `gateway.ready` payload (feature flags: `session_authority`, `heartbeat`, …).
    Ready(Value),
    Event { kind: String, session_id: String, payload: Value },
    /// A server→client request (approval, clarify, sudo, secret). Answer with
    /// [`Gateway::respond`] or [`Gateway::respond_error`].
    Request { id: String, method: String, params: Value },
}

type Pending = oneshot::Sender<std::result::Result<Value, RpcError>>;

struct Shared {
    ws_url: String,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
    out: Mutex<Option<mpsc::UnboundedSender<String>>>,
    link: watch::Sender<LinkState>,
    stop: watch::Sender<bool>,
    inbound: async_channel::Sender<Inbound>,
}

#[derive(Clone)]
pub struct Gateway {
    s: Arc<Shared>,
}

impl Gateway {
    /// `ws_url` already carries the credential (`?token=`), see [`crate::conn::Endpoint`].
    pub fn spawn(ws_url: String, inbound: async_channel::Sender<Inbound>) -> Gateway {
        let s = Arc::new(Shared {
            ws_url,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            out: Mutex::new(None),
            link: watch::channel(LinkState::Connecting).0,
            stop: watch::channel(false).0,
            inbound,
        });
        tokio::spawn(run(s.clone()));
        Gateway { s }
    }

    pub fn stop(&self) {
        self.s.stop.send_replace(true);
    }

    pub fn link(&self) -> LinkState {
        self.s.link.borrow().clone()
    }

    /// Wait until connected (or fail on bad credentials / timeout).
    pub async fn connected(&self, within: Duration) -> Result<()> {
        let mut rx = self.s.link.subscribe();
        let fut = async {
            loop {
                match &*rx.borrow_and_update() {
                    LinkState::Connected => return Ok(()),
                    LinkState::Unauthorized => return Err(anyhow!("Hermes refused the session token.")),
                    LinkState::Stopped => return Err(anyhow!("Connection stopped.")),
                    _ => {}
                }
                rx.changed().await.map_err(|_| anyhow!("Connection closed."))?;
            }
        };
        tokio::time::timeout(within, fut).await.map_err(|_| anyhow!("Couldn't reach Hermes yet. Still trying…"))?
    }

    pub async fn call(&self, method: &str, params: Value) -> std::result::Result<Value, RpcError> {
        self.call_timeout(method, params, Duration::from_secs(60)).await
    }

    pub async fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> std::result::Result<Value, RpcError> {
        let local = |m: &str| RpcError { code: -1, message: m.to_owned(), data: Value::Null };
        self.connected(Duration::from_secs(20)).await.map_err(|e| local(&e.to_string()))?;
        let id = self.s.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.s.pending.lock().unwrap().insert(id, tx);
        let frame = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        if !send(&self.s, frame) {
            self.s.pending.lock().unwrap().remove(&id);
            return Err(local("Not connected to Hermes."));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(local("The connection to Hermes dropped.")),
            Err(_) => {
                self.s.pending.lock().unwrap().remove(&id);
                Err(local(&format!("Hermes didn't answer {method} in time.")))
            }
        }
    }

    pub fn respond(&self, id: &str, result: Value) -> bool {
        send(&self.s, json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }

    pub fn respond_error(&self, id: &str, code: i64, message: &str) -> bool {
        send(&self.s, json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}}).to_string())
    }
}

fn send(s: &Shared, text: String) -> bool {
    s.out.lock().unwrap().as_ref().is_some_and(|tx| tx.send(text).is_ok())
}

fn set_link(s: &Shared, st: LinkState) {
    s.link.send_replace(st.clone());
    let _ = s.inbound.try_send(Inbound::Link(st));
}

fn fail_pending(s: &Shared) {
    let drained: Vec<Pending> = s.pending.lock().unwrap().drain().map(|(_, v)| v).collect();
    for tx in drained {
        let _ = tx.send(Err(RpcError { code: -1, message: "The connection to Hermes dropped.".into(), data: Value::Null }));
    }
}

async fn run(s: Arc<Shared>) {
    let mut attempt = 0u32;
    let mut stop = s.stop.subscribe();
    set_link(&s, LinkState::Connecting);
    loop {
        if is_stopped(&stop) {
            break;
        }
        let reason = match connect(&s.ws_url).await {
            Ok(ws) => {
                attempt = 0;
                let r = pump(&s, ws).await;
                *s.out.lock().unwrap() = None;
                fail_pending(&s);
                match r {
                    None => break,
                    Some(reason) => reason,
                }
            }
            Err(ConnectErr::Unauthorized) => {
                set_link(&s, LinkState::Unauthorized);
                // Keep retrying slowly: the user may fix the token on the server side.
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => continue,
                    _ = stopped(&mut stop) => break,
                }
            }
            Err(ConnectErr::Other(e)) => e,
        };
        attempt += 1;
        set_link(&s, LinkState::Reconnecting { attempt, reason });
        let delay = BACKOFF_MS[(attempt as usize - 1).min(BACKOFF_MS.len() - 1)];
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(delay)) => {}
            _ = stopped(&mut stop) => break,
        }
    }
    set_link(&s, LinkState::Stopped);
}

enum ConnectErr {
    Unauthorized,
    Other(String),
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str) -> std::result::Result<Ws, ConnectErr> {
    let req = url.into_client_request().map_err(|e| ConnectErr::Other(e.to_string()))?;
    let cfg = tungstenite::protocol::WebSocketConfig::default()
        // Large attachments travel as single frames (server cap 384 MB).
        .max_message_size(Some(400 << 20))
        .max_frame_size(Some(400 << 20));
    match tokio::time::timeout(Duration::from_secs(15), tokio_tungstenite::connect_async_with_config(req, Some(cfg), false)).await {
        Err(_) => Err(ConnectErr::Other("Timed out connecting to Hermes.".into())),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tungstenite::Error::Http(r))) if matches!(r.status().as_u16(), 401 | 403) => Err(ConnectErr::Unauthorized),
        Ok(Err(e)) => Err(ConnectErr::Other(friendly(&e))),
    }
}

fn friendly(e: &tungstenite::Error) -> String {
    match e {
        tungstenite::Error::Io(_) | tungstenite::Error::Tls(_) => "Couldn't reach Hermes. Check the address, network or Tailscale.".into(),
        tungstenite::Error::Http(r) => format!("Hermes answered {} to the chat connection.", r.status()),
        other => format!("Connection error: {other}"),
    }
}

/// Returns `None` when stopped deliberately, `Some(reason)` when the link dropped.
async fn pump(s: &Arc<Shared>, ws: Ws) -> Option<String> {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    *s.out.lock().unwrap() = Some(tx);
    set_link(s, LinkState::Connected);
    let mut stop = s.stop.subscribe();
    let mut beat = tokio::time::interval(HEARTBEAT_EVERY);
    beat.tick().await;
    let mut json_heartbeat = false;
    let mut last_in = Instant::now();
    let mut seq = 0u64;
    loop {
        tokio::select! {
            frame = stream.next() => {
                let text = match frame {
                    Some(Ok(Message::Text(t))) => t.to_string(),
                    Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                    Some(Ok(Message::Close(_))) | None => return Some("Connection closed.".into()),
                    Some(Ok(_)) => { last_in = Instant::now(); continue; }
                    Some(Err(e)) => return Some(friendly(&e)),
                };
                last_in = Instant::now();
                if let Some(hb) = route(s, &text) {
                    json_heartbeat = hb;
                }
            }
            Some(text) = rx.recv() => {
                if log::log_enabled!(target: "hermes_net", log::Level::Debug) {
                    // Network debug logs names only, never payloads (they can hold prompts or secrets).
                    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                    log::debug!(target: "hermes_net", "→ {} id={}", v.get("method").and_then(Value::as_str).unwrap_or("response"), v.get("id").map(|i| i.to_string()).unwrap_or_default());
                }
                if let Err(e) = sink.send(Message::Text(text.into())).await {
                    return Some(friendly(&e));
                }
            }
            _ = beat.tick() => {
                if last_in.elapsed() > HEARTBEAT_DEADLINE {
                    return Some("Connection went quiet.".into());
                }
                seq += 1;
                let ping = if json_heartbeat {
                    Message::Text(json!({"jsonrpc":"2.0","id":format!("hb-{seq}"),"method":"gateway.ping","params":{}}).to_string().into())
                } else {
                    Message::Ping(Default::default())
                };
                if let Err(e) = sink.send(ping).await {
                    return Some(friendly(&e));
                }
            }
            _ = stopped(&mut stop) => {
                let _ = sink.close().await;
                return None;
            }
        }
    }
}

/// Route one frame. Returns `Some(heartbeat)` for `gateway.ready`.
fn route(s: &Arc<Shared>, text: &str) -> Option<bool> {
    let frame: Value = serde_json::from_str(text).ok()?;
    let method = frame.get("method").and_then(Value::as_str);
    if log::log_enabled!(target: "hermes_net", log::Level::Debug) {
        let kind = frame.pointer("/params/type").and_then(Value::as_str).or(method).unwrap_or("response");
        log::debug!(target: "hermes_net", "← {kind} id={} {}B", frame.get("id").map(|i| i.to_string()).unwrap_or_default(), text.len());
    }
    match (method, frame.get("id")) {
        (Some("event"), _) => {
            let p = frame.get("params").cloned().unwrap_or(Value::Null);
            let kind = p.get("type").and_then(Value::as_str).unwrap_or_default().to_owned();
            let sid = p.get("session_id").and_then(Value::as_str).unwrap_or_default().to_owned();
            let payload = p.get("payload").cloned().unwrap_or(Value::Null);
            if kind == "gateway.ready" {
                // Advertise that we answer server→client requests (required since the
                // breaking change in *Programmatic Integration*).
                let id = s.next_id.fetch_add(1, Ordering::Relaxed);
                send(s, json!({"jsonrpc":"2.0","id":id,"method":"client.capabilities","params":{"server_requests":true}}).to_string());
                let hb = payload.get("heartbeat").and_then(Value::as_bool).unwrap_or(false);
                let _ = s.inbound.try_send(Inbound::Ready(payload));
                return Some(hb);
            }
            let _ = s.inbound.try_send(Inbound::Event { kind, session_id: sid, payload });
        }
        (Some(m), Some(Value::String(id))) => {
            let _ = s.inbound.try_send(Inbound::Request {
                id: id.clone(),
                method: m.to_owned(),
                params: frame.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        (None, Some(id)) => {
            let key = match id {
                Value::Number(n) => n.as_u64(),
                Value::String(t) => t.parse().ok(),
                _ => None,
            }?;
            if let Some(tx) = s.pending.lock().unwrap().remove(&key) {
                let r = match frame.get("error").filter(|e| !e.is_null()) {
                    Some(e) => Err(RpcError {
                        code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                        message: e.get("message").and_then(Value::as_str).unwrap_or("Hermes request failed.").to_owned(),
                        data: e.get("data").cloned().unwrap_or(Value::Null),
                    }),
                    None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(r);
            }
        }
        _ => {}
    }
    None
}

/// Resolves once a stop is requested (the guard is dropped inside, keeping callers `Send`).
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

fn is_stopped(rx: &watch::Receiver<bool>) -> bool {
    *rx.borrow()
}
