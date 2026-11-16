# Changelog

All notable changes to this project. Format: [Keep a Changelog](https://keepachangelog.com),
versioning: [SemVer](https://semver.org) (see AGENTS.md).

## [0.5.0] - 2026-11-16

### Added
- **Sign in with your Hermes username and password**, the same account as the web dashboard.
  The app uses Hermes' official native sign-in (RFC 8252: `/auth/native/authorize` →
  `/auth/password-login` → `/auth/native/token`), gets bearer tokens and refreshes them before they
  expire. Each WebSocket connect uses a fresh single-use `/api/auth/ws-ticket`. Research and
  citations are in docs/PROTOCOL.md → Sign-in.
- **New first-run flow**: a welcome screen, then choosing a server (Hermes found on this computer
  on ports 9119/9120, or a typed remote or Tailscale address with a live "Checking…" health
  probe), then sign in (show password, Enter to submit, *Stay signed in*), then picking or
  creating your first bot. Clear errors cover a wrong password, too many attempts, an unreachable
  server, a name that doesn't resolve, a non-Hermes server, and TLS problems (untrusted, wrong
  name, expired, or HTTPS against an HTTP port).
- **Silent re-authentication**: when a session ends, the app refreshes it, or signs in again with
  the password kept in the keychain if you chose *Stay signed in*. Otherwise the server shows
  *Sign in required*.
- **Settings → Account**: each saved server shows who you're signed in as, with **Sign out**,
  **Sign in**, **Switch server** and remove. **Add server** opens the same flow. Any number of
  servers can be saved.
- Dashboards on this computer that have no accounts connect with no typing: the app reads the
  page's per-run session token, which is how the official dashboard page authenticates.
- Mock: `--basic-auth USER:PASS` and `--access-ttl SECS` gate it like a public dashboard (native
  sign-in endpoints, tickets, 429 rate limit). New `tests/signin.rs`.

### Changed
- The session-token field is now *Advanced: use a session token* in the sign-in flow.
- Keychain entries hold JSON (tokens, provider, username, optional password). Older plain-token
  entries still work.
- "Token refused" is now "Sign in required".

## [0.4.0] - 2026-11-16

### Added
- **Keyboard shortcuts**: hold ⌘ (Ctrl on Windows/Linux) to see 1–9 badges on the top bots and
  press ⌘1–9 to jump. New: ⇧⌘]/⇧⌘[ and ⌃Tab/⌃⇧Tab (next/previous bot), ⇧⌘P (pin), ⌘B (sidebar),
  ⌘L (focus composer), ⇧⌘U (usage). Settings → Shortcuts lists every binding, plus a View menu.
- **Pinning**: pin bots from the info pane, the palette or ⇧⌘P. Pinned bots sort first (stored in
  Bot Mode `ui_meta["hermes-bots"].pinned`) and take the first number shortcuts.
- **One continuous chat per bot**: all of a bot's sessions merge into one stream with "Today /
  Yesterday / Mon 12 Nov" dividers and "Title · from telegram" markers. After a long break
  (Settings → Chat: Keep going / Compact / New session, after 1 h–1 day, default Compact after
  6 h) the app calls Hermes' `session.compress` or starts a fresh Bot Chat session before sending,
  with a "Back after 8 hours · context compacted" chip. Mapping in docs/PROTOCOL.md.
- **Slash commands**: typing `/` opens an autocomplete popover with all 71 gateway commands (from
  the official registry) plus the bot's live `commands.catalog` (skills), showing category,
  arguments, aliases and a description. Runs through `slash.exec`; output appears as a chip.
- **Usage page** (button next to your name, ⇧⌘U, `/usage`): tokens, cost, requests and sessions per
  bot and per day (7/30/90 days), a daily chart, per-bot and per-model tables, from
  `GET /api/analytics/usage`. Unavailable data is listed on the page and in PROTOCOL.md.
- **Collapsible sidebar** (button + ⌘B, remembered). Below 760 px wide it steps aside
  automatically and the toggle shows it as an overlay.

### Changed
- Minimum window size is 560×420 on every platform; the info pane, then the sidebar, give way at
  narrow widths and Share becomes an icon. Measured with `resizebench`: one frame per resize
  step, view build p95 0.12 ms (layout/paint not included).
- Opening a bot no longer reorders the sidebar (empty sessions don't count as activity).

### Untested
- Windows and Linux: Ctrl-based badges/shortcuts, minimum size and the traffic-light inset logic
  (macOS-only padding) were only run on macOS.

## [0.3.0] - 2026-11-16

### Changed
- **System events are now small, muted, centred one-line chips** instead of bubbles and cards,
  so the conversation stays the focus. Each chip has an icon or avatar and a short label, shows
  its time on hover, and expands in place when clicked:
  - bot-to-bot messages: "Message from (avatar) Research Buddy", and consecutive messages from one
    sender fold into "2 messages with (avatar) Research Buddy". Expanding shows every message in
    full, with an "Open Research Buddy" button. This replaces the large purple notice bubble.
  - tool calls and subagents: one chip per call, or one summary chip per turn ("Used 2 tools and
    2 subagents"), with a spinner while running. Expanding lists each step with its detail,
    duration and result.
  - routines (Hermes cron jobs): `cronjob` calls are titled from their arguments, e.g.
    "Updated routine · News watch", from both live `tool.start.args` and stored
    `tool_calls[].function.arguments`.
  - timeline rows (model switch, compaction, process complete).

### Added
- Session-only chips for answered approvals ("Approved · rm -rf build/") and saved files ("Saved
  report.pdf to Downloads", with Show in folder). Hermes history does not record these, so they
  are not kept after a reload. The save toast is gone.
- Demo-script steps `unfold`, `approve:<choice>` and `saveas:<remote path>`.

 - 2026-11-16

### Added
- `size:WxH` demo-script step, used for the narrow-window screenshot.
- Screenshots of every main screen in `docs/screenshots/`.

### Fixed
- The `type:` demo step leaves the cursor at the end, so the @mention popup opens.
- Settings → Shortcuts shows ⌘1–9 for "Jump to bot"; About says macOS/Windows/Linux and no
  longer breaks a command across lines.

## [0.2.0] - 2026-11-16

### Added
- **Command palette** (⌘K / Ctrl+K): fuzzy search across bots, their threads and every action,
  grouped, with keyboard navigation and shortcut hints.
- **Keyboard shortcuts** throughout (new bot, settings, apps, info pane, next/previous bot,
  ⌘1–9 to jump to a bot, focus search, copy conversation, stop reply, attach, Escape closes the
  top overlay), listed in Settings → Shortcuts and the menu bar.
- **Bot info pane** (⌘I): profile details, capabilities from `profiles.describe` (model, toolsets,
  skills, MCP servers), and **threads**, the bot's other stored sessions from `session.list`, which
  open in place with a back arrow. Edit name, colour (Bot Mode `ui_meta`) and description
  (`profiles.configure`). The new-bot form takes an optional profile id and a description.
- **Settings window**, persisted in `config.json`: Account and servers (multiple endpoints, edit,
  remove, status), Appearance (system/light/dark, 8 accents, font size 12–18, density), Notifications
  and sounds, Chat (Enter to send, Markdown, timestamps, tool activity), Files and media (download
  folder, image previews), Shortcuts, Privacy (hide previews, clear cache), Advanced (log level,
  name-only network debug log, open logs folder, reconnect) and About.
- **Apps & Connectors** (⇧⌘A): search, category chips, Connected / Recommended / Toolsets / Skills
  sections, and Connect / Manage. It uses only official gateway methods: `connectors.catalog`,
  `connectors.list`, `connectors.connect` (opens the official `connect_url` in the browser),
  `connectors.tools`, `mcp.catalog`, `mcp.servers.add/remove`, `toolsets.list`, `tools.configure`
  and `skills.manage`. Sources this Hermes does not serve show a notice; nothing is invented.
- Generic `Cmd::Rpc` / `Ev::Rpc` in the core service for official one-off calls.
- Loading skeletons for the roster, chat, info pane and Apps grid; an empty state with starter
  prompts; hover, pressed and focus states on every control.
- Logs are written to `<data dir>/logs/hermes-desktop.log`.

### Changed
- UI rebuilt on a 4/8 px grid with shared controls (`ui.rs`) and a richer palette (`theme.rs`).
- Messages sit in a centred 760 px column. The info pane hides itself on narrow windows.
- `session.list` previews are stripped of inline Markdown.

### Fixed
- The composer's + button is now vertically centred: every composer control is a 32 px box
  aligned with the input's first line.

### Untested
- Windows toast notifications (PowerShell) and sounds on Windows and Linux.

## [0.1.0] - 2026-11-16

### Added
- `hermes-core`: a gateway JSON-RPC client over `/api/ws` with reconnect/backoff, the JSON
  heartbeat, `client.capabilities` and server→client requests; a dashboard REST client (history,
  image upload, streamed downloads); an API-server client (`/v1/capabilities`, runs and SSE,
  multiplexed `/p/<profile>`).
- Bot Mode: the roster from `profiles.list`, `ui_meta["hermes-bots"]` metadata (title, colour,
  hidden, sections kept intact), compare-and-swap writes through `profiles.configure`, find/create
  of the canonical "Bot Chat", `/new` → compact, `profiles.create` for new bots.
- @mention autocomplete and resolution against the live roster (the official "resolved from
  the Bot Mode roster" note), with `Message from 🤖 …` agent notices rendered as distinct cards.
- GPUI app: Grok-style sidebar (current bot, search, new bot, roster with avatar, preview, time
  and unread dot, account footer), chat bubbles, Markdown with highlighted code, tool-activity
  and subagent cards, inline images, file cards with Open and Save, approvals, Share (copies
  Markdown).
- Composer: auto-growing input, + to attach, drag and drop, paste of files or images, @
  autocomplete, send/stop. Large files are read off the UI thread, with the 256 MB server limit
  enforced up front.
- SQLite cache (roster, recent messages, unread watermarks) for instant start-up. Tokens are
  kept in the OS keychain.
- Follows the OS light/dark appearance (dark by default). OS notifications on macOS
  (`osascript`) and Linux (`notify-send`).
- `hermes-mock` backend and end-to-end tests.

### Untested
- Windows and Linux builds and runtime behaviour (only macOS was available). Windows
  notifications are not implemented.
