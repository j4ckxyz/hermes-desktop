# Protocol notes

Wire shapes used by Hermes Desktop. All of them are official Hermes surfaces, taken from the
Hermes docs and the `NousResearch/hermes-agent` source (`tui_gateway/`, `hermes_cli/web_routers/`).
The full reasoning is in [PLAN.md](../PLAN.md).

## Transport
* **Gateway JSON-RPC 2.0** at `ws(s)://<dashboard>/api/ws?token=<session token>`. This is the
  same channel the official Desktop uses for Bot Mode.
  * Events arrive as `{"method":"event","params":{"type","session_id","payload"}}`.
  * On `gateway.ready`, the client sends `client.capabilities {server_requests:true}`. If
    `payload.heartbeat` is set, it sends `gateway.ping` every 15 s and drops the socket after 45 s
    of silence.
  * Server→client requests (`approval`, `clarify`, `sudo`, `secret`) have string ids. Any the
    client doesn't handle get `-32601`.
  * Each frame selects a profile through `params.profile`. There is no `/p/<profile>/api/ws`.
* **Dashboard REST** uses the header `X-Hermes-Session-Token`:
  * `/api/status`
  * `/api/sessions/{id}/messages?profile=&limit=&order=latest&include_compacted=true`
  * `POST /api/chat/image-upload`
  * `/api/files/download?path=`
  * `/api/media?path=`
* **API server** `/v1` (and `/p/<profile>/v1` with `gateway.multiplex_profiles`) provides
  `capabilities` and `runs`, with events streamed over SSE.

## Bot Mode
| What | Call |
|---|---|
| Roster | `profiles.list {}` (polled every 5 s; the UI updates only when something changed) |
| Metadata | `profiles.configure {name, ui_meta:{"hermes-bots":{…}}, ui_meta_expected_revisions}`, a compare-and-swap that retries up to 3 times |
| New bot | `profiles.create {name, description, model?}` |
| Canonical chat | `session.list {profile, title:"Bot Chat", include_hidden:true}`, then if needed `session.create {profile, title:"Bot Chat", hidden:true, follow_profile_config:true, source:"gui"}`, then `session.resume {…, defer_history:true, omit_messages:true}` |
| Send | `prompt.submit {session_id, text, profile, submission_id\|input_id, attachments}` (`input_id` when `gateway.ready.session_authority` is set) |
| `/new`, `/reset` | Become `/compact` through `session.compress` (fallback: `session.mutate {operation:"compress"}`), so the bot keeps a single chat |
| @mentions | The official note `[@mentions resolved from the Bot Mode roster — …]` is appended, and the agent uses `message_agent` |
| Agent notices | Rows with `role:user` that start `Message from 🤖 Name (@handle[@conn]):` are shown as cards |
| Unread | Computed client-side as `max(canonical.last_active, last_session.last_active)` compared with a SQLite watermark |

## Files
* Images up to 25 MB are sent with `POST /api/chat/image-upload {data_url, filename}`, which
  returns `{path, mime_type}`, and are then passed in `attachments`.
* Other files up to 256 MB use `file.attach {session_id, name, data_url}`, which returns
  `ref_text`. That reference is prepended to the prompt.
* Agent output arrives as `MEDIA:/abs/path` tags. These are previewed (images inline) and can be
  opened or saved. Downloads stream to a `.part` file, which is renamed when complete.

## Gaps (no official API, so nothing was invented)
* **Cross-machine bot DMs via the Desktop relay** (`bot_relay.*`). The official Desktop acts as a
  courier here. This is not implemented; `hermes peer` covers gateway-to-gateway messaging.
* **API-server-only connections** cannot list profiles or read Bot metadata. They also cannot
  bind the canonical Bot Chat, because a session another surface owns returns 409
  `permission_denied`.
* **Voice/mic.** `voice.*` may not be served by a remote gateway, so the button only shows a
  notice for now.
* **Avatars** (`profiles.get_asset`) are available but not displayed yet. Bots get coloured
  initials instead.
* **Routines and groups** have official calls (`cron.*`, `groups.*`) but no UI yet.
