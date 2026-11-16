# Changelog

All notable changes to this project. Format: [Keep a Changelog](https://keepachangelog.com),
versioning: [SemVer](https://semver.org) (see AGENTS.md).

## [0.2.1] - 2026-11-16

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
