# AGENTS.md

Guidance for coding agents (and humans) working on Hermes Desktop.

## What this is

A native, cross-platform (macOS, Windows, Linux X11/Wayland) desktop client for
[Hermes Agent](https://hermes-agent.nousresearch.com), built in Rust on [GPUI](https://gpui.rs)
(`gpui-pre` 0.3.12 + `gpui-component` 0.10). It has no webview and no Electron.

**Hard rule: use only official Hermes protocols.** These are the gateway JSON-RPC WebSocket
(`/api/ws`), the dashboard REST API (`/api/...`) and the OpenAI-compatible API server (`/v1`). Never
invent endpoints or run a backend of your own. If something has no official API, document the gap
in `docs/PROTOCOL.md` instead of working around it. Cite the docs page, or the
`NousResearch/hermes-agent` source file, for every wire shape you add.

## Layout

```
crates/core   hermes-core: protocol clients (gateway.rs WS JSON-RPC, rest.rs),
              files.rs (link/open safety rules, portable names, quarantine flag),
              logos.rs (connector logos), housekeeping.rs (log rotation, cache caps),
              roster.rs (Bot Mode: bots = profiles, mentions, agent notices),
              transcript.rs (history rows, MEDIA tags), store.rs (SQLite cache),
              conn.rs (config + OS keychain), service.rs (actor: Cmd in, Ev out)
              auth.rs (sign-in: probe, native PKCE password login, refresh, ws tickets)
              apps.rs (Apps page: param builders + parsers for connectors/mcp/toolsets/skills)
              slash.rs + slash_registry.json (slash commands; regenerate with
              scripts/gen_slash_registry.py <hermes-agent checkout>), usage.rs (Usage page)
crates/mock   hermes-mock: stand-in backend that follows the documented wire (tests + demos)
crates/app    hermes-desktop: GPUI binary. app.rs = state, actions, shortcuts, RPC routing;
              view.rs = sidebar, chat, composer, info pane; overlays.rs = palette, settings,
              apps, new bot; onboard.rs = first-run / add-server / sign-in flow; ui.rs = shared controls
              (focusable, labelled); theme.rs = palette + prefs + contrast; notify.rs = OS notifications
              (notify-rust, never a shell); guard.rs = link clicks and "Open" through files.rs;
              chrome.rs = per-platform window chrome (Linux client-side decorations); tests.rs
docs/         PROTOCOL.md (wire notes and API gaps), ci/ (drafted CI), screenshots/
```

Threading: the UI thread only renders. All I/O runs on a 2-worker tokio runtime inside
`Service`. The UI sends `Cmd` and drains `Ev` in batches (one repaint per batch).

## Commands

```sh
cargo build                                  # debug (debug info off to save disk)
cargo build --release -p hermes-desktop      # optimized app
cargo test -p hermes-core                    # unit + end-to-end tests against the mock
cargo clippy --workspace --all-targets
cargo run -p hermes-mock -- --port 9119 --token dev-token   # fake Hermes (loopback, no accounts)
cargo run -p hermes-mock -- --port 9120 --basic-auth jack:hunter2   # fake gated Hermes (sign-in)
HERMES_DESKTOP_URL=http://127.0.0.1:9119 HERMES_DESKTOP_TOKEN=dev-token cargo run -p hermes-desktop
scripts/screenshots.sh                       # macOS: scripted demo + window-only screenshots
```

Dev-only environment variables (read through `conn::dev_env`; release builds ignore all of them):
* `HERMES_DESKTOP_URL` / `HERMES_DESKTOP_TOKEN` / `HERMES_DESKTOP_NAME` seed a connection. The
  token stays in memory and is never written to disk.
* `HERMES_DESKTOP_HOME` puts config and cache in a different directory.
* `HERMES_DESKTOP_EPHEMERAL_SECRETS=1` keeps credentials in process memory instead of the OS
  keychain (tests and demos; never on disk).
* `HERMES_DESKTOP_SCRIPT` holds `;`-separated steps (`wait:ms`, `select:profile`, `type:text`,
  `send`, `attach:path`, `share`, `hidden`, `newbot`, `setup`, `palette`, `query:text`,
  `settings:<section>`, `apps`, `category:<chip>`, `expand:<app id>`, `info`, `edit`,
  `thread:<session id>`, `theme:system|light|dark`, `size:WxH`, `unfold` (expand every event chip), `approve:<choice>`,
  `saveas:<remote path>` (download into the demo home without a dialog), `key:<binding>` (dispatch a
  keystroke, e.g. `key:secondary-2`), `cmdheld[:off]` (show the ⌘ number badges), `usage`,
  `usagebot:<profile>`, `pin:<profile>`, `sidebar`, `gap:<hours>` (age the chat to trigger the
  long-break policy), `resizebench:<n>` (resize every frame and log render timings), `onboard:welcome|server|signin|token|bot`,
  `addr:<url>` (type an address and probe it), `pick:<n>` (choose a local server), `creds:user|pass`,
  `showpw`, `tokenfield:<token>`, `bottitle:<name>`, `next` (the flow's primary button),
  `signout[:<server>]`, `resignin[:<server>]`, `close`) for
  reproducible demos. A scripted window opens unfocused and never activates the app or raises
  itself; `scripts/screenshots.sh` therefore uses the debug build.

## Rules

* **Secrets** go in the OS keychain only (`conn::Secrets`, via `keyring`). Never write tokens to
  config files, logs, tests or the repo.
* **Platform code** sits behind `cfg` (see `view::notify_os`). Only macOS is tested locally. When
  you add platform behaviour, say what is untested in `CHANGELOG.md`.
* **Performance**: avoid per-frame allocation in hot paths. Coalesce stream events, and only
  `cx.notify()` on change. The roster poll (5 s) emits only when something changed.
* **Tests**: every protocol behaviour gets a mock-backed test in `crates/core/tests/e2e.rs`. When
  you change the mock, keep it faithful to the docs.
* **UI**: build on `ui.rs` controls. Sizes sit on the 4/8 px grid (controls 24/28/32/36 px,
  icons 16 px in 32 px boxes); scale text with `Palette::t` and spacing with `Palette::g`. Check
  both themes and a narrow window with window-only screenshots before committing UI changes.
* **Gateway methods** that the shared gateway may refuse (`connectors.*`, `mcp.*`, ...) go through
  `Cmd::Rpc`; a -32601 becomes a friendly notice, never a fallback of our own.
* **CI** is drafted in `docs/ci/`. The pushing token lacks the `workflow` scope, so do not add
  `.github/workflows/` until it has it.

## Versioning (semver)

`VERSION` holds the app version and must match `[workspace.package] version` in `Cargo.toml`.
**Every change that alters behaviour bumps `VERSION`, `Cargo.toml` and `CHANGELOG.md` in the same
commit:**
* MAJOR: incompatible changes to stored config/cache, or dropping support for a Hermes version.
* MINOR: new features.
* PATCH: fixes and internal changes with no visible behaviour change.
Pre-1.0, breaking changes bump MINOR.
