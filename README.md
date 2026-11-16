# Hermes Desktop

A fast, lightweight, native desktop client for [Hermes Agent](https://hermes-agent.nousresearch.com),
written in Rust on GPUI, with no Electron and no webview. It targets macOS, Windows and Linux
(X11 and Wayland).

Every Hermes profile appears as a **bot**, using the same Bot Mode model as the official
desktop app. Each bot has one canonical "Bot Chat", and the roster shows its unread state. You
can @mention other bots, and messages from bot to bot (`Message from 🤖 …`) appear as distinct
cards. Bots on other machines appear as additional connections.

![chat](docs/screenshots/chat.png)

Also included: a ⌘K command palette, per-bot info pane with threads and editing, a sectioned
Settings window (servers, appearance, notifications, chat, files, shortcuts, privacy, advanced),
and an **Apps & Connectors** page built on Hermes' official connector, MCP, toolset and skill
catalogs. More screenshots are in [docs/screenshots](docs/screenshots).

Each bot is **one continuous chat**: all of its Hermes sessions (CLI, Telegram, Bot Chat…) are
merged into a single stream with date dividers, and after a long break Hermes compacts the
context (or starts a fresh session) behind the scenes to save tokens. Pin bots to the top, type
`/` for every Hermes slash command with arguments and descriptions, and open **Usage** (next to
your name) for tokens, cost, requests and sessions per bot.

| Shortcut (Ctrl on Windows/Linux) | Action |
|---|---|
| ⌘K | Command palette and search |
| ⌘N | New bot |
| ⌘1–9 | Jump to bot (hold ⌘ to see the numbers; pinned bots come first) |
| ⇧⌘] / ⇧⌘[ (or ⌃Tab / ⌃⇧Tab) | Next / previous bot |
| ⇧⌘P | Pin or unpin the bot |
| ⌘B | Show or hide the sidebar (remembered) |
| ⌘L | Focus the message box |
| ⌘I | Bot info & threads |
| ⇧⌘A | Apps & Connectors |
| ⇧⌘U | Usage |
| ⌘, | Settings |
| ⌘U / ⇧⌘C / ⇧⌘. | Attach files / copy conversation / stop reply |
| / and @ | Slash commands / mention a bot |
| Esc | Close the top overlay |

## Connect

Run `hermes dashboard` (or `hermes serve`) on the machine that hosts your agent. Open
**Settings** (the gear icon, bottom left) and enter the dashboard URL and its session token.
The token is kept in the OS keychain (macOS Keychain, Windows Credential Manager, or Secret
Service on Linux). The client only speaks Hermes' official protocols: the gateway WebSocket
`/api/ws`, the dashboard REST API and the `/v1` API server. See [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Build

```sh
cargo build --release -p hermes-desktop
./target/release/hermes-desktop
```

Linux needs `libxkbcommon`, `wayland`, `vulkan`, `fontconfig` and `dbus` development packages
(see `docs/ci/ci.yml`).

## Try it without Hermes

```sh
cargo run --release -p hermes-mock -- --port 9119 --token dev-token &
HERMES_DESKTOP_URL=http://127.0.0.1:9119 HERMES_DESKTOP_TOKEN=dev-token ./target/release/hermes-desktop
```

See [PLAN.md](PLAN.md) for the design and milestones, and [AGENTS.md](AGENTS.md) for contributing.
