# Protocol notes

Wire shapes used by Hermes Desktop. All of them are official Hermes surfaces, taken from the
Hermes docs and the `NousResearch/hermes-agent` source (`tui_gateway/`, `hermes_cli/web_routers/`).
The full reasoning is in [PLAN.md](../PLAN.md).

## Transport
* **Gateway JSON-RPC 2.0** at `ws(s)://<dashboard>/api/ws?ticket=<ws ticket>` (signed in) or
  `?token=<session token>` (loopback / Advanced). This is the same channel the official Desktop
  uses for Bot Mode. See **Sign-in** below for where each comes from.
  * Events arrive as `{"method":"event","params":{"type","session_id","payload"}}`.
  * On `gateway.ready`, the client sends `client.capabilities {server_requests:true}`. If
    `payload.heartbeat` is set, it sends `gateway.ping` every 15 s and drops the socket after 45 s
    of silence.
  * Server→client requests (`approval`, `clarify`, `sudo`, `secret`) have string ids. Any the
    client doesn't handle get `-32601`.
  * Each frame selects a profile through `params.profile`. There is no `/p/<profile>/api/ws`.
* **Dashboard REST** uses `Authorization: Bearer <access token>` when signed in, otherwise the
  header `X-Hermes-Session-Token`:
  * `/api/status`
  * `/api/sessions/{id}/messages?profile=&limit=&order=latest&include_compacted=true`
  * `POST /api/chat/image-upload`
  * `/api/files/download?path=`
  * `/api/media?path=`
* **API server** `/v1` (and `/p/<profile>/v1` with `gateway.multiplex_profiles`) provides
  `capabilities` and `runs`, with events streamed over SSE.

## Sign-in

Researched on 2026-11-16 against the official docs (`llms-full.txt`, fetched that day from
hermes-agent.nousresearch.com) and `NousResearch/hermes-agent` at `dff103e0`. Code: `crates/core/src/auth.rs`.

**When a server asks for a password.** The dashboard's auth gate turns on for any non-loopback
bind or a non-loopback `public_url`; loopback binds have no accounts (docs: *Web Dashboard →
Authentication (gated mode)*, llms-full.txt ~L96931). Providers are `basic` (username + password,
`plugins/dashboard_auth/basic`), `nous` (OAuth), `self-hosted` (OIDC) and `session-token`.

**Discovery.** `GET /api/status` → `version`, `auth_required`, `auth_providers`, `auth_flows`
(`native_pkce` when the native flow below is available; docs *Desktop Native Sign-In (RFC 8252)*,
~L81961–82061). `GET /api/auth/providers` → `{providers:[{name, display_name, supports_password}]}`,
503 when none is registered (`hermes_cli/dashboard_auth/routes.py:214`). The app probes what you
type (and ports 9119/9120 on this machine) with these two calls and nothing else.

**How the web dashboard signs in.** Its `/login` page POSTs `/auth/password-login
{provider, username, password, next}` (`routes.py:466`). Success is `{ok, next}` plus two cookies:
`hermes_session_at` (HttpOnly, SameSite=Lax) and `hermes_session_rt` (refresh). Failures: 401
"Invalid credentials" (deliberately generic), 429 after 10 attempts per minute per IP, 503 when the
provider is down, 404 for an unknown provider. The middleware refreshes the session from the rt
cookie transparently (`dashboard_auth/middleware.py`); `POST /auth/logout` clears and revokes it
(`routes.py:515`); `GET /api/auth/me` returns the identity (`routes.py:551`). The browser reaches
the WebSocket with `POST /api/auth/ws-ticket` → `{ticket, ttl_seconds: 30}`, single use, presented
as `/api/ws?ticket=` or the `hermes-gateway-ticket.` subprotocol (`routes.py:583`,
`web_server_chat.py:377 _consume_ws_ticket`). Lifetimes for `basic`: access 12 h
(`session_ttl_seconds`), refresh 30 days, stateless; changing the username, password or secret
invalidates every session. (Nous OAuth access tokens live 15 min.)

**What a native app does (what we use).** Cookies are for browsers; the official native clients use
the RFC 8252 flow, which yields bearer tokens (docs ~L81961, `routes.py:307/605/626`):

1. `GET /auth/native/authorize?provider&code_challenge(S256)&code_challenge_method=S256&redirect_uri=http://127.0.0.1:<port>/callback&state`
   sets the `hermes_session_pkce` cookie and 302s to `/login` for password providers.
2. `POST /auth/password-login` with that cookie returns `next = <redirect_uri>?code&state` and sets
   no session cookies.
3. `POST /auth/native/token {code, code_verifier}` → `{access_token, refresh_token,
   token_type: "Bearer", expires_at, provider, user_id}`.
4. Every gated route accepts `Authorization: Bearer`. `POST /auth/native/refresh {refresh_token,
   provider}` rotates both tokens; 401 `session_expired` means sign in again.

Hermes Desktop runs these steps itself (the password form is ours, so no browser or loopback
listener is needed; the `redirect_uri` is only echoed back). Hermes' own
`hermes_cli/gateway_connections_oauth.py` drives the same endpoints (`REFRESH_SKEW_S = 60`), and
hermacos (`core/src/auth.rs`) uses the same headless password variant.

**Session handling in the app.**
* Tokens are refreshed 120 s before `expires_at`, once for all callers (single-flight), and the
  rotated pair is written back to the keychain. A REST 401 triggers one refresh and one retry.
* If the refresh token is dead (password changed, 30 days passed) and the user ticked *Stay signed
  in*, the app signs in again silently with the password kept in the keychain. Otherwise the link
  goes to *Sign in required* and Settings → Account offers **Sign in**.
* Every WebSocket (re)connect mints a fresh ticket via `/api/auth/ws-ticket`.
* Plain HTTP to another machine (not loopback, not a Tailscale `*.ts.net` / 100.64.0.0/10 /
  fd7a:115c:a1e0::/48 address) needs an explicit "send anyway" before credentials go out.
* A remembered password the server refuses is deleted rather than retried.
* **Sign out** revokes the live session in memory, then deletes the keychain entry (what the native-flow docs prescribe; `/auth/logout` only
  revokes cookie sessions, so it isn't called).
* **Loopback dashboards** (no accounts): the page injects `window.__HERMES_SESSION_TOKEN__`
  (`web_server_dashboard.py:167/221`, docs ~L7722), rotated on every restart. The app reads it
  from `/` on each connect and sends it as `X-Hermes-Session-Token` / `?token=`
  (`web_server.py:299 _has_valid_session_token`).
* **Advanced:** a deployment session token (`HERMES_DASHBOARD_SESSION_TOKEN`) can still be pasted;
  it is sent the same way.
* What is stored: one keychain item per server (service `dev.hermes-desktop`, account = connection id)
  holding JSON `{kind, access/refresh tokens or token, provider, username, password?}`. The config
  file only has the server's name and address; the shown username lives in the local cache DB.

**What does not exist.** There is no official endpoint that issues an API key or gateway token
after a password login. The `/v1` API server (port 8642) is separate and uses its own
`API_SERVER_KEY` bearer, so a dashboard sign-in does not reach `/v1`. `/api/env/reveal` can show
secrets to an admin, but using it to fish out keys would be off-label, so the app doesn't.

## Two socket modes
A gateway WebSocket is either a **plain gateway** socket (the dashboard's `server.dispatch`;
contracts in `tui_gateway/contracts/*.py`) or a **session authority** socket (`gateway.ready`
carries `session_authority: true`; contracts in `tui_gateway/contracts/canonical*.py`). Both
refuse any key a method's contract does not declare: the plain socket with `4000 invalid params
for <method>: <key>: Extra inputs are not permitted …`, the authority with `4001 invalid_params`
(which also covers missing required keys). The shapes that differ are built in
`crates/core/src/wire.rs`, checked against hermes-agent v0.22.5, and the mock enforces both
contract sets so a stray key fails the tests.

| Call | Plain gateway | Session authority |
|---|---|---|
| `session.create` retry key | `idempotency_key` | `request_id` |
| `prompt.submit` | `{session_id, text, profile}`; images staged first with `image.attach_bytes` | adds `input_id` and `attachments:[{path, mime}]` (images from `/api/chat/image-upload`) |
| `session.list` | stored sessions | `scope:"stored"` (the default scope is live sessions only) |
| `session.interrupt` | `{session_id}` | adds `execution_generation` from a fresh `session.resume` |
| Rename, compact | `session.title`, `session.compress` | `session.mutate {request_id, expected_revision, operation, payload}` (compress also `expected_generation`) |

`toolsets.list` takes only `{session_id}` in both modes (it reads that session's profile).

## Bot Mode
| What | Call |
|---|---|
| Roster | `profiles.list {}` (polled every 5 s; the UI updates only when something changed). `canonical_session.id` is the Bot Chat's registry row; `resolved_id` is its live compression tip, which `preview` and `last_active` describe and which the app opens |
| Metadata | `profiles.configure {name, ui_meta:{"hermes-bots":{…}}, ui_meta_expected_revisions}`, a compare-and-swap that retries up to 3 times. The patch is applied to the raw stored object, so keys this client can't parse survive |
| New bot | `profiles.create {name, description, model?}` |
| Canonical chat | The roster's `canonical_session.resolved_id`, else `session.list {profile, title:"Bot Chat", include_hidden:true}` (its row's `resolved_id`), else `session.create {profile, title:"Bot Chat", hidden:true, follow_profile_config:true, source:"gui", idempotency_key\|request_id}`; then `session.resume {…, defer_history:true, omit_messages:true}`. Resuming the registry row instead would replay the transcript from before the first compaction |
| Send | `prompt.submit`, shaped per socket mode (above) |
| `/new`, `/reset` | Become `/compact` through `session.compress` (fallback: `session.mutate {operation:"compress"}`), so the bot keeps a single chat |
| @mentions | The official note `[@mentions resolved from the Bot Mode roster — …]` is appended, and the agent uses `message_agent` |
| Agent notices | Rows with `role:user` that start `Message from 🤖 Name (@handle[@conn]):` are shown as cards |
| Unread | Computed client-side as `max(canonical.last_active, last_session.last_active)` compared with a SQLite watermark |

## One continuous chat per bot
No custom backend: the stream is assembled from official session calls, and the "long break"
policy only chooses which official call runs before the next message.
| What | Call |
|---|---|
| Stream | Canonical Bot Chat (above) plus `session.list {profile, limit:200, include_hidden:true}`, keeping only conversations (`message_count > 0`, source not `cron`/`kanban`/`tool`/`oneshot`; on a real server the newest rows are mostly empty cron runs and idle TUI windows); up to 12 other sessions' latest 60 messages each (`GET /api/sessions/{id}/messages?profile=&limit=60&order=latest`) are merged by timestamp. Each foreign session gets a marker chip "Title · from source" |
| Date dividers | Client-side, from message timestamps |
| Long break → **Compact** (default) | `session.compress {session_id, profile}` on the canonical session; if refused (-32601), `session.mutate {operation:"compress"}`. Same session, older context summarised |
| Long break → **New session** | `session.create {profile, title:"Bot Chat", hidden:true, …}` + resume, so the new turn starts with an empty context; older sessions stay visible in the stream |
| Long break → **Keep going** | Nothing |
| "Long break" | Time since the bot's last user/assistant message ≥ Settings → Chat (1 h–1 day, default 6 h) |
Why client-side: Hermes never resets a session on inactivity (the legacy `session_reset` config is
ignored; `hermes-session-reset-policy` is an opt-in plugin), so the client decides when to call
the official compress/create methods.

## Slash commands
* Catalog: `commands.catalog {profile}` → `{categories:[{name, pairs}], pairs, commands:{"/x":
  {argument_mode, desktop}}, skills, …}` (live, includes skill commands), merged with a bundled
  copy of the official `COMMAND_REGISTRY` (`hermes_cli/commands.py`: name, description, category,
  aliases, args_hint, subcommands, gateway_only) generated by `scripts/gen_slash_registry.py` —
  71 commands available on the gateway/Discord.
* Detection: text is a command only when its first word (`/name`) is in the merged catalog or a
  desktop-local command (`/usage /insights /stop /approve /deny`). Anything else that starts with
  `/` (a path such as `/etc/hosts`) is sent as an ordinary prompt.
* Run: `slash.exec {session_id, command, profile}` → `{type:"exec", output}` (shown as an
  expandable chip) or `{type:"skill", message}` (sent as the next turn). The shared gateway only
  executes a reviewed read set (`/help /commands /status /context /version /whoami /title`);
  others answer `unsupported_command`, which the chip reports ("not available here").
* Answered locally: `/new` `/reset` (compact, above), `/usage` `/insights` (Usage page), `/stop`,
  `/approve`, `/deny`.

## Usage
* `GET /api/analytics/usage?days=N&profile=P` (`hermes_cli/web_routers/analytics.py`), once per
  bot: `daily[{day, input_tokens, output_tokens, cache_read_tokens, reasoning_tokens,
  estimated_cost, actual_cost, sessions, api_calls}]`, `by_model`, `totals`.
* Cost shows `actual_cost` when the provider reported it, otherwise `estimated_cost`.

## Files
* Images up to 25 MB are sent with `POST /api/chat/image-upload {data_url, filename}`, which
  returns `{path, mime_type}`, and are then passed in `attachments`.
* Other files up to 256 MB use `file.attach {session_id, name, data_url}`, which returns
  `ref_text`. That reference is prepended to the prompt.
* Agent output arrives as `MEDIA:/abs/path` tags. These are previewed (images inline) and can be
  opened or saved. Downloads stream to a `.part` file, which is renamed when complete.

## Event chips
* Routines are cron jobs managed by the agent's `cronjob` tool (`tools/cronjob_tools.py`:
  `action` = create | list | update | pause | resume | remove | run | cancel, `name`, `job_id`).
  The chip title comes from `tool.start.args` live (`ToolStartPayload.args` in
  `tui_gateway/contracts/events.py`) and from `tool_calls[].function.arguments` in history.
* Bot-to-bot messages are `Message from 🤖 …` user rows (Bot Mode). Consecutive rows from one sender
  are grouped on the client.
* Approvals answered and files saved are local, session-only events: there is no official
  history row for them.

## Apps & Connectors
Research result: Hermes has official directories, and the app uses only those.
* **Hosted connectors**: `connectors.catalog {profile}` → `{connectors:[{slug,name,description,
  category,logo_url}]}`; `connectors.list {profile, owner:{type:"account"}}` for state;
  `connectors.connect {profile, owner, connectors:[slug], reconnect?}` returns targets whose
  `connect_url` is opened in the browser (OAuth happens on the official site); `connectors.tools`.
  `logo_url` (https only) is fetched by the core with its own client: no Hermes credentials,
  HTTPS-only redirects, 15 s timeout, 1 MB cap, `image/*` only, cached a week.
  Source: `tui_gateway/contracts/connectors.py`, `connectors_operation.py`. May need a Nous
  account (`NEEDS_NOUS_AUTH`, `CATALOG_UNAVAILABLE`).
* **Curated MCP presets**: `mcp.catalog`, `mcp.servers.add {name, preset}` / `remove`. Presets
  that need API keys list them in `requires`; the app tells the user to set them on the host
  and never sends secrets.
* **Toolsets**: `toolsets.list {session_id}` (the bot's live session), `tools.configure {action, names, profile}`.
* **Skills**: `skills.manage {action:"list"}`.
* **Plugin catalog** (human-reviewed YAML in `plugin-catalog/`, `plugins.catalog.lookup`): exists
  but is **not shown yet**.
* On the shared gateway these are "older handler" methods. They answer only when the
  connection's ticket carries the full interactive grant, otherwise -32601. The page shows a
  per-source notice in that case.
* Threads: `GET /api/sessions?profile=&limit=31&order=recent&min_messages=1&exclude_sources=cron,kanban,tool,oneshot` (each compression chain once, as its tip, by latest activity, with `last_active`); if REST is unavailable, `session.list` filtered the same way client-side, whose rows carry only `started_at`, so no time is shown.
* History: `GET /api/sessions/{id}/messages`. A 404 for a session the gateway just created or resumed means it has no stored rows yet (a row appears with its first prompt): shown as an empty chat. Plus `profiles.describe {name}`.

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
* **Plugin catalog** browsing in Apps; **MCP env/keys entry** (must be set on the host).
* **Usage**: no per-message or per-turn cost; daily buckets only. `session.usage` (live session
  usage) is refused on the shared gateway, and `usage.bars` (rate-limit bars) / `insights.get`
  are older-handler methods that need a Nous account, so live limits are not shown.
  `actual_cost` is 0 unless the provider reports it, so most figures are estimates.
* **Slash commands**: beyond the reviewed read set, `slash.exec` returns `unsupported_command` on
  the shared gateway; the commands are listed for discovery but cannot run from here.
  `session.title` and `session.usage` are also refused.
* **Long-break chips** and slash-output chips are session-only (no history row).
* **Updates**: there is no official update feed for this client, so About links to the repo.
