//! Local cache (SQLite) for instant start-up. The backend stays the source of truth: everything
//! here is overwritten by what the server says once connected.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use crate::roster::{Bot, BotKey};
use crate::transcript::ChatMessage;

pub struct Store {
    db: Mutex<Connection>,
}

/// Keep at most this many messages per chat in the cache.
const CACHE_MESSAGES: usize = 200;

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> Result<Store> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Store> {
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS roster(conn TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY(conn));
             CREATE TABLE IF NOT EXISTS chat(conn TEXT NOT NULL, profile TEXT NOT NULL, session TEXT, json TEXT NOT NULL, PRIMARY KEY(conn, profile));
             CREATE TABLE IF NOT EXISTS seen(conn TEXT NOT NULL, profile TEXT NOT NULL, ts REAL NOT NULL, PRIMARY KEY(conn, profile));
             CREATE TABLE IF NOT EXISTS kv(k TEXT PRIMARY KEY, v TEXT NOT NULL);",
        )?;
        Ok(Store { db: Mutex::new(db) })
    }

    pub fn save_roster(&self, conn: &str, bots: &[Bot]) -> Result<()> {
        let json = serde_json::to_string(bots)?;
        self.db.lock().unwrap().execute("INSERT OR REPLACE INTO roster(conn, json) VALUES(?1, ?2)", params![conn, json])?;
        Ok(())
    }

    pub fn load_rosters(&self) -> Vec<Bot> {
        let db = self.db.lock().unwrap();
        let Ok(mut st) = db.prepare("SELECT json FROM roster") else { return vec![] };
        let rows = st.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect::<Vec<_>>()).unwrap_or_default();
        rows.iter().filter_map(|j| serde_json::from_str::<Vec<Bot>>(j).ok()).flatten().collect()
    }

    pub fn save_chat(&self, key: &BotKey, session: Option<&str>, msgs: &[ChatMessage]) -> Result<()> {
        let tail = &msgs[msgs.len().saturating_sub(CACHE_MESSAGES)..];
        let tail: Vec<ChatMessage> = tail.iter().filter(|m| !m.streaming).cloned().collect();
        let json = serde_json::to_string(&tail)?;
        self.db.lock().unwrap().execute(
            "INSERT OR REPLACE INTO chat(conn, profile, session, json) VALUES(?1, ?2, ?3, ?4)",
            params![key.conn, key.profile, session, json],
        )?;
        Ok(())
    }

    pub fn load_chat(&self, key: &BotKey) -> Option<(Option<String>, Vec<ChatMessage>)> {
        let db = self.db.lock().unwrap();
        db.query_row("SELECT session, json FROM chat WHERE conn=?1 AND profile=?2", params![key.conn, key.profile], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
        })
        .optional()
        .ok()
        .flatten()
        .and_then(|(s, j)| Some((s, serde_json::from_str(&j).ok()?)))
    }

    /// Unread watermark: the last activity time the user has seen for a bot.
    pub fn seen(&self, key: &BotKey) -> Option<f64> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT ts FROM seen WHERE conn=?1 AND profile=?2", params![key.conn, key.profile], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_seen(&self, key: &BotKey, ts: f64) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO seen(conn, profile, ts) VALUES(?1, ?2, ?3) ON CONFLICT(conn, profile) DO UPDATE SET ts=max(ts, excluded.ts)",
            params![key.conn, key.profile, ts],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, k: &str) -> Option<String> {
        self.db.lock().unwrap().query_row("SELECT v FROM kv WHERE k=?1", params![k], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_kv(&self, k: &str, v: &str) -> Result<()> {
        self.db.lock().unwrap().execute("INSERT OR REPLACE INTO kv(k, v) VALUES(?1, ?2)", params![k, v])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::Role;

    #[test]
    fn roundtrip() {
        let s = Store::memory().unwrap();
        let row = serde_json::json!({"name":"a","ui_meta":{"hermes-bots":{"title":"A"}}});
        let b = Bot::from_row("c", "Mac", &row).unwrap();
        s.save_roster("c", &[b.clone()]).unwrap();
        assert_eq!(s.load_rosters(), vec![b.clone()]);
        let m = vec![ChatMessage::new("1", Role::User, "hi", Some(1.0))];
        s.save_chat(&b.key, Some("sess"), &m).unwrap();
        let (sess, got) = s.load_chat(&b.key).unwrap();
        assert_eq!(sess.as_deref(), Some("sess"));
        assert_eq!(got, m);
        assert_eq!(s.seen(&b.key), None);
        s.set_seen(&b.key, 5.0).unwrap();
        s.set_seen(&b.key, 3.0).unwrap();
        assert_eq!(s.seen(&b.key), Some(5.0));
    }
}
