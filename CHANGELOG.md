# Changelog

All notable changes to this project. Format: [Keep a Changelog](https://keepachangelog.com),
versioning: [SemVer](https://semver.org) (see AGENTS.md).

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
