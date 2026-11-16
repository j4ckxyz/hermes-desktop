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
use hermes_core::conn::{dev_env, ConnKind, Config, Connection, Prefs};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{self, Bot, BotKey};
use hermes_core::service::{Cmd, Ev, LocalFile, MAX_FILE_BYTES};
use hermes_core::transcript::{self, Activity, Attachment, ChatMessage, Role};
use hermes_core::Service;
use serde_json::{json, Value};

use crate::theme::{Palette, DEFAULT_ACCENT};

actions!(
    hermes,
    [
        Quit,
        OpenPalette,
        NewBot,
        OpenSettings,
        OpenApps,
        OpenUsage,
        ToggleInfo,
        ToggleSidebar,
        TogglePin,
        FocusSearch,
        FocusComposer,
        NextBot,
        PrevBot,
        CopyConversation,
        StopTurn,
        AttachFiles
    ]
);

/// Every shortcut, shown in Settings → Shortcuts and the palette: (bindings, description).
/// Text inputs already bind ⌘⌥↑/↓, ⌘F, ⌘[ and ⌘], so bot switching uses ⇧⌘[ / ⇧⌘] and ⌃Tab.
pub const SHORTCUTS: &[(&[&str], &str)] = &[
    (&["secondary-k"], "Command palette and search"),
    (&["secondary-n"], "New bot"),
    (&["secondary-1"], "Jump to bot 1–9 (hold ⌘ to see numbers)"),
    (&["secondary-shift-]", "ctrl-tab"], "Next bot"),
    (&["secondary-shift-[", "ctrl-shift-tab"], "Previous bot"),
    (&["secondary-shift-p"], "Pin or unpin bot"),
    (&["secondary-b"], "Show or hide the sidebar"),
    (&["secondary-l"], "Focus the message box"),
    (&["secondary-i"], "Bot info & threads"),
    (&["secondary-shift-a"], "Apps & connectors"),
    (&["secondary-shift-u"], "Usage"),
    (&["secondary-,"], "Settings"),
    (&["secondary-u"], "Attach files"),
    (&["secondary-shift-c"], "Copy conversation as Markdown"),
    (&["secondary-shift-."], "Stop the reply"),
    (&["escape"], "Close a dialog or panel"),
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
        KeyBinding::new(ks("secondary-shift-u"), OpenUsage, c),
        KeyBinding::new(ks("secondary-i"), ToggleInfo, c),
        KeyBinding::new(ks("secondary-b"), ToggleSidebar, c),
        KeyBinding::new(ks("secondary-shift-p"), TogglePin, c),
        KeyBinding::new(ks("secondary-l"), FocusComposer, c),
        KeyBinding::new(ks("secondary-shift-]"), NextBot, c),
        KeyBinding::new(ks("secondary-shift-["), PrevBot, c),
        KeyBinding::new("ctrl-tab", NextBot, c),
        KeyBinding::new("ctrl-shift-tab", PrevBot, c),
        KeyBinding::new(ks("secondary-u"), AttachFiles, c),
        KeyBinding::new(ks("secondary-shift-c"), CopyConversation, c),
        KeyBinding::new(ks("secondary-shift-."), StopTurn, c),
    ]);
}

pub fn shortcut_for(desc: &str) -> Option<&'static str> {
    SHORTCUTS.iter().find(|(_, d)| *d == desc).map(|(k, _)| k[0])
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

impl Approval {
    /// Shown (and answerable with `/approve`) in this chat: its own bot's approvals, plus ones
    /// Hermes couldn't tie to a session on the same server.
    pub fn belongs_to(&self, key: &BotKey) -> bool {
        match &self.key {
            Some(k) => k == key,
            None => self.conn == key.conn,
        }
    }

    /// Waiting for a bot that isn't the one on screen (sidebar badge + notification).
    pub fn waiting_on(&self, bot: &BotKey) -> bool {
        self.key.as_ref().is_some_and(|k| k.same_bot(bot))
    }
}

/// The `/` command popover: matching commands for what follows the slash.
/// Minimum window size on every platform: the composer, header and one chat column still fit.
pub const MIN_W: f32 = 560.;
pub const MIN_H: f32 = 420.;

pub struct SlashPopup {
    pub items: Vec<hermes_core::slash::SlashCmd>,
    pub sel: usize,
}

/// Usage page: one `GET /api/analytics/usage` per bot (profile) on each connection.
pub struct UsageState {
    pub days: u32,
    /// `None` = all bots.
    pub bot: Option<BotKey>,
    pub gen: u64,
    pub data: HashMap<BotKey, Result<hermes_core::usage::Usage, String>>,
}

impl UsageState {
    pub fn loading(&self, bots: &[Bot]) -> bool {
        bots.iter().any(|b| !self.data.contains_key(&b.key.base()))
    }
}

pub struct MentionPopup {
    pub at: usize,
    pub cursor: usize,
    pub items: Vec<BotKey>,
    pub sel: usize,
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
    Usage,
    Pin,
    Sidebar,
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
    pub slash: Option<SlashPopup>,
    pub slash_scroll: ScrollHandle,
    /// Live `commands.catalog` per bot (`conn:profile`); the embedded registry fills gaps.
    pub slash_cmds: HashMap<String, Vec<hermes_core::slash::SlashCmd>>,
    pub usage: Option<UsageState>,
    /// The primary modifier (⌘ / Ctrl) is held: sidebar rows show their 1–9 shortcut.
    pub cmd_held: bool,
    /// Sidebar shown over the chat on a narrow window.
    pub sidebar_peek: bool,
    /// Dev only (`resizebench` script step): render durations in ms.
    pub frame_stats: Option<Vec<f64>>,
    pub show_hidden: bool,
    pub toast: Option<(SharedString, bool)>,
    toast_gen: u64,
    pub media: HashMap<String, PathBuf>,
    media_requested: HashSet<String>,
    pub approvals: Vec<Approval>,
    pub onboard: Option<crate::onboard::Onboard>,
    /// Select this bot when it first shows up in the roster (just created).
    pub want_select: Option<BotKey>,
    pub new_bot: Option<NewBotForm>,
    pub settings: Option<SettingsState>,
    pub palette: Option<PaletteState>,
    pub apps: Option<AppsState>,
    pub info_open: bool,
    pub info: Option<InfoState>,
    pub thread_titles: HashMap<String, String>,
    /// Event chips (agent messages, tool runs, local events) the user has expanded, by message id.
    pub expanded_events: HashSet<String>,
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
/// The demo script, in development builds only.
pub fn scripted() -> Option<String> {
    dev_env("HERMES_DESKTOP_SCRIPT")
}

/// Slash commands handled by the desktop, whatever the bot's catalog says.
const LOCAL_SLASH: &[&str] = &["/usage", "/insights", "/stop", "/approve", "/deny"];

pub fn bootstrap_config(mut c: Config) -> Config {
    if let Some(url) = dev_env("HERMES_DESKTOP_URL") {
        if let Some(t) = dev_env("HERMES_DESKTOP_TOKEN") {
            std::env::set_var("HERMES_TOKEN_ENV", t);
        }
        c.connections.retain(|x| x.id != "env");
        c.connections.insert(0, Connection { id: "env".into(), name: dev_env("HERMES_DESKTOP_NAME").unwrap_or_else(|| "Hermes".into()), base_url: url, kind: ConnKind::Gateway });
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
                } else if this.slash.is_some() && !*secondary {
                    this.complete_slash(true, window, cx);
                } else if submit {
                    this.send(window, cx);
                }
            }
            InputEvent::Change => {
                this.update_mention(cx);
                this.update_slash(cx);
            }
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
            // A ⌘ released while another app had focus never reaches us.
            this.cmd_held = false;
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
            slash: None,
            slash_cmds: HashMap::new(),
            usage: None,
            cmd_held: false,
            sidebar_peek: false,
            frame_stats: None,
            slash_scroll: ScrollHandle::new(),
            show_hidden: false,
            toast: None,
            toast_gen: 0,
            media: HashMap::new(),
            media_requested: HashSet::new(),
            approvals: vec![],
            onboard: None,
            want_select: None,
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
            expanded_events: HashSet::new(),
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
            this.open_onboarding(crate::onboard::Step::Welcome, None, window, cx);
        }
        for c in this.config.connections.clone() {
            this.svc.send(Cmd::Connect(c));
        }
        if let Some(k) = this.selected.clone() {
            this.svc.send(Cmd::OpenBot(k));
        }
        this.pump_events(cx);
        if let Some(script) = scripted() {
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
        // Never activate or raise the window: a demo must not steal focus from whoever is working.
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
                self.update_slash(cx);
            }
            "send" => self.send(window, cx),
            "key" => {
                // Deferred: dispatching re-enters this view, which is mid-update here.
                // Like a user, type from the composer (with no focus the key context is missed).
                if window.focused(cx).is_none() {
                    self.composer.update(cx, |s, cx| s.focus(window, cx));
                }
                if let Ok(k) = Keystroke::parse(ks(arg)) {
                    window.defer(cx, move |window, cx| {
                        window.dispatch_keystroke(k, cx);
                    });
                }
            }
            // Dev: resize back and forth N times, one size per frame, and log frame timings.
            "resizebench" => {
                let n: usize = arg.parse().unwrap_or(120);
                self.frame_stats = Some(Vec::with_capacity(n));
                cx.spawn_in(window, async move |this, cx| {
                    for i in 0..n {
                        let w = 700.0 + 500.0 * ((i as f32 / 30.0) * std::f32::consts::PI).sin().abs();
                        let _ = this.update_in(cx, |_, window, _| window.resize(size(px(w), px(760.))));
                        cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                    }
                    let _ = this.update(cx, |this, _| {
                        if let Some(v) = this.frame_stats.take() {
                            let mut v = v;
                            v.sort_by(f64::total_cmp);
                            let p = |q: f64| v.get(((v.len() as f64 - 1.0) * q) as usize).copied().unwrap_or(0.0);
                            log::warn!("resizebench: {} renders, render ms p50 {:.2} p95 {:.2} max {:.2}", v.len(), p(0.5), p(0.95), p(1.0));
                        }
                    });
                })
                .detach();
            }
            "cmdheld" => self.cmd_held = arg != "off",
            "usage" => self.open_usage(window, cx),
            "usagebot" => {
                if let Some(u) = &mut self.usage {
                    u.bot = self.bots.iter().find(|b| b.name == arg).map(|b| b.key.base());
                }
            }
            "pin" => {
                if let Some(k) = self.bots.iter().find(|b| b.name == arg).map(|b| b.key.clone()) {
                    self.toggle_pin(&k, cx);
                }
            }
            "sidebar" => self.toggle_sidebar(window, cx),
            "gap" => {
                // Demo: pretend the stream went quiet `arg` hours ago.
                let h: f64 = arg.parse().unwrap_or(12.0);
                if let Some(chat) = self.selected.clone().and_then(|k| self.chats.get_mut(&k)) {
                    let shift = hermes_core::service::now() - h * 3600.0 - chat.msgs.iter().filter_map(|m| m.m.timestamp).fold(0.0, f64::max);
                    for m in &mut chat.msgs {
                        m.m.timestamp = m.m.timestamp.map(|t| t + shift);
                    }
                }
            }
            "unfold" => {
                if let Some(chat) = self.selected.as_ref().and_then(|k| self.chats.get(k)) {
                    for vm in &chat.msgs {
                        match vm.m.role {
                            Role::Assistant if !vm.m.activity.is_empty() => {
                                self.expanded_events.insert(format!("act-{}", vm.m.id));
                            }
                            Role::Agent(_) | Role::System | Role::Local(_) => {
                                self.expanded_events.insert(vm.m.id.clone());
                            }
                            _ => {}
                        }
                    }
                    self.list_count = usize::MAX;
                }
            }
            "approve" => {
                let sel = self.selected.clone();
                if let Some(id) = sel.and_then(|k| self.approvals.iter().find(|a| a.belongs_to(&k)).map(|a| a.request_id.clone())) {
                    self.approve(&id, arg, cx);
                }
            }
            "saveas" => {
                if let Some(key) = self.selected.clone() {
                    let dest = Some(hermes_core::conn::data_dir().join("demo-downloads"));
                    self.svc.send(Cmd::Download { key, remote: arg.to_owned(), dest, save_as: None, open: false });
                }
            }
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
            "setup" => self.open_onboarding(crate::onboard::Step::Server, None, window, cx),
            "onboard" => {
                if self.onboard.is_none() {
                    self.open_onboarding(crate::onboard::Step::parse(arg), None, window, cx);
                } else {
                    self.set_onboard_step(crate::onboard::Step::parse(arg), window, cx);
                }
            }
            "resignin" => {
                let id = self.config.connections.iter().find(|c| c.name == arg || c.id == arg).or(self.config.connections.first()).map(|c| c.id.clone());
                self.open_onboarding(crate::onboard::Step::SignIn, id, window, cx);
            }
            "addr" => {
                if let Some(o) = &self.onboard {
                    let v = arg.to_owned();
                    o.url.update(cx, |s, cx| s.set_value(v, window, cx));
                }
                self.schedule_probe(cx);
            }
            "pick" => self.pick_local(arg.parse().unwrap_or(0), window, cx),
            "creds" => {
                if let Some(o) = &self.onboard {
                    let (u, p) = arg.split_once('|').unwrap_or((arg, ""));
                    let (u, p) = (u.to_owned(), p.to_owned());
                    o.username.update(cx, |s, cx| s.set_value(u, window, cx));
                    o.password.update(cx, |s, cx| s.set_value(p, window, cx));
                }
            }
            "tokenfield" => {
                if let Some(o) = &self.onboard {
                    let v = arg.to_owned();
                    o.token.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }
            "bottitle" => {
                if let Some(o) = &self.onboard {
                    let v = arg.to_owned();
                    o.bot_title.update(cx, |s, cx| s.set_value(v, window, cx));
                }
            }
            "showpw" => self.toggle_show_password(window, cx),
            "next" => self.onboard_next(window, cx),
            "signout" => {
                if let Some(id) = self.config.connections.iter().find(|c| c.name == arg || c.id == arg).or(self.config.connections.first()).map(|c| c.id.clone()) {
                    self.sign_out(&id, cx);
                }
            }
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
            self.slash = None;
            self.sidebar_peek = false;
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
        self.load_slash_catalog(&key);
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
                if state != LinkState::Connected {
                    // The socket is gone: no turn on it can finish here. Stop the spinners; a
                    // running turn and its pending approvals come back from `session.resume`.
                    for (_, chat) in self.chats.iter_mut().filter(|(k, _)| k.conn == conn) {
                        chat.awaiting = false;
                        if chat.streaming {
                            chat.streaming = false;
                            chat.status = Some("Connection lost, picking the reply back up when Hermes is back…".into());
                        }
                        for m in chat.msgs.iter_mut().filter(|m| m.m.streaming) {
                            m.m.streaming = false;
                        }
                    }
                    self.approvals.retain(|a| a.conn != conn);
                }
                // Signed out in Settings: the gateway stops, but what the user needs is "Sign in".
                let state = if state == LinkState::Stopped && self.svc.store.get_kv(&format!("signedout:{conn}")).as_deref() == Some("1") { LinkState::Unauthorized } else { state };
                if state == LinkState::Unauthorized && self.links.get(&conn) != Some(&LinkState::Unauthorized) && self.svc.store.get_kv(&format!("signedout:{conn}")).as_deref() != Some("1") {
                    self.show_toast(format!("Sign in to {} again in Settings → Account.", self.conn_name(&conn)), true, cx);
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
                if let Some(want) = self.want_select.clone().filter(|w| self.bots.iter().any(|b| b.key.same_bot(w))) {
                    self.want_select = None;
                    self.selected = Some(want.clone());
                    self.list_count = usize::MAX;
                    self.svc.send(Cmd::OpenBot(want));
                }
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
                chat.status = None;
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
                if self.approvals.iter().any(|a| a.request_id == request_id) {
                    return; // replayed from `open_requests` after a reconnect
                }
                let a = Approval { conn, key, request_id, command, description, choices };
                let on_screen = self.selected.as_ref().is_some_and(|k| a.belongs_to(k)) && self.window_active;
                if !on_screen {
                    // Someone is waiting on the user: always say so, even with replies muted.
                    let who = a.key.as_ref().and_then(|k| self.bot(k)).map(|b| b.title()).unwrap_or_else(|| self.conn_name(&a.conn));
                    let body = if self.config.prefs.hide_previews { "A command is waiting for your approval.".to_owned() } else { a.command.clone() };
                    crate::notify::post(&format!("{who} needs approval"), &body, self.config.prefs.sounds);
                }
                self.approvals.push(a);
            }
            Ev::ApprovalGone { request_id } => self.approvals.retain(|a| a.request_id != request_id),
            Ev::MediaReady { remote, local } => {
                self.media.insert(remote, local);
            }
            Ev::Downloaded { key, path, open } => {
                if open {
                    if let Some(msg) = crate::guard::open_file(&path, cx) {
                        self.show_toast(msg, false, cx);
                    }
                } else {
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let dir = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let full = path.display().to_string();
                    // Show `~/…` rather than the full home path.
                    let shown = directories::UserDirs::new()
                        .and_then(|u| path.strip_prefix(u.home_dir()).ok().map(|r| format!("~/{}", r.display())))
                        .unwrap_or_else(|| full.clone());
                    self.push_local(&key, format!("Saved {name} to {dir}"), "download", None, Some(shown), Some(full));
                }
            }
            Ev::Notify { key, title, body } => {
                let p = &self.config.prefs;
                let alert = crate::notify::decide(Some(&key) == self.selected.as_ref(), self.window_active, p.notifications, p.notify_only_unfocused, p.sounds);
                if alert.mark_unread {
                    self.unread.insert(key.base());
                }
                if alert.notify {
                    crate::notify::post(&title, if p.hide_previews { "New message" } else { &body }, alert.sound);
                } else if alert.sound {
                    crate::notify::play_sound();
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
            Ev::Probed { tag, result } => self.on_probed(&tag, result, cx),
            Ev::LocalServers { servers } => self.on_local_servers(servers, cx),
            Ev::SignedIn { tag, conn_id, result } => self.on_signed_in(&tag, conn_id, result, cx),
            Ev::Slash { key, command, output, error } => {
                self.chat_mut(&key).awaiting = false;
                let icon = if error { "circle-alert" } else { "slash" };
                let text = if error { format!("{command} · not available here") } else { format!("Ran {command}") };
                let id = self.push_local(&key, text, icon, error.then(|| output.clone()), (!error).then_some(output), None);
                self.expanded_events.insert(id);
            }
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
        let first = text.split_whitespace().next().unwrap_or_default();
        let slash = files.is_empty() && !is_command && (LOCAL_SLASH.contains(&first) || hermes_core::slash::is_command(&self.slash_commands(), &text));
        if slash && self.local_slash(&text, window, cx) {
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
            self.slash = None;
            cx.notify();
            return;
        }
        // Long break since the last message: refresh the context first (Settings → Chat).
        let gap = (!slash && !is_command).then(|| self.gap_refresh(&key)).flatten();
        let refresh = gap.map(|(a, _)| a);
        if let Some((action, secs)) = gap {
            let away = fmt_gap(secs);
            let label = match action {
                hermes_core::conn::GapAction::Fresh => format!("Back after {away} · new session started"),
                _ => format!("Back after {away} · context compacted"),
            };
            let detail = "Saves tokens: Hermes summarises or sets aside the older context. The whole conversation stays here.".to_owned();
            self.push_local(&key, label, "history", Some(detail), None, None);
        }
        let chat = self.chat_mut(&key);
        if !is_command && !slash {
            chat.msgs.push(MsgVm { m, md: None });
        }
        if !is_command {
            chat.awaiting = true;
        }
        chat.error = None;
        self.svc.send(Cmd::Send { key, text, files, refresh, slash });
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.mention = None;
        self.slash = None;
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
                // The dialog already asked about replacing, so save exactly there.
                let dest = path.parent().map(|p| p.to_path_buf());
                svc.send(Cmd::Download { key, remote, dest, save_as: Some(path), open: false });
            }
        })
        .detach();
    }

    pub fn open_attachment(&mut self, key: BotKey, a: &Attachment, cx: &mut Context<Self>) {
        let local = a.local_path.as_ref().map(PathBuf::from).or_else(|| a.remote_path.as_ref().and_then(|r| self.media.get(r)).cloned());
        if let Some(l) = local {
            if let Some(msg) = crate::guard::open_file(&l, cx) {
                self.show_toast(msg, false, cx);
            }
        } else if let Some(remote) = a.remote_path.clone() {
            self.svc.send(Cmd::Download { key, remote, dest: Some(hermes_core::conn::data_dir().join("cache").join("downloads")), save_as: None, open: true });
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

    /// Add a session-only event chip (Hermes history does not record approvals or local saves).
    pub fn push_local(&mut self, key: &BotKey, text: String, icon: &str, detail: Option<String>, code: Option<String>, path: Option<String>) -> String {
        let ev = transcript::LocalEvent { icon: icon.to_owned(), detail, code, path };
        let id = format!("local-{}", uuid::Uuid::new_v4());
        let m = ChatMessage::new(id.clone(), Role::Local(ev), text, Some(hermes_core::service::now()));
        let chat = self.chat_mut(key);
        // Keep a live reply last so streaming keeps appending to it.
        let at = if chat.msgs.last().is_some_and(|m| m.m.streaming) { chat.msgs.len() - 1 } else { chat.msgs.len() };
        chat.msgs.insert(at, MsgVm { m, md: None });
        if self.selected.as_ref() == Some(key) {
            self.list_count = usize::MAX;
        }
        id
    }

    // ---- long breaks, pinning, sidebar ------------------------------------------------------

    /// The refresh to apply when the bot's stream has been quiet longer than the setting.
    /// The refresh to apply before sending, and how long the break was (seconds).
    fn gap_refresh(&self, key: &BotKey) -> Option<(hermes_core::conn::GapAction, f64)> {
        use hermes_core::conn::GapAction;
        let p = &self.config.prefs;
        if p.gap_action == GapAction::Off || key.thread.is_some() {
            return None;
        }
        let last = self.chats.get(key)?.msgs.iter().rev().filter(|m| matches!(m.m.role, Role::User | Role::Assistant | Role::Agent(_))).find_map(|m| m.m.timestamp)?;
        let gap = hermes_core::service::now() - last;
        (gap > p.gap_hours as f64 * 3600.0).then_some((p.gap_action, gap))
    }

    /// Pin state lives in the bot's Bot Mode metadata (`ui_meta["hermes-bots"].pinned`), so every
    /// desktop on that Hermes sees it. Applied locally at once, then written with `SetMeta`.
    pub fn toggle_pin(&mut self, key: &BotKey, cx: &mut Context<Self>) {
        let Some(b) = self.bots.iter_mut().find(|b| b.key.same_bot(key)) else { return };
        let pinned = !b.meta.pinned.unwrap_or(false);
        b.meta.pinned = pinned.then_some(true);
        let title = b.title();
        roster::sort_roster(&mut self.bots);
        self.svc.send(Cmd::SetMeta { key: key.base(), patch: json!({"pinned": if pinned { Value::Bool(true) } else { Value::Null }}) });
        self.show_toast(if pinned { format!("Pinned {title}") } else { format!("Unpinned {title}") }, false, cx);
        cx.notify();
    }

    pub fn toggle_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar_auto_hidden(window) {
            self.sidebar_peek = !self.sidebar_peek;
            cx.notify();
            return;
        }
        self.set_pref(|p| p.sidebar_collapsed = !p.sidebar_collapsed, window, cx);
    }

    /// Below this width the sidebar steps aside so the conversation keeps a usable column.
    pub fn sidebar_auto_hidden(&self, window: &Window) -> bool {
        window.viewport_size().width < px(crate::view::SIDEBAR_AUTO_HIDE)
    }

    pub fn sidebar_visible(&self, window: &Window) -> bool {
        !self.config.prefs.sidebar_collapsed && !self.sidebar_auto_hidden(window)
    }

    pub fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |s, cx| s.focus(window, cx));
    }

    // ---- slash commands ----------------------------------------------------------------------

    fn slash_key(key: &BotKey) -> String {
        format!("{}:{}", key.conn, key.profile)
    }

    /// Ask the bot's Hermes for its live command catalog (once per bot).
    pub fn load_slash_catalog(&mut self, key: &BotKey) {
        let k = Self::slash_key(key);
        if !self.slash_cmds.contains_key(&k) {
            self.slash_cmds.insert(k.clone(), vec![]);
            self.rpc(&key.conn, "commands.catalog", json!({"profile": key.profile}), format!("slash:{k}"));
        }
    }

    pub fn slash_commands(&self) -> Vec<hermes_core::slash::SlashCmd> {
        let live = self.selected.as_ref().and_then(|k| self.slash_cmds.get(&Self::slash_key(k))).cloned().unwrap_or_default();
        hermes_core::slash::merged(&live)
    }

    pub fn update_slash(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let prev = self.slash.as_ref().map(|s| s.sel).unwrap_or(0);
        self.slash = None;
        if let Some(q) = text.strip_prefix('/') {
            if !q.contains(char::is_whitespace) && !q.starts_with('/') {
                let all = self.slash_commands();
                let items: Vec<_> = hermes_core::slash::filter(&all, q).into_iter().take(50).cloned().collect();
                if !items.is_empty() {
                    let sel = prev.min(items.len() - 1);
                    self.slash = Some(SlashPopup { items, sel });
                }
            }
        }
        cx.notify();
    }

    pub fn move_slash(&mut self, delta: i32, cx: &mut Context<Self>) {
        if let Some(s) = &mut self.slash {
            let n = s.items.len() as i32;
            s.sel = ((s.sel as i32 + delta).rem_euclid(n)) as usize;
            self.slash_scroll.scroll_to_item(s.sel);
            cx.notify();
        }
    }

    /// Fill in the highlighted command. With `run`, a command that takes no arguments is sent
    /// straight away; one that does gets a trailing space so the user can type them.
    pub fn complete_slash(&mut self, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(c) = self.slash.as_ref().and_then(|s| s.items.get(s.sel)).cloned() else { return };
        self.pick_slash(&c, run, window, cx);
    }

    pub fn pick_slash(&mut self, c: &hermes_core::slash::SlashCmd, run: bool, window: &mut Window, cx: &mut Context<Self>) {
        let takes_args = !c.args.is_empty() || !c.subcommands.is_empty();
        let text = if takes_args { format!("/{} ", c.name) } else { format!("/{}", c.name) };
        let end = text.len();
        self.composer.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            s.set_selected_range(end..end, cx);
            s.focus(window, cx);
        });
        self.slash = None;
        if run && !takes_args {
            self.send(window, cx);
        }
        cx.notify();
    }

    /// Commands the app answers itself. Returns true when handled.
    /// Commands the desktop handles itself (see `local_slash`).
    fn local_slash(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = text.split_whitespace().next().unwrap_or_default();
        match name {
            "/usage" | "/insights" => {
                self.open_usage(window, cx);
                true
            }
            "/stop" => {
                self.stop(cx);
                true
            }
            "/approve" | "/deny" => {
                // Only this chat's approval: never one from another bot the user can't see.
                let sel = self.selected.clone();
                if let Some(id) = sel.and_then(|k| self.approvals.iter().find(|a| a.belongs_to(&k)).map(|a| a.request_id.clone())) {
                    self.approve(&id, if name == "/deny" { "deny" } else { "once" }, cx);
                } else {
                    let elsewhere = self.approvals.len();
                    self.show_toast(if elsewhere > 0 { format!("Nothing here is waiting for approval ({elsewhere} waiting in other chats).") } else { "Nothing is waiting for approval.".to_owned() }, false, cx);
                }
                true
            }
            _ => false,
        }
    }

    // ---- usage -------------------------------------------------------------------------------

    pub fn open_usage(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        self.usage = Some(UsageState { days: 30, bot: None, gen: 0, data: HashMap::new() });
        self.reload_usage(cx);
        cx.notify();
    }

    pub fn reload_usage(&mut self, cx: &mut Context<Self>) {
        let bots: Vec<BotKey> = self.bots.iter().map(|b| b.key.base()).collect();
        let Some(u) = &mut self.usage else { return };
        u.gen += 1;
        u.data.clear();
        let (gen, days) = (u.gen, u.days);
        for k in bots {
            self.svc.send(Cmd::Usage { conn: k.conn.clone(), profile: Some(k.profile.clone()), days, tag: format!("usage:{gen}:{}:{}", k.conn, k.profile) });
        }
        cx.notify();
    }

    /// Expand or collapse an event chip; `ix` is its list row, which must be re-measured.
    pub fn toggle_event(&mut self, id: &str, ix: usize, cx: &mut Context<Self>) {
        if !self.expanded_events.remove(id) {
            self.expanded_events.insert(id.to_owned());
        }
        if ix < self.list_count && self.list_count != usize::MAX {
            self.list.splice(ix..ix + 1, 1);
        }
        cx.notify();
    }

    pub fn approve(&mut self, request_id: &str, choice: &str, cx: &mut Context<Self>) {
        if let Some(i) = self.approvals.iter().position(|a| a.request_id == request_id) {
            let a = self.approvals.remove(i);
            if let Some(key) = a.key.clone() {
                let (verb, icon) = if choice == "deny" { ("Denied", "shield-x") } else { ("Approved", "shield-check") };
                let scope = match choice {
                    "session" => " for this session",
                    "always" => " always",
                    _ => "",
                };
                let detail = (!a.description.is_empty()).then(|| a.description.clone());
                self.push_local(&key, format!("{verb}{scope} · {}", roster::one_line(&a.command, 60)), icon, detail, Some(a.command.clone()), None);
            }
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
        self.palette.is_some() || self.settings.is_some() || self.apps.is_some() || self.onboard.is_some() || self.new_bot.is_some()
    }

    /// Close the top-most overlay (Escape). Returns whether anything closed.
    pub fn close_top(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let closed = if self.onboard.as_ref().is_some_and(|o| !o.first_run && !o.busy) {
            self.onboard = None;
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
        } else if self.usage.is_some() {
            self.usage = None;
            true
        } else if self.slash.is_some() {
            self.slash = None;
            true
        } else if self.sidebar_peek {
            self.sidebar_peek = false;
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
        self.usage = None;
        self.new_bot = None;
        if self.onboard.as_ref().is_some_and(|o| !o.first_run && !o.busy) {
            self.onboard = None;
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
        // Stops the link, revokes the live session and deletes the keychain entry off the UI thread.
        self.svc.send(Cmd::SignOut { conn_id: id.to_owned() });
        let _ = self.svc.store.set_kv(&format!("account:{id}"), "");
        let _ = self.svc.store.set_kv(&format!("signedout:{id}"), "");
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
        let pinned = self.current_bot().is_some_and(|b| b.meta.pinned.unwrap_or(false));
        let acts: [(Act, &str, Option<&'static str>); 13] = [
            (Act::NewBot, "New bot", shortcut_for("New bot")),
            (Act::Usage, "Usage and cost", shortcut_for("Usage")),
            (Act::Pin, if pinned { "Unpin this bot" } else { "Pin this bot to the top" }, shortcut_for("Pin or unpin bot")),
            (Act::Sidebar, "Show or hide the sidebar", shortcut_for("Show or hide the sidebar")),
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
            Act::Usage => self.open_usage(window, cx),
            Act::Pin => {
                if let Some(k) = self.selected.clone() {
                    self.toggle_pin(&k, cx);
                }
            }
            Act::Sidebar => self.toggle_sidebar(window, cx),
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
                    self.svc.send(Cmd::Send { key: k.base(), text: "/compact".into(), files: vec![], refresh: None, slash: true });
                }
            }
            Act::AddServer => self.open_onboarding(crate::onboard::Step::Server, None, window, cx),
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
        if let Some(k) = tag.strip_prefix("slash:") {
            // A Hermes without `commands.catalog` keeps the embedded registry.
            if let Ok(v) = &result {
                self.slash_cmds.insert(k.to_owned(), hermes_core::slash::from_catalog(v));
            }
            return;
        }
        if let Some(rest) = tag.strip_prefix("usage:") {
            let mut it = rest.splitn(3, ':');
            let (gen, conn, profile) = (it.next().and_then(|g| g.parse::<u64>().ok()).unwrap_or(0), it.next().unwrap_or_default(), it.next().unwrap_or_default());
            if let Some(u) = &mut self.usage {
                if u.gen == gen {
                    u.data.insert(BotKey::new(conn, profile), result.map(|v| hermes_core::usage::parse(&v)));
                    cx.notify();
                }
            }
            return;
        }
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
                                Some(url) if matches!(hermes_core::files::link_action(&url), hermes_core::files::LinkAction::Open(_)) => {
                                    cx.open_url(&url);
                                    self.show_toast("Finish connecting in your browser. Hermes picks it up automatically.", false, cx);
                                }
                                _ => self.show_toast("Connection started. Hermes will finish it.", false, cx),
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

    // ---- bot creation --------------------------------------------------

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

/// "8 hours", "3 days".
fn fmt_gap(secs: f64) -> String {
    let h = (secs / 3600.0).floor() as i64;
    match h {
        0 | 1 => "an hour".into(),
        2..=47 => format!("{h} hours"),
        _ => format!("{} days", h / 24),
    }
}
