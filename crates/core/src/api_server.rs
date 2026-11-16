//! The OpenAI-compatible API server (`/v1`, Bearer `API_SERVER_KEY`).
//! Docs: *API Server*, *Programmatic Integration → OpenAI-Compatible API Server*,
//! *Running Many Gateways at Once* (multiplexed `/p/<profile>/v1`).
//!
//! Used for capability detection and as a plain-chat fallback via the runs API:
//! `POST /v1/runs` → `{run_id}`; `GET /v1/runs/{id}/events` (SSE); `POST /v1/runs/{id}/stop`.

use anyhow::{anyhow, bail, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};

#[derive(Clone)]
pub struct ApiServer {
    /// e.g. `http://host:8642` or `http://host:8642/p/researcher` (multiplexed profile).
    root: String,
    key: String,
    http: reqwest::Client,
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq)]
pub struct Sse {
    pub event: String,
    pub data: Value,
}

/// Incremental SSE parser: feed bytes, get complete events.
#[derive(Default)]
pub struct SseParser {
    buf: String,
}

impl SseParser {
    pub fn feed(&mut self, chunk: &str) -> Vec<Sse> {
        self.buf.push_str(&chunk.replace("\r\n", "\n"));
        let mut out = Vec::new();
        while let Some(i) = self.buf.find("\n\n") {
            let block: String = self.buf.drain(..i + 2).collect();
            let mut event = String::from("message");
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event:") {
                    event = v.trim().to_owned();
                } else if let Some(v) = line.strip_prefix("data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(v.strip_prefix(' ').unwrap_or(v));
                }
            }
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let data = serde_json::from_str(&data).unwrap_or(Value::String(data));
            // Runs events carry their type in the payload when no `event:` line is sent.
            if event == "message" {
                if let Some(t) = data.get("event").or_else(|| data.get("type")).and_then(Value::as_str) {
                    event = t.to_owned();
                }
            }
            out.push(Sse { event, data });
        }
        out
    }
}

impl ApiServer {
    pub fn new(base: &str, profile: Option<&str>, key: &str) -> Result<ApiServer> {
        let mut root = base.trim().trim_end_matches('/').to_owned();
        if let Some(stripped) = root.strip_suffix("/v1") {
            root = stripped.to_owned();
        }
        if let Some(p) = profile.filter(|p| !p.is_empty() && *p != "default") {
            root = format!("{root}/p/{p}");
        }
        Ok(ApiServer { root, key: key.to_owned(), http: reqwest::Client::builder().build()? })
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{}", self.root, path)).bearer_auth(&self.key)
    }

    /// `GET /v1/capabilities`: machine-readable feature flags.
    pub async fn capabilities(&self) -> Result<Value> {
        let r = self.get("/v1/capabilities").send().await?;
        if !r.status().is_success() {
            bail!("capabilities: {}", r.status());
        }
        Ok(r.json().await?)
    }

    pub async fn health(&self) -> Result<bool> {
        Ok(self.http.get(format!("{}/health", self.root)).send().await?.status().is_success())
    }

    /// Start a run; `session_id` continues a conversation (`X-Hermes-Session-Id`).
    pub async fn start_run(&self, input: &str, session_id: Option<&str>) -> Result<String> {
        // Docs: runs take `input` and an optional `session_id` the API server owns; a session
        // another surface owns (Desktop, TUI, CLI) is refused with 409 `permission_denied`.
        let b = self.http.post(format!("{}/v1/runs", self.root)).bearer_auth(&self.key).json(&json!({"input": input, "session_id": session_id}));
        let r = b.send().await?;
        if !r.status().is_success() {
            bail!("runs: {}", r.status());
        }
        let v: Value = r.json().await?;
        v.get("run_id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| anyhow!("no run_id"))
    }

    /// Stream a run's lifecycle events until a terminal `run.*` event.
    pub async fn run_events(&self, run_id: &str, mut on: impl FnMut(Sse)) -> Result<()> {
        let r = self.get(&format!("/v1/runs/{run_id}/events")).send().await?;
        if !r.status().is_success() {
            bail!("run events: {}", r.status());
        }
        let mut p = SseParser::default();
        let mut s = r.bytes_stream();
        while let Some(chunk) = s.next().await {
            for ev in p.feed(&String::from_utf8_lossy(&chunk?)) {
                let done = matches!(ev.event.as_str(), "run.completed" | "run.failed" | "run.cancelled" | "run.interrupted");
                on(ev);
                if done {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    pub async fn stop_run(&self, run_id: &str) -> Result<()> {
        self.http.post(format!("{}/v1/runs/{run_id}/stop", self.root)).bearer_auth(&self.key).send().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parses_split_chunks() {
        let mut p = SseParser::default();
        assert!(p.feed("event: message.delta\ndata: {\"delta\":\"he").is_empty());
        let e = p.feed("llo\"}\n\ndata: {\"event\":\"run.completed\",\"output\":\"x\"}\n\n");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].event, "message.delta");
        assert_eq!(e[0].data["delta"], "hello");
        assert_eq!(e[1].event, "run.completed");
    }

    #[test]
    fn multiplex_root() {
        let a = ApiServer::new("http://h:8642/v1/", Some("researcher"), "k").unwrap();
        assert_eq!(a.root, "http://h:8642/p/researcher");
        let b = ApiServer::new("http://h:8643", None, "k").unwrap();
        assert_eq!(b.root, "http://h:8643");
    }
}
