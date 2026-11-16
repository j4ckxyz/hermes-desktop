//! Application state: the roster, chats, composer, overlays and the bridge to the core service.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::*;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::text::TextViewState;
use gpui_component::{Theme, ThemeMode};
use hermes_core::apps::{self, AppItem, AppKind, AppsSnapshot};
use hermes_core::conn::{ConnKind, Config, Connection, Prefs, Secrets};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{self, Bot, BotKey};
use hermes_core::service::{Cmd, Ev, LocalFile, MAX_FILE_BYTES};
use hermes_core::transcript::{self, Activity, Attachment, ChatMessage, Role};
use hermes_core::Service;
use serde_json::{json, Value};

use crate::theme::{Palette, DEFAULT_ACCENT};

actions!(
    hermes,
    [Quit, OpenPalette, NewBot, OpenSettings, OpenApps, ToggleInfo, FocusSearch, NextBot, PrevBot, CopyConversation, StopTurn, AttachFiles]
);

/// Every shortcut, shown in Settings → Shortcuts and the palette.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("secondary-k", "Command palette"),
    ("secondary-n", "New bot"),
    ("secondary-,", "Settings"),
    ("secondary-shift-a", "Apps & connectors"),
    ("secondary-i", "Bot info & threads"),
    ("secondary-f", "Search bots"),
    ("secondary-1", "Jump to bot 1–9"),
    ("secondary-alt-down", "Next bot"),
    ("secondary-alt-up", "Previous bot"),
    ("secondary-u", "Attach files"),
    ("secondary-shift-c", "Copy conversation as Markdown"),
    ("secondary-.", "Stop the reply"),
    ("escape", "Close a dialog or panel"),
];

/// `secondary` is Cmd on macOS and Ctrl elsewhere.
/// Leaked once at start-up: GPUI wants `&'static str` bindings.
pub fn ks(binding: &str) -> &'static str {
    Box::leak(binding.replace("secondary", if cfg!(target_os = "macos") { "cmd" } else { "ctrl" }).into_boxed_str())
}

pub fn bind_keys(cx: &mut App) {
    let c = Some("Hermes");
    cx.bind_keys([
        KeyBinding::new(ks("secondary-q"), Quit, None),
        KeyBinding::new(ks("secondary-k"), OpenPalette, c),
        KeyBinding::new(ks("secondary-n"), NewBot, c),
        KeyBinding::new(ks("secondary-,"), OpenSettings, c),
        KeyBinding::new(ks("secondary-shift-a"), OpenApps, c),
        KeyBinding::new(ks("secondary-i"), ToggleInfo, c),
        KeyBinding::new(ks("secondary-f"), FocusSearch, c),
        KeyBinding::new(ks("secondary-alt-down"), NextBot, c),
        KeyBinding::new(ks("secondary-alt-up"), PrevBot, c),
        KeyBinding::new(ks("secondary-u"), AttachFiles, c),
        KeyBinding::new(ks("secondary-shift-c"), CopyConversation, c),
        KeyBinding::new(ks("secondary-."), StopTurn, c),
    ]);
}

pub fn shortcut_for(desc: &str) -> Option<&'static str> {
    SHORTCUTS.iter().find(|(_, d)| *d == desc).map(|(k, _)| *k)
}

pub struct MsgVm {
    pub m: ChatMessage,
    pub md: Option<Entity<TextViewState>>,
}

#[derive(Default)]
pub struct Chat {
    pub msgs: Vec<MsgVm>,
    pub streaming: bool,
    pub awaiting: bool,
    pub status: Option<String>,
    pub error: Option<String>,
    pub loaded: bool,
}

pub struct Approval {
    pub conn: String,
    pub key: Option<BotKey>,
    pub request_id: String,
    pub command: String,
    pub description: String,
    pub choices: Vec<String>,
}

pub struct MentionPopup {
    pub at: usize,
    pub cursor: usize,
    pub items: Vec<BotKey>,
    pub sel: usize,
}

pub struct SetupForm {
    pub name: Entity<InputState>,
    pub url: Entity<InputState>,
    pub token: Entity<InputState>,
    pub editing: Option<String>,
    pub error: Option<String>,
}

pub struct NewBotForm {
    pub name: Entity<InputState>,
    pub title: Entity<InputState>,
    pub description: Entity<InputState>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Account,
    Appearance,
    Notifications,
    Chat,
    Files,
    Shortcuts,
    Privacy,
    Advanced,
    About,
}

impl Section {
    pub const ALL: [Section; 9] =
        [Section::Account, Section::Appearance, Section::Notifications, Section::Chat, Section::Files, Section::Shortcuts, Section::Privacy, Section::Advanced, Section::About];
    pub fn label(self) -> &'static str {
        match self {
            Section::Account => "Account & servers",
            Section::Appearance => "Appearance",
            Section::Notifications => "Notifications",
            Section::Chat => "Chat",
            Section::Files => "Files & media",
            Section::Shortcuts => "Shortcuts",
            Section::Privacy => "Privacy",
            Section::Advanced => "Advanced",
            Section::About => "About",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Section::Account => "server",
            Section::Appearance => "palette",
            Section::Notifications => "bell",
            Section::Chat => "message-circle",
            Section::Files => "folder",
            Section::Shortcuts => "keyboard",
            Section::Privacy => "shield",
            Section::Advanced => "sliders-horizontal",
            Section::About => "info",
        }
    }
    fn parse(s: &str) -> Section {
        Section::ALL.into_iter().find(|x| format!("{x:?}").eq_ignore_ascii_case(s)).unwrap_or(Section::Account)
    }
}

pub struct SettingsState {
    pub section: Section,
    pub name: Entity<InputState>,
}

pub struct PaletteState {
    pub input: Entity<InputState>,
    pub sel: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    NewBot,
    Settings,
    Apps,
    Info,
    Share,
    Theme,
    Hidden,
    Compact,
    AddServer,
    BotChat,
}

#[derive(Clone)]
pub enum PaletteTarget {
    Bot(BotKey),
    Thread(BotKey, String),
    Action(Act),
}

#[derive(Clone)]
pub struct PaletteItem {
    pub target: PaletteTarget,
    pub title: String,
    pub subtitle: String,
    pub keys: Option<&'static str>,
}

pub struct AppsState {
    pub key: BotKey,
    pub search: Entity<InputState>,
    pub category: String,
    pub gen: u64,
    pub raw: HashMap<&'static str, Result<Value, String>>,
    pub busy: HashSet<String>,
    pub expanded: Option<String>,
    pub tools: HashMap<String, ToolRows>,
}

/// `(name, description, facet)` rows from `connectors.tools`, or a friendly error.
pub type ToolRows = Result<Vec<(String, String, String)>, String>;

const APP_SOURCES: [(&str, &str); 6] = [
    ("connectors.catalog", "catalog"),
    ("connectors.list", "list"),
    ("mcp.catalog", "mcp"),
    ("toolsets.list", "toolsets"),
    ("skills.manage", "skills"),
    ("", ""),
];

impl AppsState {
    pub fn loading(&self) -> bool {
        self.raw.len() < 5
    }

    /// Join what each official source said. A failed source is listed in `unavailable`.
    pub fn snapshot(&self) -> AppsSnapshot {
        let mut snap = AppsSnapshot::default();
        match self.raw.get("catalog") {
            Some(Ok(cat)) => snap.items.extend(apps::connectors(cat, self.raw.get("list").and_then(|r| r.as_ref().ok()))),
            Some(Err(e)) => snap.unavailable.push((AppKind::Connector, e.clone())),
            None => {}
        }
        match self.raw.get("mcp") {
            Some(Ok(v)) => snap.items.extend(apps::mcp(v)),
            Some(Err(e)) => snap.unavailable.push((AppKind::Mcp, e.clone())),
            None => {}
        }
        match self.raw.get("toolsets") {
            Some(Ok(v)) => snap.items.extend(apps::toolsets(v)),
            Some(Err(e)) => snap.unavailable.push((AppKind::Toolset, e.clone())),
            None => {}
        }
        match self.raw.get("skills") {
            Some(Ok(v)) => snap.items.extend(apps::skills(v)),
            Some(Err(e)) => snap.unavailable.push((AppKind::Skill, e.clone())),
            None => {}
        }
        snap
    }
}

#[derive(Clone, Debug)]
pub struct ThreadRow {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub source: String,
    pub last: f64,
    pub count: u64,
}

pub struct EditBot {
    pub title: Entity<InputState>,
    pub description: Entity<InputState>,
    pub color: Option<u32>,
}

pub struct InfoState {
    pub key: BotKey,
    pub describe: Option<Result<Value, String>>,
    pub threads: Option<Result<Vec<ThreadRow>, String>>,
    pub edit: Option<EditBot>,
}

pub struct HermesApp {
    pub svc: Rc<Service>,
    pub config: Config,
    pub pal: Palette,
    pub bots: Vec<Bot>,
    pub unread: HashSet<BotKey>,
    pub links: HashMap<String, LinkState>,
    pub selected: Option<BotKey>,
    pub chats: HashMap<BotKey, Chat>,
    pub composer: Entity<TextareaState>,
    pub search: Entity<InputState>,
    pub searching: bool,
    pub pending: Vec<LocalFile>,
    pub mention: Option<MentionPopup>,
    pub show_hidden: bool,
    pub toast: Option<(SharedString, bool)>,
    toast_gen: u64,
    pub media: HashMap<String, PathBuf>,
    media_requested: HashSet<String>,
    pub approvals: Vec<Approval>,
    pub setup: Option<SetupForm>,
    pub new_bot: Option<NewBotForm>,
    pub settings: Option<SettingsState>,
    pub palette: Option<PaletteState>,
    pub apps: Option<AppsState>,
    pub info_open: bool,
    pub info: Option<InfoState>,
    pub thread_titles: HashMap<String, String>,
    pub window_active: bool,
    pub list: ListState,
    list_count: usize,
    pub dragging: bool,
    pub focus: FocusHandle,
    rpc_gen: u64,
    _subs: Vec<Subscription>,
}

/// Seed a connection from the environment (`HERMES_DESKTOP_URL` + `HERMES_DESKTOP_TOKEN`) for
/// development and testing. The token is never written to disk.
pub fn bootstrap_config(mut c: Config) -> Config {
    if let Ok(url) = std::env::var("HERMES_DESKTOP_URL") {
        if let Ok(t) = std::env::var("HERMES_DESKTOP_TOKEN") {
            std::env::set_var("HERMES_TOKEN_ENV", t);
        }
        c.connections.retain(|x| x.id != "env");
        c.connections.insert(0, Connection { id: "env".into(), name: std::env::var("HERMES_DESKTOP_NAME").unwrap_or_else(|_| "Hermes".into()), base_url: url, kind: ConnKind::Gateway });
    }
    c
}

pub fn accent_of(c: &Config) -> u32 {
    c.accent.as_deref().and_then(|a| u32::from_str_radix(a.trim_start_matches('#'), 16).ok()).unwrap_or(DEFAULT_ACCENT)
}

pub fn log_level(p: &Prefs) -> log::LevelFilter {
    if p.network_debug {
        return log::LevelFilter::Debug;
    }
    match p.log_level.as_str() {
        "error" => log::LevelFilter::Error,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        _ => log::LevelFilter::Warn,
    }
}

impl HermesApp {
    pub fn new(svc: Rc<Service>, config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pal = Palette::resolve(window.appearance(), &config.prefs, accent_of(&config));
        let enter = config.prefs.enter_to_send;
        let composer = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8).submit_on_enter(enter).placeholder("Message…"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search bots"));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift, secondary } => {
                let submit = *secondary || (this.config.prefs.enter_to_send && !*shift);
                if this.mention.is_some() && !*secondary {
                    this.complete_mention(window, cx);
                } else if submit {
                    this.send(window, cx);
                }
            }
            InputEvent::Change => this.update_mention(cx),
            _ => {}
        }));
        subs.push(cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        }));
        subs.push(cx.observe_window_appearance(window, |this, window, cx| this.apply_prefs(window, cx)));
        subs.push(cx.observe_window_activation(window, |this, window, cx| {
            this.window_active = window.is_window_active();
            cx.notify();
        }));

        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        let info_open = svc.store.get_kv("info_open").is_some_and(|v| v == "1");
        let mut this = HermesApp {
            svc,
            config,
            pal,
            bots: vec![],
            unread: HashSet::new(),
            links: HashMap::new(),
            selected: None,
            chats: HashMap::new(),
            composer,
            search,
            searching: false,
            pending: vec![],
            mention: None,
            show_hidden: false,
            toast: None,
            toast_gen: 0,
            media: HashMap::new(),
            media_requested: HashSet::new(),
            approvals: vec![],
            setup: None,
            new_bot: None,
            settings: None,
            palette: None,
            apps: None,
            info_open,
            info: None,
            thread_titles: HashMap::new(),
            window_active: true,
            list,
            list_count: 0,
            dragging: false,
            focus: cx.focus_handle(),
            rpc_gen: 0,
            _subs: subs,
        };
        this.apply_prefs(window, cx);
        // Paint the cached roster immediately, then connect.
        if let Some(last) = this.svc.store.get_kv("selected").and_then(|s| serde_json::from_str::<BotKey>(&s).ok()) {
            this.selected = Some(last.base());
        }
        if this.config.connections.is_empty() {
            this.open_setup(None, window, cx);
        }
        for c in this.config.connections.clone() {
            this.svc.send(Cmd::Connect(c));
        }
        if let Some(k) = this.selected.clone() {
            this.svc.send(Cmd::OpenBot(k));
        }
        this.pump_events(cx);
        if let Ok(script) = std::env::var("HERMES_DESKTOP_SCRIPT") {
            this.run_script(script, window, cx);
        }
        this
    }

    // ---- preferences ----------------------------------------------------------------------

    pub fn apply_prefs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pal = Palette::resolve(window.appearance(), &self.config.prefs, accent_of(&self.config));
        Theme::change(if self.pal.dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
        let fs = self.pal.fs;
        let theme = Theme::global_mut(cx);
        theme.font_size = px(fs);
        theme.mono_font_size = px((fs - 1.0).max(11.0));
        theme.accent = self.pal.accent.into();
        theme.ring = self.pal.accent.into();
        theme.primary = self.pal.accent.into();
        theme.caret = self.pal.accent.into();
        let enter = self.config.prefs.enter_to_send;
        self.composer.update(cx, |s, cx| s.set_submit_on_enter(enter, cx));
        log::set_max_level(log_level(&self.config.prefs));
        self.list_count = usize::MAX; // remeasure: font size may have changed
        self.sync_list();
        cx.notify();
    }

    pub fn set_pref(&mut self, f: impl FnOnce(&mut Prefs), window: &mut Window, cx: &mut Context<Self>) {
        f(&mut self.config.prefs);
        self.save_config(cx);
        self.apply_prefs(window, cx);
    }

    pub fn set_accent(&mut self, accent: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.config.accent = Some(format!("#{accent:06x}"));
        self.save_config(cx);
        self.apply_prefs(window, cx);
    }

    pub fn save_config(&mut self, cx: &mut Context<Self>) {
        // The env-seeded dev connection is never persisted.
        let mut c = self.config.clone();
        c.connections.retain(|x| x.id != "env");
        if let Err(e) = c.save() {
            self.show_toast(format!("Couldn't save settings: {e}"), true, cx);
        }
    }

    // ---- scripted demo driver ---------------------------------------------------------------

    /// Development aid for reproducible demos and screenshots: `;`-separated steps such as
    /// `wait:1500;select:researcher;type:hi @sc;send`. It drives the same code paths as the mouse
    /// and keyboard.
    fn run_script(&mut self, script: String, window: &mut Window, cx: &mut Context<Self>) {
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            for step in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
                let (cmd, arg) = step.split_once(':').unwrap_or((step, ""));
                if cmd == "wait" {
                    cx.background_executor().timer(Duration::from_millis(arg.parse().unwrap_or(500))).await;
                    continue;
                }
                let (cmd, arg) = (cmd.to_owned(), arg.to_owned());
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = this.update(cx, |app, cx| app.script_step(&cmd, &arg, window, cx));
                });
            }
        })
        .detach();
    }

    fn script_step(&mut self, cmd: &str, arg: &str, window: &mut Window, cx: &mut Context<Self>) {
        // Demos run unattended: keep the window frontmost so it keeps painting.
        cx.activate(true);
        window.activate_window();
        window.refresh();
        match cmd {
            "select" => {
                if let Some(k) = self.bots.iter().find(|b| b.name == arg).map(|b| b.key.clone()) {
                    self.select(k, window, cx);
                }
            }
            "type" => {
                let arg = arg.to_owned();
                let end = arg.len();
                self.composer.update(cx, |s, cx| {
                    s.set_value(arg, window, cx);
                    s.set_selected_range(end..end, cx);
                    s.focus(window, cx);
                });
                self.update_mention(cx);
            }
            "send" => self.send(window, cx),
            "size" => {
                if let Some((w, h)) = arg.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?))) {
                    window.resize(size(px(w), px(h)));
                }
            }
            "attach" => self.add_paths(&[PathBuf::from(arg)], cx),
            "share" => self.share(cx),
            "hidden" => {
                self.show_hidden = !self.show_hidden;
                cx.notify();
            }
            "newbot" => self.open_new_bot(window, cx),
            "setup" => self.open_setup(self.config.connections.first().map(|c| c.id.clone()), window, cx),
            "palette" => self.open_palette(window, cx),
            "query" => {
                let arg = arg.to_owned();
                if let Some(p) = &self.palette {
                    p.input.update(cx, |s, cx| s.set_value(arg.clone(), window, cx));
                } else if let Some(a) = &self.apps {
                    a.search.update(cx, |s, cx| s.set_value(arg.clone(), window, cx));
                } else {
                    self.searching = true;
                    self.search.update(cx, |s, cx| s.set_value(arg.clone(), window, cx));
                }
                cx.notify();
            }
            "settings" => {
                self.open_settings(window, cx);
                if let Some(s) = &mut self.settings {
                    s.section = Section::parse(arg);
                }
            }
            "apps" => self.open_apps(window, cx),
            "category" => {
                if let Some(a) = &mut self.apps {
                    a.category = arg.to_owned();
                }
            }
            "expand" => {
                if let Some(it) = self.apps.as_ref().and_then(|a| a.snapshot().items.into_iter().find(|i| i.id == arg)) {
                    self.app_manage(&it, cx);
                }
            }
            "info" => self.toggle_info(window, cx),
            "edit" => self.start_edit(window, cx),
            "thread" => {
                let row = self.info.as_ref().and_then(|i| i.threads.as_ref()).and_then(|t| t.as_ref().ok()).and_then(|t| t.iter().find(|r| r.id == arg).cloned());
                if let Some(r) = row {
                    self.open_thread(&r, window, cx);
                }
            }
            "theme" => self.set_pref(
                |p| {
                    p.theme = match arg {
                        "light" => hermes_core::conn::ThemePref::Light,
                        "dark" => hermes_core::conn::ThemePref::Dark,
                        _ => hermes_core::conn::ThemePref::System,
                    }
                },
                window,
                cx,
            ),
            "close" => self.close_overlays(cx),
            _ => {}
        }
        cx.notify();
    }

    /// Drain service events on the UI thread. Events that arrive together are applied in one
    /// batch and cause one repaint, which keeps CPU low while streaming.
    fn pump_events(&mut self, cx: &mut Context<Self>) {
        let rx = self.svc.ev.clone();
        cx.spawn(async move |this, cx| {
            while let Ok(first) = rx.recv().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                if this
                    .update(cx, |app, cx| {
                        for ev in batch {
                            app.on_event(ev, cx);
                        }
                        app.sync_list();
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    // ---- roster ---------------------------------------------------------------------------

    pub fn visible_bots(&self, cx: &App) -> Vec<&Bot> {
        let q = self.search.read(cx).value().to_lowercase();
        let q = if self.searching { q } else { String::new() };
        self.bots
            .iter()
            .filter(|b| self.show_hidden || !b.hidden() || self.selected.as_ref().is_some_and(|s| s.same_bot(&b.key)))
            .filter(|b| q.is_empty() || b.title().to_lowercase().contains(&q) || b.name.contains(&q) || b.preview().to_lowercase().contains(&q))
            .collect()
    }

    pub fn bot(&self, key: &BotKey) -> Option<&Bot> {
        self.bots.iter().find(|b| b.key.same_bot(key))
    }

    pub fn current_bot(&self) -> Option<&Bot> {
        self.selected.as_ref().and_then(|k| self.bot(k))
    }

    pub fn current_chat(&self) -> Option<&Chat> {
        self.selected.as_ref().and_then(|k| self.chats.get(k))
    }

    pub fn conn_name(&self, conn: &str) -> String {
        self.config.connections.iter().find(|c| c.id == conn).map(|c| c.name.clone()).unwrap_or_else(|| conn.to_owned())
    }

    pub fn thread_title(&self, key: &BotKey) -> Option<String> {
        key.thread.as_ref().map(|t| self.thread_titles.get(t).cloned().filter(|s| !s.is_empty()).unwrap_or_else(|| "Untitled thread".into()))
    }

    pub fn select(&mut self, key: BotKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&key) {
            let bot_changed = !self.selected.as_ref().is_some_and(|s| s.same_bot(&key));
            self.selected = Some(key.clone());
            self.mention = None;
            if key.thread.is_none() {
                let _ = self.svc.store.set_kv("selected", &serde_json::to_string(&key).unwrap_or_default());
            }
            self.list_count = usize::MAX; // force a reset
            self.sync_list();
            if bot_changed && self.info_open {
                self.load_info(cx);
            }
        }
        self.unread.remove(&key.base());
        self.svc.send(Cmd::MarkRead(key.base()));
        self.svc.send(Cmd::OpenBot(key));
        self.composer.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub fn step_bot(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let rows: Vec<BotKey> = self.visible_bots(cx).iter().map(|b| b.key.clone()).collect();
        if rows.is_empty() {
            return;
        }
        let cur = self.selected.as_ref().and_then(|s| rows.iter().position(|k| k.same_bot(s))).unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(rows.len() as i32) as usize;
        self.select(rows[next].clone(), window, cx);
    }

    pub fn select_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(k) = self.visible_bots(cx).get(n).map(|b| b.key.clone()) {
            self.select(k, window, cx);
        }
    }

    /// Item count of the message list for the selected chat (messages plus a footer).
    fn item_count(&self) -> usize {
        self.current_chat().map(|c| c.msgs.len()).unwrap_or(0) + 1
    }

    pub fn sync_list(&mut self) {
        let n = self.item_count();
        if n != self.list_count {
            if self.list_count == usize::MAX || n < self.list_count {
                self.list.reset(n);
                self.list.scroll_to_end();
            } else {
                let old = self.list_count;
                self.list.splice(old.saturating_sub(1)..old, n - old + 1);
            }
            self.list_count = n;
        } else if n > 0 {
            // Streaming grows the last message and the footer: remeasure them.
            let from = n.saturating_sub(2);
            self.list.splice(from..n, n - from);
        }
    }

    fn chat_mut(&mut self, key: &BotKey) -> &mut Chat {
        self.chats.entry(key.clone()).or_default()
    }

    fn streaming_msg(chat: &mut Chat) -> &mut MsgVm {
        if !chat.msgs.last().is_some_and(|m| m.m.streaming) {
            let mut m = ChatMessage::new(format!("live-{}", uuid::Uuid::new_v4()), Role::Assistant, "", Some(hermes_core::service::now()));
            m.streaming = true;
            chat.msgs.push(MsgVm { m, md: None });
        }
        chat.msgs.last_mut().unwrap()
    }

    fn on_event(&mut self, ev: Ev, cx: &mut Context<Self>) {
        match ev {
            Ev::Link { conn, state } => {
                if state == LinkState::Unauthorized {
                    self.show_toast(format!("{} refused the session token. Check it in Settings.", self.conn_name(&conn)), true, cx);
                }
                // The info pane may have been open at launch, before this server answered.
                let reload = state == LinkState::Connected && self.info_open && self.selected.as_ref().is_some_and(|k| k.conn == conn);
                self.links.insert(conn, state);
                if reload {
                    self.load_info(cx);
                }
            }
            Ev::Roster { conn, bots, unread } => {
                self.bots.retain(|b| b.key.conn != conn);
                self.bots.extend(bots);
                roster::sort_roster(&mut self.bots);
                self.unread.retain(|k| k.conn != conn);
                let sel = self.selected.clone();
                self.unread.extend(unread.into_iter().filter(|k| !sel.as_ref().is_some_and(|s| s.same_bot(k))));
                if self.selected.as_ref().is_none_or(|k| !self.bots.iter().any(|b| b.key.same_bot(k))) {
                    if let Some(first) = self.bots.iter().find(|b| !b.hidden()).map(|b| b.key.clone()) {
                        self.selected = Some(first.clone());
                        self.list_count = usize::MAX;
                        self.svc.send(Cmd::MarkRead(first.clone()));
                        self.svc.send(Cmd::OpenBot(first));
                    }
                }
            }
            Ev::History { key, messages, cached } => {
                let chat = self.chat_mut(&key);
                if chat.streaming || chat.awaiting || (cached && chat.loaded) {
                    return;
                }
                chat.msgs = messages.into_iter().map(|m| MsgVm { m, md: None }).collect();
                chat.loaded = !cached;
                chat.error = None;
                if Some(&key) == self.selected.as_ref() {
                    self.list_count = usize::MAX;
                }
            }
            Ev::Incoming { key, message } => {
                let chat = self.chat_mut(&key);
                let dup = chat.msgs.iter().rev().take(3).any(|m| m.m.text.trim() == message.text.trim());
                if !dup {
                    chat.msgs.push(MsgVm { m: message, md: None });
                }
            }
            Ev::TurnStart { key } => {
                let chat = self.chat_mut(&key);
                chat.awaiting = false;
                chat.streaming = true;
                chat.error = None;
                chat.status = None;
                Self::streaming_msg(chat);
            }
            Ev::Delta { key, text } => {
                let chat = self.chat_mut(&key);
                chat.awaiting = false;
                chat.streaming = true;
                let vm = Self::streaming_msg(chat);
                vm.m.text.push_str(&text);
                if let Some(md) = &vm.md {
                    md.update(cx, |s, cx| s.push_str(&text, cx));
                }
            }
            Ev::Activity { key, activity } => {
                let chat = self.chat_mut(&key);
                chat.awaiting = false;
                let vm = Self::streaming_msg(chat);
                upsert(&mut vm.m.activity, activity);
            }
            Ev::Status { key, text } => self.chat_mut(&key).status = Some(text),
            Ev::TurnEnd { key, text, error } => {
                let chat = self.chat_mut(&key);
                chat.awaiting = false;
                chat.streaming = false;
                chat.status = None;
                if let Some(last) = chat.msgs.last_mut().filter(|m| m.m.streaming) {
                    last.m.streaming = false;
                    let (clean, atts) = transcript::extract_media(if text.is_empty() { &last.m.text } else { &text });
                    if clean != last.m.text {
                        last.m.text = clean.clone();
                        if let Some(md) = &last.md {
                            md.update(cx, |s, cx| s.set_text(&clean, cx));
                        }
                    }
                    last.m.attachments.extend(atts);
                    for a in &mut last.m.activity {
                        if a.state == transcript::ToolState::Running {
                            a.state = transcript::ToolState::Done;
                        }
                    }
                    if transcript::is_silent(&last.m.text) || (last.m.text.trim().is_empty() && last.m.activity.is_empty() && last.m.attachments.is_empty()) {
                        chat.msgs.pop();
                    }
                }
                chat.error = error;
                if Some(&key) == self.selected.as_ref() {
                    self.svc.send(Cmd::MarkRead(key.base()));
                }
            }
            Ev::Approval { conn, key, request_id, command, description, choices } => {
                self.approvals.push(Approval { conn, key, request_id, command, description, choices });
            }
            Ev::ApprovalGone { request_id } => self.approvals.retain(|a| a.request_id != request_id),
            Ev::MediaReady { remote, local } => {
                self.media.insert(remote, local);
            }
            Ev::Downloaded { path, open } => {
                if open {
                    cx.open_with_system(&path);
                } else {
                    self.show_toast(format!("Saved {}", path.display()), false, cx);
                    cx.reveal_path(&path);
                }
            }
            Ev::Notify { key, title, body } => {
                if Some(&key) != self.selected.as_ref() {
                    self.unread.insert(key.base());
                    let p = &self.config.prefs;
                    if p.notifications && (!p.notify_only_unfocused || !self.window_active || key.thread.is_none()) {
                        crate::view::notify_os(&title, if p.hide_previews { "New message" } else { &body });
                    }
                    if p.sounds {
                        crate::view::play_sound();
                    }
                }
            }
            Ev::Error { key, message } => match key.and_then(|k| self.chats.get_mut(&k)) {
                Some(chat) if !message.is_empty() => {
                    chat.error = Some(message);
                    chat.awaiting = false;
                }
                _ => self.show_toast(message, true, cx),
            },
            Ev::Info { message } => self.show_toast(message, false, cx),
            Ev::Rpc { tag, result, .. } => self.on_rpc(&tag, result, cx),
        }
    }

    pub fn show_toast(&mut self, msg: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.toast = Some((msg.into(), error));
        self.toast_gen += 1;
        let gen = self.toast_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |app, cx| {
                if app.toast_gen == gen {
                    app.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Make sure a remote image is being fetched for inline display.
    pub fn want_media(&mut self, key: &BotKey, a: &Attachment) {
        if let Some(r) = &a.remote_path {
            if !self.media.contains_key(r) && self.media_requested.insert(r.clone()) {
                self.svc.send(Cmd::FetchMedia { key: key.clone(), remote: r.clone() });
            }
        }
    }

    // ---- composer ---------------------------------------------------------------------------

    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.selected.clone() else { return };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() && self.pending.is_empty() {
            return;
        }
        let files = std::mem::take(&mut self.pending);
        let mut m = ChatMessage::new(format!("local-{}", uuid::Uuid::new_v4()), Role::User, text.clone(), Some(hermes_core::service::now()));
        m.attachments = files
            .iter()
            .map(|f| Attachment { name: f.name.clone(), mime: f.mime.clone(), kind: transcript::kind_of(&f.mime), remote_path: None, local_path: Some(f.path.display().to_string()), size: Some(f.size) })
            .collect();
        let is_command = key.thread.is_none() && roster::rewrite_bot_chat_command(&text).is_some() && files.is_empty();
        let chat = self.chat_mut(&key);
        if !is_command {
            chat.msgs.push(MsgVm { m, md: None });
            chat.awaiting = true;
        }
        chat.error = None;
        self.svc.send(Cmd::Send { key, text, files });
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.mention = None;
        self.sync_list();
        self.list.scroll_to_end();
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(k) = self.selected.clone() {
            self.svc.send(Cmd::Stop(k));
        }
        cx.notify();
    }

    pub fn add_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        for p in paths {
            match LocalFile::from_path(p.clone()) {
                Some(f) if f.size > MAX_FILE_BYTES => self.show_toast(format!("{} is over Hermes' 256 MB limit.", f.name), true, cx),
                Some(f) => {
                    if !self.pending.iter().any(|x| x.path == f.path) {
                        self.pending.push(f);
                    }
                }
                None => self.show_toast(format!("Can't attach {}: only files are supported.", p.display()), true, cx),
            }
        }
        cx.notify();
    }

    /// Pasted image bytes are written to a temp file and attached like any other file.
    pub fn add_image_bytes(&mut self, bytes: &[u8], ext: &str, cx: &mut Context<Self>) {
        let dir = hermes_core::conn::data_dir().join("cache").join("pasted");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(format!("pasted-{}.{ext}", &uuid::Uuid::new_v4().to_string()[..8]));
        if std::fs::write(&p, bytes).is_ok() {
            self.add_paths(&[p], cx);
        }
    }

    pub fn pick_files(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                let _ = this.update(cx, |app, cx| app.add_paths(&paths, cx));
            }
        })
        .detach();
    }

    fn download_dir(&self) -> PathBuf {
        self.config
            .prefs
            .download_dir
            .clone()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .or_else(|| directories::UserDirs::new().and_then(|u| u.download_dir().map(|p| p.to_path_buf())))
            .unwrap_or_else(std::env::temp_dir)
    }

    pub fn save_attachment(&mut self, key: BotKey, a: &Attachment, cx: &mut Context<Self>) {
        let Some(remote) = a.remote_path.clone() else {
            if let Some(l) = &a.local_path {
                cx.reveal_path(std::path::Path::new(l));
            }
            return;
        };
        let rx = cx.prompt_for_new_path(&self.download_dir(), Some(&a.name));
        let svc = self.svc.clone();
        cx.spawn(async move |_, _| {
            if let Ok(Ok(Some(path))) = rx.await {
                let dest = path.parent().map(|p| p.to_path_buf());
                svc.send(Cmd::Download { key, remote, dest, open: false });
            }
        })
        .detach();
    }

    pub fn open_attachment(&mut self, key: BotKey, a: &Attachment, cx: &mut Context<Self>) {
        if let Some(l) = &a.local_path {
            cx.open_with_system(std::path::Path::new(l));
        } else if let Some(local) = a.remote_path.as_ref().and_then(|r| self.media.get(r)) {
            cx.open_with_system(local);
        } else if let Some(remote) = a.remote_path.clone() {
            self.svc.send(Cmd::Download { key, remote, dest: Some(hermes_core::conn::data_dir().join("cache").join("downloads")), open: true });
        }
    }

    pub fn share(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected.clone() else { return };
        let title = self.thread_title(&key).or_else(|| self.bot(&key).map(|b| b.title())).unwrap_or_default();
        let msgs: Vec<ChatMessage> = self.chats.get(&key).map(|c| c.msgs.iter().map(|m| m.m.clone()).collect()).unwrap_or_default();
        cx.write_to_clipboard(ClipboardItem::new_string(transcript::to_markdown(&title, &msgs)));
        self.show_toast("Conversation copied to the clipboard as Markdown", false, cx);
    }

    pub fn toggle_hidden(&mut self, key: BotKey, cx: &mut Context<Self>) {
        let hidden = self.bot(&key).map(|b| b.hidden()).unwrap_or(false);
        // Stored in the bot's profile metadata, so every desktop on that backend follows it.
        self.svc.send(Cmd::SetMeta { key: key.base(), patch: json!({"hidden": if hidden { Value::Null } else { Value::Bool(true) }}) });
        self.show_toast(if hidden { "Bot unhidden" } else { "Bot hidden (still reachable by @mention)" }, false, cx);
    }

    pub fn approve(&mut self, request_id: &str, choice: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.approvals.iter().position(|a| a.request_id == request_id) {
            let a = self.approvals.remove(i);
            self.svc.send(Cmd::Approve { conn: a.conn, request_id: a.request_id, choice: choice.to_owned() });
        }
        cx.notify();
    }

    // ---- @mention autocomplete -------------------------------------------------------------

    pub fn update_mention(&mut self, cx: &mut Context<Self>) {
        let st = self.composer.read(cx);
        let text = st.value().to_string();
        let cursor = st.cursor().min(text.len());
        self.mention = None;
        let before = &text[..cursor];
        if let Some(at) = before.rfind('@') {
            let q = &before[at + 1..];
            let ok_start = at == 0 || before[..at].ends_with(char::is_whitespace);
            if ok_start && !q.contains(char::is_whitespace) && q.len() <= 40 {
                let items: Vec<BotKey> = roster::complete_mention(q, &self.bots).into_iter().take(6).map(|b| b.key.clone()).collect();
                if !items.is_empty() {
                    self.mention = Some(MentionPopup { at, cursor, items, sel: 0 });
                }
            }
        }
        cx.notify();
    }

    pub fn move_mention(&mut self, delta: i32, cx: &mut Context<Self>) {
        if let Some(m) = &mut self.mention {
            let n = m.items.len() as i32;
            m.sel = ((m.sel as i32 + delta).rem_euclid(n)) as usize;
            cx.notify();
        }
    }

    /// The handle to insert for a bot: connection-qualified when it lives elsewhere and the tag
    /// is ambiguous.
    pub fn tag_for(&self, key: &BotKey) -> String {
        let Some(b) = self.bot(key) else { return key.profile.clone() };
        let h = b.handle();
        let clash = self.bots.iter().filter(|o| o.handle() == h).count() > 1;
        let home = self.selected.as_ref().map(|s| s.conn.as_str()) == Some(key.conn.as_str());
        if clash && !home {
            format!("{h}@{}", roster::slug(&b.conn_name, true))
        } else {
            h
        }
    }

    pub fn complete_mention(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.mention.take() else { return };
        let Some(key) = m.items.get(m.sel).cloned() else { return };
        self.pick_mention(m.at, m.cursor, key, window, cx);
    }

    pub fn pick_mention(&mut self, at: usize, cursor: usize, key: BotKey, window: &mut Window, cx: &mut Context<Self>) {
        let tag = self.tag_for(&key);
        let text = self.composer.read(cx).value().to_string();
        let cursor = cursor.min(text.len());
        let new_text = format!("{}@{} {}", &text[..at], tag, &text[cursor..]);
        let new_cursor = at + tag.len() + 2;
        self.mention = None;
        self.composer.update(cx, |s, cx| {
            s.set_value(new_text.clone(), window, cx);
            let line = new_text[..new_cursor].matches('\n').count() as u32;
            let col = new_text[..new_cursor].rsplit('\n').next().map(|l| l.encode_utf16().count()).unwrap_or(0) as u32;
            s.set_cursor_position(gpui_component::input::Position { line, character: col }, window, cx);
        });
        cx.notify();
    }

    // ---- overlays -------------------------------------------------------------------------

    pub fn any_overlay(&self) -> bool {
        self.palette.is_some() || self.settings.is_some() || self.apps.is_some() || self.setup.is_some() || self.new_bot.is_some()
    }

    /// Close the top-most overlay (Escape). Returns whether anything closed.
    pub fn close_top(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let closed = if self.setup.is_some() && !self.config.connections.is_empty() {
            self.setup = None;
            true
        } else if self.new_bot.is_some() {
            self.new_bot = None;
            true
        } else if self.palette.is_some() {
            self.palette = None;
            true
        } else if self.apps.is_some() {
            if self.apps.as_ref().is_some_and(|a| a.expanded.is_some()) {
                self.apps.as_mut().unwrap().expanded = None;
            } else {
                self.apps = None;
            }
            true
        } else if self.settings.is_some() {
            self.settings = None;
            true
        } else if self.info.as_ref().is_some_and(|i| i.edit.is_some()) {
            self.info.as_mut().unwrap().edit = None;
            true
        } else if self.mention.is_some() {
            self.mention = None;
            true
        } else {
            false
        };
        if closed {
            if !self.any_overlay() {
                self.composer.update(cx, |s, cx| s.focus(window, cx));
            }
            cx.notify();
        }
        closed
    }

    pub fn close_overlays(&mut self, cx: &mut Context<Self>) {
        self.palette = None;
        self.apps = None;
        self.settings = None;
        self.new_bot = None;
        if !self.config.connections.is_empty() {
            self.setup = None;
        }
        cx.notify();
    }

    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        let user = self.config.user_name.clone().unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Your name").default_value(user));
        self.settings = Some(SettingsState { section: Section::Account, name });
        cx.notify();
    }

    pub fn save_user_name(&mut self, cx: &mut Context<Self>) {
        let Some(s) = &self.settings else { return };
        let v = s.name.read(cx).value().trim().to_owned();
        self.config.user_name = (!v.is_empty()).then_some(v);
        self.save_config(cx);
        self.show_toast("Name saved", false, cx);
    }

    pub fn remove_connection(&mut self, id: &str, cx: &mut Context<Self>) {
        self.config.connections.retain(|c| c.id != id);
        Secrets::delete(id);
        self.svc.send(Cmd::Disconnect(id.to_owned()));
        self.bots.retain(|b| b.key.conn != id);
        if self.selected.as_ref().is_some_and(|k| k.conn == id) {
            self.selected = None;
        }
        self.save_config(cx);
        cx.notify();
    }

    pub fn clear_cache(&mut self, cx: &mut Context<Self>) {
        let r = self.svc.store.clear_cache();
        let media = hermes_core::conn::data_dir().join("cache");
        let _ = std::fs::remove_dir_all(media.join("media"));
        let _ = std::fs::remove_dir_all(media.join("pasted"));
        let _ = std::fs::remove_dir_all(media.join("downloads"));
        self.media.clear();
        self.media_requested.clear();
        match r {
            Ok(()) => self.show_toast("Cache cleared. Conversations reload from your Hermes.", false, cx),
            Err(e) => self.show_toast(format!("Couldn't clear the cache: {e}"), true, cx),
        }
    }

    pub fn pick_download_dir(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(p) = paths.into_iter().next() {
                    let _ = this.update(cx, |app, cx| {
                        app.config.prefs.download_dir = Some(p.display().to_string());
                        app.save_config(cx);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    // ---- command palette ---------------------------------------------------------------------

    pub fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.palette = None;
            cx.notify();
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search bots, threads and actions…"));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe(&input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                if let Some(p) = &mut this.palette {
                    p.sel = 0;
                }
                cx.notify();
            }
        });
        self._subs.push(sub);
        self.palette = Some(PaletteState { input, sel: 0 });
        if self.info.as_ref().is_none_or(|i| !self.selected.as_ref().is_some_and(|s| s.same_bot(&i.key))) {
            self.load_info(cx);
        }
        cx.notify();
    }

    pub fn palette_items(&self, cx: &App) -> Vec<PaletteItem> {
        let q = self.palette.as_ref().map(|p| p.input.read(cx).value().trim().to_lowercase()).unwrap_or_default();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        let mut out = vec![];
        for b in &self.bots {
            if (self.show_hidden || !b.hidden()) && (hit(&b.title()) || hit(&b.name)) {
                let sub = if self.config.prefs.hide_previews { format!("@{}", b.handle()) } else { b.preview() };
                out.push(PaletteItem { target: PaletteTarget::Bot(b.key.clone()), title: b.title(), subtitle: sub, keys: None });
            }
        }
        if let Some(info) = &self.info {
            if let Some(Ok(rows)) = &info.threads {
                for r in rows.iter().take(8) {
                    if hit(&r.title) || hit(&r.preview) {
                        out.push(PaletteItem { target: PaletteTarget::Thread(info.key.clone(), r.id.clone()), title: r.title.clone(), subtitle: format!("Thread · {}", r.source), keys: None });
                    }
                }
            }
        }
        let theme = match self.config.prefs.theme {
            hermes_core::conn::ThemePref::System => "Switch to dark theme",
            hermes_core::conn::ThemePref::Dark => "Switch to light theme",
            hermes_core::conn::ThemePref::Light => "Follow the system theme",
        };
        let acts: [(Act, &str, Option<&'static str>); 10] = [
            (Act::NewBot, "New bot", shortcut_for("New bot")),
            (Act::Apps, "Apps & connectors", shortcut_for("Apps & connectors")),
            (Act::Info, "Bot info & threads", shortcut_for("Bot info & threads")),
            (Act::Settings, "Settings", shortcut_for("Settings")),
            (Act::Share, "Copy conversation as Markdown", shortcut_for("Copy conversation as Markdown")),
            (Act::BotChat, "Back to the Bot Chat", None),
            (Act::Compact, "Compact the Bot Chat (/compact)", None),
            (Act::Theme, theme, None),
            (Act::Hidden, if self.show_hidden { "Hide hidden bots" } else { "Show hidden bots" }, None),
            (Act::AddServer, "Add a Hermes server", None),
        ];
        for (a, title, keys) in acts {
            if a == Act::BotChat && !self.selected.as_ref().is_some_and(|k| k.thread.is_some()) {
                continue;
            }
            if hit(title) {
                out.push(PaletteItem { target: PaletteTarget::Action(a), title: title.to_owned(), subtitle: String::new(), keys });
            }
        }
        out
    }

    pub fn run_palette(&mut self, item: PaletteItem, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        match item.target {
            PaletteTarget::Bot(k) => self.select(k, window, cx),
            PaletteTarget::Thread(k, id) => {
                self.thread_titles.entry(id.clone()).or_insert(item.title);
                self.select(k.with_thread(Some(id)), window, cx);
            }
            PaletteTarget::Action(a) => self.run_act(a, window, cx),
        }
        cx.notify();
    }

    pub fn run_act(&mut self, a: Act, window: &mut Window, cx: &mut Context<Self>) {
        match a {
            Act::NewBot => self.open_new_bot(window, cx),
            Act::Settings => self.open_settings(window, cx),
            Act::Apps => self.open_apps(window, cx),
            Act::Info => self.toggle_info(window, cx),
            Act::Share => self.share(cx),
            Act::Theme => self.set_pref(
                |p| {
                    p.theme = match p.theme {
                        hermes_core::conn::ThemePref::System => hermes_core::conn::ThemePref::Dark,
                        hermes_core::conn::ThemePref::Dark => hermes_core::conn::ThemePref::Light,
                        hermes_core::conn::ThemePref::Light => hermes_core::conn::ThemePref::System,
                    }
                },
                window,
                cx,
            ),
            Act::Hidden => self.show_hidden = !self.show_hidden,
            Act::Compact => {
                if let Some(k) = self.selected.clone() {
                    self.svc.send(Cmd::Send { key: k.base(), text: "/compact".into(), files: vec![] });
                }
            }
            Act::AddServer => self.open_setup(None, window, cx),
            Act::BotChat => {
                if let Some(k) = self.selected.clone() {
                    self.select(k.base(), window, cx);
                }
            }
        }
        cx.notify();
    }

    // ---- bot info, threads, editing ------------------------------------------------------------

    fn rpc(&mut self, conn: &str, method: &str, params: Value, tag: String) {
        self.svc.send(Cmd::Rpc { conn: conn.to_owned(), method: method.to_owned(), params, tag });
    }

    pub fn toggle_info(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.info_open = !self.info_open;
        let _ = self.svc.store.set_kv("info_open", if self.info_open { "1" } else { "0" });
        if self.info_open {
            self.load_info(cx);
        }
        cx.notify();
    }

    pub fn load_info(&mut self, cx: &mut Context<Self>) {
        let Some(key) = self.selected.as_ref().map(|k| k.base()) else { return };
        let keep = self.info.as_ref().is_some_and(|i| i.key == key);
        if !keep {
            self.info = Some(InfoState { key: key.clone(), describe: None, threads: None, edit: None });
        }
        self.rpc(&key.conn, "profiles.describe", json!({"name": key.profile, "profile": key.profile}), format!("describe:{}:{}", key.conn, key.profile));
        self.rpc(&key.conn, "session.list", json!({"profile": key.profile, "limit": 30}), format!("threads:{}:{}", key.conn, key.profile));
        cx.notify();
    }

    pub fn open_thread(&mut self, row: &ThreadRow, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else { return };
        let key = info.key.with_thread(Some(row.id.clone()));
        self.thread_titles.insert(row.id.clone(), row.title.clone());
        self.select(key, window, cx);
    }

    pub fn start_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.current_bot().cloned() else { return };
        if !self.info_open {
            self.toggle_info(window, cx);
        }
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Display name").default_value(b.title()));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("What is this bot good at?").default_value(b.description.clone().unwrap_or_default()));
        let color = b.color_hex();
        if let Some(i) = &mut self.info {
            i.edit = Some(EditBot { title, description, color });
        }
        cx.notify();
    }

    pub fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(info) = &mut self.info else { return };
        let Some(e) = info.edit.take() else { return };
        let key = info.key.clone();
        let title = e.title.read(cx).value().trim().to_owned();
        let description = e.description.read(cx).value().trim().to_owned();
        let old = self.bot(&key).cloned();
        let mut patch = serde_json::Map::new();
        if old.as_ref().map(|b| b.title()) != Some(title.clone()) {
            patch.insert("title".into(), if title.is_empty() { Value::Null } else { json!(title) });
        }
        if old.as_ref().and_then(|b| b.color_hex()) != e.color {
            patch.insert("color".into(), e.color.map(|c| json!(format!("#{c:06x}"))).unwrap_or(Value::Null));
        }
        if !patch.is_empty() {
            self.svc.send(Cmd::SetMeta { key: key.clone(), patch: Value::Object(patch) });
        }
        if old.as_ref().and_then(|b| b.description.clone()).unwrap_or_default() != description {
            self.rpc(&key.conn, "profiles.configure", json!({"name": key.profile, "description": description}), format!("edit:{}:{}", key.conn, key.profile));
        }
        self.show_toast("Bot updated", false, cx);
        cx.notify();
    }

    // ---- apps ---------------------------------------------------------------------------------

    pub fn open_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        let Some(key) = self.selected.as_ref().map(|k| k.base()) else {
            self.show_toast("Pick a bot first: apps are configured per bot (Hermes profile).", false, cx);
            return;
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search apps"));
        search.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        self._subs.push(sub);
        self.rpc_gen += 1;
        self.apps = Some(AppsState { key, search, category: String::new(), gen: self.rpc_gen, raw: HashMap::new(), busy: HashSet::new(), expanded: None, tools: HashMap::new() });
        self.reload_apps(cx);
    }

    fn reload_apps(&mut self, cx: &mut Context<Self>) {
        let Some(a) = &self.apps else { return };
        let (key, gen) = (a.key.clone(), a.gen);
        let p = &key.profile;
        for (method, slot) in APP_SOURCES.iter().filter(|(m, _)| !m.is_empty()) {
            let params = match *slot {
                "list" => apps::connectors_list_params(p),
                "skills" => apps::skills_list_params(p),
                _ => apps::catalog_params(p),
            };
            self.rpc(&key.conn, method, params, format!("apps:{gen}:{slot}"));
        }
        cx.notify();
    }

    pub fn app_action(&mut self, it: &AppItem, cx: &mut Context<Self>) {
        let Some(a) = &mut self.apps else { return };
        let key = a.key.clone();
        a.busy.insert(it.id.clone());
        let (method, params, what) = match it.kind {
            AppKind::Connector => ("connectors.connect", apps::connect_params(&key.profile, &it.id), "connect"),
            AppKind::Mcp if it.connected => ("mcp.servers.remove", apps::mcp_remove_params(&key.profile, &it.id), "remove"),
            AppKind::Mcp => ("mcp.servers.add", apps::mcp_add_params(&key.profile, &it.id), "install"),
            AppKind::Toolset => ("tools.configure", apps::toolset_params(&key.profile, &it.id, !it.connected), "toggle"),
            AppKind::Skill => return,
        };
        self.rpc(&key.conn, method, params, format!("apps-act:{}:{what}", it.id));
        cx.notify();
    }

    pub fn app_manage(&mut self, it: &AppItem, cx: &mut Context<Self>) {
        let Some(a) = &mut self.apps else { return };
        if a.expanded.as_deref() == Some(&it.id) {
            a.expanded = None;
        } else {
            a.expanded = Some(it.id.clone());
            if it.kind == AppKind::Connector && !a.tools.contains_key(&it.id) {
                let key = a.key.clone();
                self.rpc(&key.conn, "connectors.tools", json!({"profile": key.profile, "slug": it.id}), format!("tools:{}", it.id));
            }
        }
        cx.notify();
    }

    fn on_rpc(&mut self, tag: &str, result: Result<Value, String>, cx: &mut Context<Self>) {
        let mut parts = tag.splitn(3, ':');
        let (kind, a1, a2) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        match kind {
            "apps" => {
                if let Some(ap) = &mut self.apps {
                    if a1 == ap.gen.to_string() {
                        if let Some((_, slot)) = APP_SOURCES.iter().find(|(_, s)| *s == a2) {
                            ap.raw.insert(slot, result);
                        }
                    }
                }
            }
            "apps-act" => {
                if let Some(ap) = &mut self.apps {
                    ap.busy.remove(a1);
                }
                match result {
                    Ok(v) => {
                        if a2 == "connect" {
                            match apps::connect_url(&v) {
                                Some(url) => {
                                    cx.open_url(&url);
                                    self.show_toast("Finish connecting in your browser. Hermes picks it up automatically.", false, cx);
                                }
                                None => self.show_toast("Connection started. Hermes will finish it.", false, cx),
                            }
                        }
                        self.reload_apps(cx);
                    }
                    Err(e) => self.show_toast(e, true, cx),
                }
            }
            "tools" => {
                if let Some(ap) = &mut self.apps {
                    let tools = result.map(|v| {
                        v.get("tools")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .map(|t| {
                                        let g = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
                                        (g("name"), g("description"), g("facet"))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    });
                    ap.tools.insert(a1.to_owned(), tools);
                }
            }
            "describe" | "threads" => {
                let Some(info) = &mut self.info else { return };
                if info.key.conn != a1 || info.key.profile != a2 {
                    return;
                }
                if kind == "describe" {
                    info.describe = Some(result);
                } else {
                    let canonical = self.bots.iter().find(|b| b.key.same_bot(&info.key)).and_then(|b| b.canonical.as_ref().map(|c| c.id.clone()));
                    info.threads = Some(result.map(|v| {
                        v.get("sessions")
                            .and_then(Value::as_array)
                            .map(|rows| {
                                rows.iter()
                                    .filter_map(|r| {
                                        let id = r.get("id").or_else(|| r.get("session_id")).and_then(Value::as_str)?.to_owned();
                                        let title = r.get("title").and_then(Value::as_str).unwrap_or_default().to_owned();
                                        if Some(&id) == canonical.as_ref() || title == roster::BOT_CHAT_TITLE {
                                            return None;
                                        }
                                        let preview = r.get("preview").and_then(Value::as_str).map(|p| roster::one_line(&roster::strip_inline_md(p), 90)).unwrap_or_default();
                                        Some(ThreadRow {
                                            title: if title.is_empty() { if preview.is_empty() { "Untitled".into() } else { roster::one_line(&preview, 40) } } else { title },
                                            preview,
                                            source: r.get("source").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("hermes").to_owned(),
                                            last: r.get("last_active_at").or_else(|| r.get("last_active")).or_else(|| r.get("started_at")).and_then(Value::as_f64).unwrap_or(0.0),
                                            count: r.get("message_count").and_then(Value::as_u64).unwrap_or(0),
                                            id,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }));
                }
            }
            "edit" => {
                if let Err(e) = result {
                    self.show_toast(format!("Couldn't save the description: {e}"), true, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    // ---- connection setup and bot creation --------------------------------------------------

    pub fn open_setup(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = editing.as_ref().and_then(|id| self.config.connections.iter().find(|c| c.id == *id).cloned());
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Device name, e.g. Homelab").default_value(existing.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "Hermes".into())));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("http://my-hermes.tailnet.ts.net:9119").default_value(existing.as_ref().map(|c| c.base_url.clone()).unwrap_or_default()));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(if existing.is_some() { "Leave blank to keep the saved token" } else { "Dashboard session token" }));
        url.update(cx, |s, cx| s.focus(window, cx));
        self.setup = Some(SetupForm { name, url, token, editing, error: None });
        cx.notify();
    }

    pub fn save_setup(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = &self.setup else { return };
        let name = f.name.read(cx).value().trim().to_owned();
        let url = f.url.read(cx).value().trim().to_owned();
        let token = f.token.read(cx).value().trim().to_owned();
        let editing = f.editing.clone();
        if url.is_empty() || hermes_core::conn::Endpoint::new(&url, "").is_err() {
            if let Some(f) = &mut self.setup {
                f.error = Some("Enter the address you open the Hermes dashboard at.".into());
            }
            cx.notify();
            return;
        }
        let id = editing.clone().unwrap_or_else(|| format!("c{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        if !token.is_empty() {
            if let Err(e) = Secrets::set(&id, &token) {
                if let Some(f) = &mut self.setup {
                    f.error = Some(format!("Couldn't save the token in the keychain: {e}"));
                }
                cx.notify();
                return;
            }
        }
        let conn = Connection { id: id.clone(), name: if name.is_empty() { "Hermes".into() } else { name }, base_url: url, kind: ConnKind::Gateway };
        if let Some(i) = self.config.connections.iter().position(|c| c.id == id) {
            self.config.connections[i] = conn.clone();
        } else {
            self.config.connections.push(conn.clone());
        }
        self.save_config(cx);
        self.svc.send(Cmd::Connect(conn));
        self.setup = None;
        cx.notify();
    }

    pub fn open_new_bot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("researcher"));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Research Buddy"));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("Finds and summarises sources"));
        title.update(cx, |s, cx| s.focus(window, cx));
        self.new_bot = Some(NewBotForm { name, title, description });
        cx.notify();
    }

    pub fn create_bot(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.new_bot.take() else { return };
        let title = f.title.read(cx).value().trim().to_owned();
        let mut name = roster::slug(&f.name.read(cx).value(), true);
        if name.is_empty() {
            name = roster::slug(&title, true);
        }
        let description = f.description.read(cx).value().trim().to_owned();
        let conn = self.selected.as_ref().map(|k| k.conn.clone()).or_else(|| self.config.connections.first().map(|c| c.id.clone()));
        match conn {
            Some(conn) if !name.is_empty() => self.svc.send(Cmd::CreateBot { conn, name, title, description }),
            _ => self.show_toast("A bot needs a name.", true, cx),
        }
        cx.notify();
    }
}

fn upsert(list: &mut Vec<Activity>, a: Activity) {
    match list.iter_mut().find(|x| x.id == a.id && x.is_subagent == a.is_subagent && !a.id.is_empty()) {
        Some(x) => {
            x.state = a.state;
            if a.detail.is_some() {
                x.detail = a.detail;
            }
            if a.summary.is_some() {
                x.summary = a.summary;
            }
            if a.duration_s.is_some() {
                x.duration_s = a.duration_s;
            }
        }
        None => list.push(a),
    }
}
