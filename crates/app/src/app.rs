//! Application state: the roster, chats, composer, and the bridge to the core service.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::*;
use gpui_component::input::{InputEvent, InputState, TextareaState};
use gpui_component::text::TextViewState;
use gpui_component::{Theme, ThemeMode};
use hermes_core::conn::{ConnKind, Config, Connection, Secrets};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{self, Bot, BotKey};
use hermes_core::service::{Cmd, Ev, LocalFile, MAX_FILE_BYTES};
use hermes_core::transcript::{self, Activity, Attachment, ChatMessage, Role};
use hermes_core::Service;

use crate::theme::{Palette, DEFAULT_ACCENT};

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
    pub list: ListState,
    list_count: usize,
    pub dragging: bool,
    pub focus: FocusHandle,
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

fn accent_of(c: &Config) -> u32 {
    c.accent.as_deref().and_then(|a| u32::from_str_radix(a.trim_start_matches('#'), 16).ok()).unwrap_or(DEFAULT_ACCENT)
}

impl HermesApp {
    pub fn new(svc: Rc<Service>, config: Config, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pal = Palette::for_appearance(window.appearance(), accent_of(&config));
        Theme::change(if pal.dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(1, 8).submit_on_enter(true).placeholder("Message…")
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search bots"));
        let mut subs = vec![];
        subs.push(cx.subscribe_in(&composer, window, |this, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { shift: false, .. } => {
                if this.mention.is_some() {
                    this.complete_mention(window, cx);
                } else {
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
        subs.push(cx.observe_window_appearance(window, |this, window, cx| {
            this.pal = Palette::for_appearance(window.appearance(), accent_of(&this.config));
            Theme::change(if this.pal.dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
            cx.notify();
        }));

        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
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
            list,
            list_count: 0,
            dragging: false,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        // Paint the cached roster immediately, then connect.
        if let Some(last) = this.svc.store.get_kv("selected").and_then(|s| serde_json::from_str::<BotKey>(&s).ok()) {
            this.selected = Some(last);
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

    /// Development aid for reproducible demos and screenshots: `;`-separated steps such as
    /// `wait:1500;select:researcher;type:hi @sc;send;attach:/path/file.png`. It drives the same
    /// code paths as the mouse and keyboard.
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
                    let _ = this.update(cx, |app, cx| match cmd.as_str() {
                        "select" => {
                            if let Some(k) = app.bots.iter().find(|b| b.name == arg).map(|b| b.key.clone()) {
                                app.select(k, window, cx);
                            }
                        }
                        "type" => {
                            app.composer.update(cx, |s, cx| {
                                s.set_value(arg.clone(), window, cx);
                                s.focus(window, cx);
                            });
                            app.update_mention(cx);
                        }
                        "send" => app.send(window, cx),
                        "attach" => app.add_paths(&[PathBuf::from(&arg)], cx),
                        "share" => app.share(cx),
                        "hidden" => {
                            app.show_hidden = !app.show_hidden;
                            cx.notify();
                        }
                        "newbot" => app.open_new_bot(window, cx),
                        "close" => {
                            app.new_bot = None;
                            app.setup = None;
                            cx.notify();
                        }
                        _ => {}
                    });
                });
            }
        })
        .detach();
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
                if this.update(cx, |app, cx| {
                    for ev in batch {
                        app.on_event(ev, cx);
                    }
                    app.sync_list();
                    cx.notify();
                }).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn visible_bots(&self, cx: &App) -> Vec<&Bot> {
        let q = self.search.read(cx).value().to_lowercase();
        self.bots
            .iter()
            .filter(|b| self.show_hidden || !b.hidden() || Some(&b.key) == self.selected.as_ref())
            .filter(|b| q.is_empty() || b.title().to_lowercase().contains(&q) || b.name.contains(&q) || b.preview().to_lowercase().contains(&q))
            .collect()
    }

    pub fn bot(&self, key: &BotKey) -> Option<&Bot> {
        self.bots.iter().find(|b| b.key == *key)
    }

    pub fn current_chat(&self) -> Option<&Chat> {
        self.selected.as_ref().and_then(|k| self.chats.get(k))
    }

    pub fn conn_name(&self, conn: &str) -> String {
        self.config.connections.iter().find(|c| c.id == conn).map(|c| c.name.clone()).unwrap_or_else(|| conn.to_owned())
    }

    pub fn select(&mut self, key: BotKey, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&key) {
            self.selected = Some(key.clone());
            self.mention = None;
            let _ = self.svc.store.set_kv("selected", &serde_json::to_string(&key).unwrap_or_default());
            self.list_count = usize::MAX; // force a reset
            self.sync_list();
        }
        self.unread.remove(&key);
        self.svc.send(Cmd::MarkRead(key.clone()));
        self.svc.send(Cmd::OpenBot(key));
        self.composer.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
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

    fn streaming_msg<'a>(chat: &'a mut Chat) -> &'a mut MsgVm {
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
                self.links.insert(conn, state);
            }
            Ev::Roster { conn, bots, unread } => {
                self.bots.retain(|b| b.key.conn != conn);
                self.bots.extend(bots);
                roster::sort_roster(&mut self.bots);
                self.unread.retain(|k| k.conn != conn);
                self.unread.extend(unread.into_iter().filter(|k| Some(k) != self.selected.as_ref()));
                if self.selected.as_ref().is_none_or(|k| !self.bots.iter().any(|b| b.key == *k)) {
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
                // Keep optimistic messages that the server has not stored yet.
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
                    self.svc.send(Cmd::MarkRead(key));
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
                    self.unread.insert(key);
                    crate::view::notify_os(&title, &body);
                }
            }
            Ev::Error { key, message } => {
                match key.and_then(|k| self.chats.get_mut(&k)) {
                    Some(chat) if !message.is_empty() => {
                        chat.error = Some(message);
                        chat.awaiting = false;
                    }
                    _ => self.show_toast(message, true, cx),
                }
            }
            Ev::Info { message } => self.show_toast(message, false, cx),
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
        let is_command = roster::rewrite_bot_chat_command(&text).is_some() && files.is_empty();
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

    pub fn save_attachment(&mut self, key: BotKey, a: &Attachment, cx: &mut Context<Self>) {
        let Some(remote) = a.remote_path.clone() else {
            if let Some(l) = &a.local_path {
                cx.reveal_path(std::path::Path::new(l));
            }
            return;
        };
        let start = directories::UserDirs::new().and_then(|u| u.download_dir().map(|p| p.to_path_buf())).unwrap_or_else(std::env::temp_dir);
        let rx = cx.prompt_for_new_path(&start, Some(&a.name));
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
        let title = self.bot(&key).map(|b| b.title()).unwrap_or_default();
        let msgs: Vec<ChatMessage> = self.chats.get(&key).map(|c| c.msgs.iter().map(|m| m.m.clone()).collect()).unwrap_or_default();
        cx.write_to_clipboard(ClipboardItem::new_string(transcript::to_markdown(&title, &msgs)));
        self.show_toast("Conversation copied to the clipboard as Markdown", false, cx);
    }

    pub fn toggle_hidden(&mut self, key: BotKey, cx: &mut Context<Self>) {
        let hidden = self.bot(&key).map(|b| b.hidden()).unwrap_or(false);
        // Stored in the bot's profile metadata, so every desktop on that backend follows it.
        self.svc.send(Cmd::SetMeta { key, patch: serde_json::json!({"hidden": if hidden { serde_json::Value::Null } else { serde_json::Value::Bool(true) }}) });
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
        if clash && !home { format!("{h}@{}", roster::slug(&b.conn_name, true)) } else { h }
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

    // ---- connection setup and bot creation --------------------------------------------------

    pub fn open_setup(&mut self, editing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let existing = editing.as_ref().and_then(|id| self.config.connections.iter().find(|c| c.id == *id).cloned());
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Device name, e.g. Homelab").default_value(existing.as_ref().map(|c| c.name.clone()).unwrap_or_else(|| "Hermes".into())));
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("http://my-hermes.tailnet.ts.net:9119").default_value(existing.as_ref().map(|c| c.base_url.clone()).unwrap_or_default()));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder(if existing.is_some() { "Leave blank to keep the saved token" } else { "Dashboard session token" }));
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
        self.config.connections.retain(|c| c.id != id);
        self.config.connections.push(conn.clone());
        if let Err(e) = self.config.save() {
            self.show_toast(format!("Couldn't save settings: {e}"), true, cx);
        }
        self.svc.send(Cmd::Connect(conn));
        self.setup = None;
        cx.notify();
    }

    pub fn open_new_bot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Profile name, e.g. researcher"));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title, e.g. Research Buddy"));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("What is this bot good at?"));
        self.new_bot = Some(NewBotForm { name, title, description });
        cx.notify();
    }

    pub fn create_bot(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.new_bot.take() else { return };
        let name = roster::slug(&f.name.read(cx).value(), true);
        let title = f.title.read(cx).value().trim().to_owned();
        let description = f.description.read(cx).value().trim().to_owned();
        let conn = self.selected.as_ref().map(|k| k.conn.clone()).or_else(|| self.config.connections.first().map(|c| c.id.clone()));
        match conn {
            Some(conn) if !name.is_empty() => self.svc.send(Cmd::CreateBot { conn, name, title, description }),
            _ => self.show_toast("A bot needs a profile name.", true, cx),
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
