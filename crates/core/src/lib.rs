//! hermes-core: everything in Hermes Desktop that is not pixels.
//!
//! Only official Hermes protocols are used: the gateway JSON-RPC WebSocket (`/api/ws`), the
//! dashboard REST API (`/api/...`) and the OpenAI-compatible API server (`/v1`). See PLAN.md.

pub mod api_server;
pub mod apps;
pub mod auth;
pub mod conn;
pub mod files;
pub mod gateway;
pub mod logos;
pub mod rest;
pub mod roster;
pub mod slash;
pub mod usage;
pub mod service;
pub mod store;
pub mod transcript;

pub use roster::{Bot, BotKey};
pub use service::{Cmd, Ev, LocalFile, Service};
pub use transcript::{ChatMessage, Role};
pub use url::Url;
