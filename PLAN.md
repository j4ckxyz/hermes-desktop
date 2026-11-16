# Hermes Desktop (GPUI): plan

A fast, native, cross-platform (macOS, Windows, Linux X11/Wayland) client for a
self-hosted [Hermes Agent](https://hermes-agent.nousresearch.com), written in Rust on
[GPUI](https://gpui.rs). It has no webview and no Electron. It speaks only official Hermes
protocols and runs no backend or protocol of its own.

All protocol facts below come from the official docs (the `llms-full.txt` snapshot fetched on
2026-11-16) and the official source, `NousResearch/hermes-agent@c664ffd`. Citations give the
docs page, plus the source file where the docs leave the wire format implicit.

## 1. Protocol choice

Hermes ships three integration protocols (docs: *Developer guide → Programmatic Integration*):

| Protocol | Transport | Bot Mode? |
|---|---|---|
| ACP | JSON-RPC over stdio | no (IDE-oriented, local only) |
| **TUI gateway** | JSON-RPC over WebSocket `/api/ws` (`hermes-gateway-v1`) | **yes**: `profiles.*`, `session.*`, `prompt.*`, `groups.*`, `bot_relay.*` |
| API server | OpenAI-compatible HTTP + SSE at `/v1` | no roster or profile metadata |

The docs recommend the TUI gateway for "a custom desktop … host [that wants] every Hermes
feature", and it is what the official Hermes Desktop uses for Bot Mode. *Bot Mode → Local Bots
and the gateway*: "dials it over WebSocket (`/api/ws`)". *Programmatic Integration*: "the
method and event catalog behind … the Desktop app".

**Primary transport: a "Remote gateway" connection**, the same kind Hermes Desktop has
(docs: *Connecting Desktop to Many Hermes Instances*):

* Base URL of a running `hermes serve` / `hermes dashboard` backend (e.g.
  `http://homelab.tailnet.ts.net:9119`). Reverse-proxy path prefixes work.
* Credential: the **dashboard session token**, i.e. the deployment token
  `HERMES_DASHBOARD_SESSION_TOKEN` (docs: *Web Dashboard → Deployment session-token
  provider*). It is sent as `X-Hermes-Session-Token` on REST, and as `?token=` on the `/api/ws`
  upgrade.
* WebSocket frames are plain JSON-RPC 2.0. Events arrive as
  `{"method":"event","params":{"type","session_id","payload",…}}`. Server→client requests
  (`approval`, `clarify`, `sudo`, `secret`) carry a string id. After `gateway.ready` the client
  sends `client.capabilities {"server_requests":true}`, and `gateway.ping` every 15 s when
  `gateway.ready.payload.heartbeat` is set (docs: *Programmatic Integration → Server→client
  requests*).
* The profile is selected **per frame** with `params.profile` on one host-scoped socket, and
  with `?profile=` on REST (docs: *Gateway over SSH → Profiles*; source:
  `gateway/session_controls.py`).

**Secondary transport: the API server** (`/v1`, Bearer `API_SERVER_KEY`). It is used for
`GET /v1/capabilities`, `/health`, and an optional plain-chat fallback through
`POST /v1/runs` + `GET /v1/runs/{id}/events` (SSE) when only an API-server key is configured.
Both `gateway.multiplex_profiles` (`/p/<profile>/v1/...` on the default profile's API server)
and per-port profiles (a separate base URL per profile) are supported. They are modelled as
*API-server connections*, each mapping to one or more bots. The API server has no roster
metadata, so such bots get a default look and no `message_agent` features (docs:
*Multiple gateways*, *API Server*).

Feature detection: `gateway.ready.payload` (`session_authority`, `heartbeat`), the
`client.capabilities` reply, and `-32601` meaning "not available here" (docs: *Which methods
the shared gateway answers*). For the API server, `GET /v1/capabilities`.

## 2. Bot / roster model (docs: *Bot Mode*; source: `tui_gateway/methods_profiles.py`)

* **Bot = Hermes profile.** The roster comes from `profiles.list {}` and is polled every 5 s,
  as the official Bots plugin does. Each row carries `name`, `display_name`, `description`,
  `model`, `last_session`, `canonical_session` (with preview and last_active),
  `ui_meta["hermes-bots"]`, `ui_meta_revisions`, and `has_avatar`.
* **Look and metadata.** `ui_meta["hermes-bots"]` holds `title`, `color`, `shape`, `hidden`,
  `sectionId`/`sectionName`, `groups[]`, `pinned` and `description`. Writes go through
  `profiles.configure {name, ui_meta, ui_meta_expected_revisions}`, a compare-and-swap loop with
  at most 3 tries, merged key-wise, so edits round-trip with the official Desktop. Avatars use
  `profiles.get_asset` / `profiles.set_asset` (PNG/JPEG/WebP, 2 MB or less).
* **Canonical Bot Chat.** Lookup is `session.list {profile, title:"Bot Chat",
  include_hidden:true}`, matching on `root_title || title == "Bot Chat"`. When it is missing,
  the app calls `session.create {profile, title:"Bot Chat", hidden:true,
  follow_profile_config:true}` and then sets the title. The chat opens with `session.resume`,
  and history comes from `GET /api/sessions/{id}/messages?profile=…&order=latest`.
* **/new and /reset** inside a Bot Chat are rewritten to `/compact`, which becomes
  `session.compress` (`session.mutate` operation `compress` on canonical sockets).
* **Unread state and preview** are computed client-side, as the official plugin does: the newer
  `last_active` of `canonical_session`/`last_session` is compared with a per-bot watermark that
  is persisted in the local cache.
* **Creating bots.** `profiles.create {name, description, clone_from, soul, model, provider}`
  is a JSON-RPC twin of `POST /api/profiles`, so creating a bot **remotely is officially
  supported**. No SSH fallback is needed.
* **@mentions** resolve against the live roster: profile name, title slug (`Research Buddy` →
  `@research-buddy`/`@researchbuddy`), or `@name@connection` across connections. Like the
  official composer middleware, the app appends the "[@mentions resolved from the Bot Mode
  roster …]" note to the prompt, so the bot sends its own `message_agent` call. The app never
  forwards a message to another bot itself.
* **Agent notices.** Replies between bots arrive as ordinary `role:"user"` rows prefixed
  `Message from 🤖 <Name> (@<handle>[@conn]): `. They are detected by regex at render time and
  drawn as distinct notice cards, never as the user's own bubble.
* **Bots on other machines.** Each configured connection has its own roster. Handles collide
  across machines, so they are disambiguated as `@name-device`, and chats route to the owning
  connection.

## 3. Files (source: `tui_gateway/prompt_attachments.py`, `hermes_cli/web_routers/files.py`)

* Images: `POST /api/chat/image-upload?profile=` (`{data_url, filename}` → `{path}`), then
  `prompt.submit.attachments:[{path,mime}]`. The fallback is `image.attach_bytes`. Limit 25 MB.
* Other files: `file.attach {session_id, name, data_url}` → `ref_text` (`@file:…`), which is
  inserted into the prompt. The server limit is 256 MB per attachment, sent as a single frame.
  Encoding and upload run on a background thread, so the UI never blocks. The app refuses files
  over the server limit up front.
* Agent files (`MEDIA:/path` tags in replies): `GET /api/files/download?path=` and
  `GET /api/media?path=` (inline images, 25 MB or less). Open and Save act on a streamed
  download into the cache directory.

## 4. Architecture and crate layout

```
crates/
  core/   hermes-core: no UI. Protocol client (tokio + tungstenite + reqwest/rustls),
          wire types, roster/mention/notice logic, SQLite cache, keychain access.
  mock/   hermes-mock: an axum server faithful to the documented wire (REST + /api/ws +
          /v1). Used by tests and for running the app without a real Hermes.
  app/    hermes-desktop: the GPUI binary. Views: sidebar roster, chat, composer,
          settings. Uses gpui-component for inputs, markdown (tree-sitter highlighting),
          avatars and scrolling.
```

* Threads: GPUI's main thread renders, and one small tokio runtime (2 workers) does all I/O.
  Core sends `CoreEvent`s over an async channel, and the app drains them in a GPUI task.
  Nothing blocking runs on the UI thread.
* Cache: SQLite (rusqlite, bundled) in the platform data dir, holding the roster, the last
  messages per Bot Chat, and unread watermarks. At startup the app paints from the cache and
  then reconciles with the server, which is always the source of truth.
* Secrets: tokens and keys live in the OS keychain through `keyring` (macOS Keychain, Windows
  Credential Manager, Linux Secret Service). The config file holds only base URLs and names.
* Theme: dark by default, following the OS appearance (`window.appearance()`). The accent
  colour is configurable.

## 5. Milestones

Status (v0.1.0): M1 and M2 are done. M3 is done except for download progress UI. From M4, create bot, hide/unhide and approvals are done; clarify prompts, sections UI and avatar upload are still open. M5 has not started.

1. **M1, a running app.** Connection settings with the keychain. The roster sidebar (avatar,
   name, preview, unread). A canonical Bot Chat with live streaming, markdown and code
   highlighting. Composer with + attach, drag-and-drop and paste. Tool-activity and subagent
   cards. Mock server plus tests.
2. **M2, Bot Mode social features.** @mention autocomplete and resolution, agent-notice
   rendering, unread and notifications, the SQLite cache for instant start, Share (copy the
   transcript as Markdown).
3. **M3, files done right.** Media tags in replies, Open/Save, image previews, and large files
   handled off-thread with progress.
4. **M4, management.** Create and edit bots (`profiles.create` / `profiles.configure` /
   `set_asset`), hide/unhide, sections, approvals and clarify prompts.
5. **M5, more.** Group chats (`groups.*`), routines (`cron`), multiple connections at once,
   the API-server-only mode, voice (mic) through `voice.*`.

## 6. Known gaps and risks

* **The Desktop relay for cross-machine bot DMs** (`bot_relay.*`) is a courier role played by
  the official Desktop. Implementing it would make this app a relay too. That is out of scope
  until the core is solid. Bots can use `hermes peer` (gateway-to-gateway, no desktop needed)
  instead.
* **API-server sessions cannot bind a session another surface owns** (HTTP 409
  `permission_denied`, docs *API Server → API sessions are their own sessions*). So the canonical
  Bot Chat is only reachable over the gateway WebSocket, and API-server mode keeps its own
  conversations.
* **The API server alone cannot list profiles or read Bot metadata**, so an API-server-only
  connection shows plain bots.
* The **mic** needs a speech-to-text path: either the gateway's `voice.*` methods (served by an
  older handler, so they may answer `-32601`) or a local recorder that sends audio as a file.
  The button is there, and it is feature-detected.
* Testing covers macOS only. Linux and Windows build through `cfg`-gated code and are listed as
  untested in the report.
