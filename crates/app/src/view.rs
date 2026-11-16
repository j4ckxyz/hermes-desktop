//! Main window rendering: sidebar roster, chat pane, composer, bot info pane.
//! Overlays (palette, settings, apps, forms) live in `overlays.rs`.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, Textarea};
use gpui_component::ActiveTheme as _;
use gpui_component::Sizable as _;
use gpui_component::text::{TextView, TextViewState};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{Bot, BotKey};
use hermes_core::transcript::{Activity, Attachment, AttachmentKind, ChatMessage, Role, ToolState};

use crate::app::*;
use crate::theme::ACCENTS;
use crate::ui::*;

/// What sits in front of an event chip's label or object name.
pub enum Lead {
    None,
    Icon(&'static str),
    Avatar(Option<Box<Bot>>, String),
}

/// A centred event: the chip, then (when expanded) its detail panel underneath.
fn event_row(outer: Div, chip: AnyElement, panel: Option<Div>) -> AnyElement {
    outer.flex().flex_col().items_center().gap(px(6.)).child(chip).when_some(panel, |d, p| d.child(p)).into_any_element()
}

/// The muted card an expanded chip opens.
fn event_panel(pal: &crate::theme::Palette) -> Div {
    div().w_full().max_w(px(560.)).p(px(12.)).flex().flex_col().gap(px(8.)).rounded(px(12.)).bg(pal.panel).border_1().border_color(pal.border)
}

pub const SIDEBAR_W: f32 = 280.;
pub const INFO_W: f32 = 320.;
/// Readable line length for the conversation column.
pub const CONTENT_MAX_W: f32 = 760.;
const TOP_INSET: f32 = if cfg!(target_os = "macos") { 36. } else { 8. };

/// Post an OS notification without extra dependencies. macOS uses `osascript`, Linux uses
/// `notify-send` (freedesktop), and Windows uses a PowerShell toast.
pub fn notify_os(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title));
        let _ = std::process::Command::new("osascript").arg("-e").arg(script).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("notify-send").arg("--app-name=Hermes").arg(title).arg(body).spawn();
    }
    #[cfg(windows)]
    {
        // Untested: WinRT toast through PowerShell, no extra crates.
        let esc = |s: &str| s.replace('\'', "''");
        let ps = format!(
            "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
             $t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $x = $t.GetElementsByTagName('text'); $x.Item(0).AppendChild($t.CreateTextNode('{}')) > $null; $x.Item(1).AppendChild($t.CreateTextNode('{}')) > $null; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Hermes').Show([Windows.UI.Notifications.ToastNotification]::new($t))",
            esc(title),
            esc(body)
        );
        let _ = std::process::Command::new("powershell").args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &ps]).spawn();
    }
}

/// A short system sound for new replies (Settings → Notifications → Sounds).
pub fn play_sound() {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("afplay").arg("/System/Library/Sounds/Tink.aiff").spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("canberra-gtk-play").args(["-i", "message-new-instant"]).spawn();
    #[cfg(windows)]
    let _ = std::process::Command::new("powershell").args(["-NoProfile", "-Command", "[System.Media.SystemSounds]::Asterisk.Play()"]).spawn();
}

impl HermesApp {
    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let current = self.current_bot().cloned();
        let any_hidden = self.bots.iter().any(|b| b.hidden());
        let hidden_unread = self.bots.iter().any(|b| b.hidden() && self.unread.contains(&b.key));
        let rows: Vec<Bot> = self.visible_bots(cx).into_iter().cloned().collect();
        let user = self.config.user_name.clone().or_else(|| std::env::var("USER").ok()).unwrap_or_else(|| "You".into());
        let link_ok = self.links.values().any(|l| *l == LinkState::Connected);
        let link_label = if self.config.connections.is_empty() {
            "Not connected".to_owned()
        } else if link_ok {
            self.config.connections.iter().map(|c| c.name.clone()).collect::<Vec<_>>().join(", ")
        } else if self.links.values().any(|l| *l == LinkState::Unauthorized) {
            "Token refused".to_owned()
        } else {
            "Connecting…".to_owned()
        };
        let loading = self.bots.is_empty() && !self.config.connections.is_empty();
        let searching = self.searching;

        div()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(pal.sidebar)
            .border_r_1()
            .border_color(pal.border)
            // Top: current bot, search, new bot.
            .child(
                div()
                    .pt(px(TOP_INSET))
                    .px(px(12.))
                    .h(px(TOP_INSET + 48.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(avatar_for(current.as_ref(), "H", 28.))
                    .child(div().flex_1().min_w_0().text_size(pal.t(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(current.as_ref().map(|b| b.title()).unwrap_or_else(|| "Hermes".into())))
                    .child(icon_btn("search", "search", &pal).when(searching, |d| d.bg(pal.selected)).on_click(cx.listener(|this, _, window, cx| {
                        this.searching = !this.searching;
                        if this.searching {
                            this.search.update(cx, |s, cx| s.focus(window, cx));
                        } else {
                            this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        cx.notify();
                    })))
                    .child(icon_btn("new-bot", "plus", &pal).on_click(cx.listener(|this, _, window, cx| this.open_new_bot(window, cx)))),
            )
            .when(searching, |d| d.child(div().px(px(12.)).pb(px(8.)).flex_none().child(Input::new(&self.search).small())))
            // Section header.
            .child(
                div()
                    .h(px(32.))
                    .flex_none()
                    .pl(px(20.))
                    .pr(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(section_label("BOTS", &pal))
                    .when(any_hidden, |d| {
                        d.child(
                            div()
                                .relative()
                                .child(icon_btn_sized("hidden", if self.show_hidden { "eye" } else { "eye-off" }, 24., 14., &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.show_hidden = !this.show_hidden;
                                    cx.notify();
                                })))
                                .when(hidden_unread, |d| d.child(div().absolute().top(px(2.)).right(px(2.)).size(px(6.)).rounded_full().bg(pal.accent))),
                        )
                    }),
            )
            // Roster.
            .child(
                div()
                    .id("roster")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(rows.into_iter().map(|b| self.render_row(b, cx)))
                    .when(loading, |d| d.children((0..5).map(|i| roster_skeleton(i, &pal))))
                    .when(self.config.connections.is_empty(), |d| {
                        d.child(div().p(px(12.)).text_size(pal.t(13.)).text_color(pal.faint).child("Connect to your Hermes to see your bots."))
                    })
                    .when(!loading && searching && self.visible_bots(cx).is_empty() && !self.bots.is_empty(), |d| {
                        d.child(div().p(px(12.)).text_size(pal.t(13.)).text_color(pal.faint).child("No bots match."))
                    }),
            )
            // Account.
            .child(
                div()
                    .id("account")
                    .h(px(60.))
                    .flex_none()
                    .border_t_1()
                    .border_color(pal.border)
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("account-open")
                            .flex_1()
                            .min_w_0()
                            .h(px(44.))
                            .px(px(4.))
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(pal.hover))
                            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                            .child(avatar_for(None, &user, 28.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_size(pal.t(13.)).font_weight(FontWeight::MEDIUM).truncate().child(user))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(div().size(px(6.)).flex_none().rounded_full().bg(if link_ok { pal.ok } else { pal.faint }))
                                            .child(div().text_size(pal.t(11.)).text_color(pal.muted).truncate().child(link_label)),
                                    ),
                            ),
                    )
                    .child(icon_btn("settings", "settings", &pal).on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))),
            )
    }

    fn render_row(&mut self, b: Bot, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let selected = self.selected.as_ref().is_some_and(|s| s.same_bot(&b.key));
        let unread = self.unread.contains(&b.key);
        let typing = self.chats.iter().any(|(k, c)| k.same_bot(&b.key) && (c.streaming || c.awaiting));
        let key = b.key.clone();
        let multi = self.config.connections.len() > 1;
        let hide = self.config.prefs.hide_previews;
        let preview = if typing {
            "typing…".to_owned()
        } else if hide {
            if unread { "New message".into() } else { format!("@{}", b.handle()) }
        } else {
            b.preview()
        };
        let (hover, pressed) = (pal.hover, pal.pressed);
        div()
            .id(SharedString::from(format!("row-{}-{}", b.key.conn, b.key.profile)))
            .h(pal.g(56.).max(px(48.)))
            .flex_none()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(8.))
            .rounded(px(10.))
            .cursor_pointer()
            .when(selected, |d| d.bg(pal.selected))
            .when(!selected, move |d| d.hover(move |s| s.bg(hover)).active(move |s| s.bg(pressed)))
            .when(b.hidden(), |d| d.opacity(0.55))
            .on_click(cx.listener(move |this, _, window, cx| this.select(key.clone(), window, cx)))
            .child(avatar_for(Some(&b), &b.name, 36.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().truncate().text_size(pal.t(14.)).font_weight(if unread { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM }).child(b.title()))
                            .when(multi, |d| d.child(div().flex_none().text_size(pal.t(10.)).text_color(pal.faint).child(b.conn_name.clone())))
                            .child(div().flex_none().text_size(pal.t(11.)).text_color(if unread { pal.accent } else { pal.faint }).child(fmt_time(Some(b.last_active())))),
                    )
                    .child(
                        div()
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(pal.t(13.))
                                    .text_color(if typing { pal.accent } else if unread { pal.text } else { pal.muted })
                                    .child(if preview.is_empty() { "No messages yet".to_owned() } else { preview }),
                            )
                            .when(unread, |d| d.child(div().size(px(8.)).flex_none().rounded_full().bg(pal.accent))),
                    ),
            )
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let bot = self.current_bot().cloned();
        let chat = self.current_chat();
        let thread = self.selected.as_ref().and_then(|k| self.thread_title(k));
        let sub = match (&bot, chat) {
            (_, Some(c)) if c.streaming || c.awaiting => c.status.clone().unwrap_or_else(|| "thinking…".into()),
            (Some(b), _) => {
                let mut s = format!("@{}", b.handle());
                if thread.is_some() {
                    s = format!("Thread with {}", b.title());
                } else if let Some(m) = &b.model {
                    s.push_str(&format!(" · {m}"));
                }
                if self.config.connections.len() > 1 {
                    s.push_str(&format!(" · {}", b.conn_name));
                }
                s
            }
            _ => String::new(),
        };
        let busy = chat.is_some_and(|c| c.streaming || c.awaiting);
        div()
            .h(px(TOP_INSET.max(8.) + 32.).max(px(56.)))
            .flex_none()
            .pl(px(16.))
            .pr(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(pal.border)
            .when(thread.is_some(), |d| {
                d.child(icon_btn("back", "arrow-left", &pal).on_click(cx.listener(|this, _, window, cx| this.run_act(Act::BotChat, window, cx))))
            })
            .when_some(bot.clone(), |d, b| {
                d.child(avatar_for(Some(&b), &b.name, 32.)).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(pal.t(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(thread.clone().unwrap_or_else(|| b.title())))
                        .child(div().text_size(pal.t(12.)).text_color(if busy { pal.accent } else { pal.muted }).truncate().child(sub)),
                )
            })
            .when(bot.is_none(), |d| d.child(div().flex_1()))
            .child(icon_btn("apps", "layout-grid", &pal).on_click(cx.listener(|this, _, window, cx| this.open_apps(window, cx))))
            .child(icon_btn("info", "panel-right", &pal).when(self.info_open, |d| d.bg(pal.selected)).on_click(cx.listener(|this, _, window, cx| this.toggle_info(window, cx))))
            .child(
                button("share", "Share", Kind::Secondary, &pal)
                    .child(ico("share", 14., pal.text))
                    .flex_row_reverse()
                    .on_click(cx.listener(|this, _, _, cx| this.share(cx))),
            )
    }

    pub fn render_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(key) = self.selected.clone() else { return div().into_any_element() };
        let n = self.chats.get(&key).map(|c| c.msgs.len()).unwrap_or(0);
        let inner = if ix >= n { self.render_footer(&key, window, cx).into_any_element() } else { self.render_message(&key, ix, cx) };
        // Centre a readable column inside the full-width list.
        div().w_full().flex().justify_center().child(div().w_full().max_w(px(CONTENT_MAX_W)).child(inner)).into_any_element()
    }

    fn render_message(&mut self, key: &BotKey, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let pal = self.pal;
        let prefs = self.config.prefs.clone();
        // Lazily create the markdown state (parsed once, then updated incrementally).
        let md = {
            let chat = self.chats.get_mut(key).unwrap();
            let vm = &mut chat.msgs[ix];
            if prefs.render_markdown && vm.md.is_none() && !vm.m.text.is_empty() {
                let text = vm.m.text.clone();
                vm.md = Some(cx.new(|cx| TextViewState::markdown(&text, cx).selectable(true)));
            }
            vm.md.clone()
        };
        let m = self.chats[key].msgs[ix].m.clone();
        let prev_role = ix.checked_sub(1).and_then(|i| self.chats[key].msgs.get(i)).map(|p| p.m.role.clone());
        let same = prev_role.as_ref() == Some(&m.role);
        let top_gap = if same { pal.g(4.) } else { pal.g(16.) };
        for a in &m.attachments {
            if a.kind == AttachmentKind::Image && a.local_path.is_none() && prefs.image_previews {
                self.want_media(key, a);
            }
        }
        let bot = self.bot(key).cloned();
        let body: AnyElement = match (&md, prefs.render_markdown) {
            (Some(s), true) => TextView::new(s).selectable(true).into_any_element(),
            _ => div().child(m.text.clone()).into_any_element(),
        };
        let time = if prefs.show_timestamps { fmt_clock(m.timestamp) } else { String::new() };
        let atts = self.render_attachments(key, &m.attachments, matches!(m.role, Role::User), cx);
        let outer = div().w_full().px(px(24.)).pt(top_gap);
        let (bpx, bpy) = (px(14.), pal.g(8.).max(px(8.)));
        match &m.role {
            Role::User => outer
                .flex()
                .flex_col()
                .items_end()
                .gap(px(4.))
                .when(!m.attachments.is_empty(), |d| d.child(atts))
                .when(!m.text.is_empty(), |d| {
                    d.child(div().max_w(relative(0.78)).px(bpx).py(bpy).rounded(px(18.)).rounded_br(px(6.)).bg(pal.accent).text_color(pal.on_accent).text_size(pal.t(14.)).child(body))
                })
                .when(!time.is_empty(), |d| d.child(div().pr(px(4.)).text_size(pal.t(11.)).text_color(pal.faint).child(time)))
                .into_any_element(),
            Role::Assistant => {
                let chips = (!m.activity.is_empty() && prefs.show_tool_activity).then(|| self.render_activity(&m, ix, cx));
                let has_bubble = !m.text.is_empty() || !m.attachments.is_empty() || (m.streaming && chips.is_none());
                outer
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .when_some(chips, |d, c| d.child(c))
                    .when(has_bubble, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap(px(12.))
                                .child(div().w(px(28.)).flex_none().when(!same || !m.activity.is_empty(), |d| d.child(avatar_for(bot.as_ref(), &key.profile, 28.))))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .items_start()
                                        .gap(px(4.))
                                        .when(!m.text.is_empty(), |d| {
                                            d.child(div().max_w(relative(0.9)).min_w_0().px(bpx).py(bpy).rounded(px(18.)).rounded_tl(px(6.)).bg(pal.bot_bubble).text_size(pal.t(14.)).child(body))
                                        })
                                        .when(m.streaming && m.text.is_empty(), |d| d.child(typing_dots(&pal)))
                                        .when(!m.attachments.is_empty(), |d| d.child(atts))
                                        .when(!m.streaming && !time.is_empty() && !m.text.is_empty(), |d| d.child(div().pl(px(4.)).text_size(pal.t(11.)).text_color(pal.faint).child(time))),
                                ),
                        )
                    })
                    .into_any_element()
            }
            Role::Agent(n) => {
                let (head, len) = self.agent_run(key, ix);
                if head != ix {
                    // Folded into the chip at the head of the run.
                    return div().into_any_element();
                }
                let sender = self.notice_sender(n);
                let name = sender.as_ref().map(|b| b.title()).unwrap_or_else(|| n.name.clone());
                let lead_text = if len == 1 { "Message from".to_owned() } else { format!("{len} messages with") };
                let expanded = self.expanded_events.contains(&m.id);
                let run: Vec<ChatMessage> = (head..head + len).map(|i| self.chats[key].msgs[i].m.clone()).collect();
                let last_ts = run.last().and_then(|r| r.timestamp);
                let chip = self.event_chip(&m.id, ix, Lead::None, lead_text, Some((Lead::Avatar(sender.clone().map(Box::new), name.clone()), name.clone())), last_ts, None, true, expanded, cx);
                let panel = expanded.then(|| {
                    let mut p = event_panel(&pal);
                    for (i, r) in run.iter().enumerate() {
                        let Role::Agent(rn) = &r.role else { continue };
                        p = p.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.))
                                .when(i > 0, |d| d.pt(px(8.)).border_t_1().border_color(pal.border))
                                .child(
                                    div()
                                        .h(px(20.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(pal.t(12.))
                                        .child(avatar_for(sender.as_ref(), &rn.name, 16.))
                                        .child(div().font_weight(FontWeight::MEDIUM).text_color(pal.text).child(name.clone()))
                                        .child(div().text_color(pal.faint).child(format!("@{}", rn.handle)))
                                        .child(div().flex_1())
                                        .child(div().text_color(pal.faint).child(fmt_clock(r.timestamp))),
                                )
                                .child(div().text_size(pal.t(13.)).text_color(pal.text).child(TextView::markdown(SharedString::from(format!("notice-{}", r.id)), rn.body.clone()).selectable(true))),
                        );
                        if !r.attachments.is_empty() {
                            p = p.child(self.render_attachments(key, &r.attachments, false, cx));
                        }
                    }
                    if let Some(b) = sender.clone() {
                        let k = b.key.clone();
                        p = p.child(
                            div().flex().justify_end().child(
                                button(SharedString::from(format!("open-sender-{}", m.id)), format!("Open {}", b.title()), Kind::Secondary, &pal)
                                    .h(px(28.))
                                    .on_click(cx.listener(move |this, _, window, cx| this.select(k.clone(), window, cx))),
                            ),
                        );
                    }
                    p
                });
                event_row(outer, chip, panel)
            }
            Role::System => {
                let lower = m.text.to_lowercase();
                let icon = if lower.contains("compact") {
                    "history"
                } else if lower.contains("model") {
                    "sparkles"
                } else if lower.contains("fail") || lower.contains("error") {
                    "circle-alert"
                } else {
                    "info"
                };
                let long = m.text.chars().count() > 64;
                let expanded = long && self.expanded_events.contains(&m.id);
                let short = if long { hermes_core::roster::one_line(&m.text, 64) } else { m.text.clone() };
                let chip = self.event_chip(&m.id, ix, Lead::Icon(icon), short, None, m.timestamp, None, long, expanded, cx);
                let panel = expanded.then(|| event_panel(&pal).child(div().text_size(pal.t(13.)).text_color(pal.text).child(m.text.clone())));
                event_row(outer, chip, panel)
            }
            Role::Local(ev) => {
                let expandable = ev.detail.is_some() || ev.code.is_some();
                let expanded = expandable && self.expanded_events.contains(&m.id);
                let icon: &'static str = match ev.icon.as_str() {
                    "download" => "download",
                    "shield-check" => "shield-check",
                    "shield-x" => "shield-x",
                    _ => "info",
                };
                let (text, object) = match m.text.split_once(" · ") {
                    Some((verb, obj)) => (format!("{verb} ·"), Some((Lead::None, obj.to_owned()))),
                    None => (m.text.clone(), None),
                };
                let chip = self.event_chip(&m.id, ix, Lead::Icon(icon), text, object, m.timestamp, None, expandable, expanded, cx);
                let mono = cx.theme().mono_font_family.clone();
                let panel = expanded.then(|| {
                    let path = ev.path.clone();
                    event_panel(&pal)
                        .when_some(ev.detail.clone(), |d, t| d.child(div().text_size(pal.t(13.)).text_color(pal.muted).child(t)))
                        .when_some(ev.code.clone(), |d, t| {
                            d.child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(pal.raised).font_family(mono).text_size(pal.t(12.)).text_color(pal.text).child(t))
                        })
                        .when_some(path, |d, p| {
                            d.child(div().flex().justify_end().child(
                                button(SharedString::from(format!("reveal-{}", m.id)), "Show in folder", Kind::Secondary, &pal).h(px(28.)).on_click(move |_, _, cx| cx.reveal_path(std::path::Path::new(&p))),
                            ))
                        })
                });
                event_row(outer, chip, panel)
            }
        }
    }

    /// The head and length of the run of consecutive agent messages from one sender around `ix`.
    fn agent_run(&self, key: &BotKey, ix: usize) -> (usize, usize) {
        let msgs = &self.chats[key].msgs;
        let handle = |i: usize| match &msgs[i].m.role {
            Role::Agent(n) => Some(n.handle.to_lowercase()),
            _ => None,
        };
        let Some(h) = handle(ix) else { return (ix, 1) };
        let mut head = ix;
        while head > 0 && handle(head - 1).as_ref() == Some(&h) {
            head -= 1;
        }
        let mut end = ix + 1;
        while end < msgs.len() && handle(end).as_ref() == Some(&h) {
            end += 1;
        }
        (head, end - head)
    }

    fn notice_sender(&self, n: &hermes_core::roster::AgentNotice) -> Option<Bot> {
        let tag = n.handle.split('@').next().unwrap_or("").to_lowercase();
        self.bots.iter().find(|b| b.tags().contains(&tag)).cloned()
    }

    /// A small, muted, centred one-line chip for something that happened around the conversation.
    /// The timestamp fades in on hover; clicking toggles the expanded panel.
    #[allow(clippy::too_many_arguments)]
    fn event_chip(
        &self,
        id: &str,
        ix: usize,
        lead: Lead,
        text: String,
        object: Option<(Lead, String)>,
        ts: Option<f64>,
        state: Option<ToolState>,
        expandable: bool,
        expanded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let pal = self.pal;
        let group = SharedString::from(format!("evg-{id}"));
        let time = fmt_clock(ts);
        let id_owned = id.to_owned();
        let glyph = |l: Lead| -> Option<AnyElement> {
            match l {
                Lead::None => None,
                Lead::Icon(name) => Some(ico(name, 14., pal.muted).into_any_element()),
                Lead::Avatar(b, fallback) => Some(avatar_for(b.as_deref(), &fallback, 16.).into_any_element()),
            }
        };
        let (hover, pressed) = (pal.hover, pal.pressed);
        let chip = div()
            .id(SharedString::from(format!("ev-{id}")))
            .h(px(28.))
            .max_w(px(560.))
            .min_w_0()
            .px(px(10.))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .rounded_full()
            .border_1()
            .border_color(pal.border)
            .when(expanded, |d| d.bg(pal.hover))
            .text_size(pal.t(12.))
            .text_color(pal.muted)
            .when_some(glyph(lead), |d, g| d.child(g))
            .child(div().flex_none().child(text))
            .when_some(object, |d, (l, name)| {
                d.when_some(glyph(l), |d, g| d.child(g)).child(div().min_w_0().truncate().font_weight(FontWeight::MEDIUM).text_color(pal.text).child(name))
            })
            .when_some(state, |d, st| match st {
                ToolState::Running => d.child(
                    ico("loader-circle", 12., pal.muted)
                        .with_animation(SharedString::from(format!("evspin-{id}")), Animation::new(std::time::Duration::from_millis(900)).repeat(), |s, t| s.with_transformation(Transformation::rotate(percentage(t)))),
                ),
                ToolState::Failed => d.child(ico("x", 12., pal.danger)),
                ToolState::Done => d,
            })
            .when(expandable, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .active(move |s| s.bg(pressed))
                    .child(ico("chevron-down", 12., pal.faint).when(expanded, |s| s.with_transformation(Transformation::rotate(percentage(0.5)))))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_event(&id_owned, ix, cx)))
            });
        // The time hangs off the chip's right edge so the chip itself stays centred.
        div()
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .child(chip)
            .when(!time.is_empty() && self.config.prefs.show_timestamps, |d| {
                d.child(div().absolute().left_full().pl(px(8.)).text_size(pal.t(11.)).text_color(pal.faint).whitespace_nowrap().opacity(0.).group_hover(group, |s| s.opacity(1.)).child(time))
            })
            .into_any_element()
    }

    /// Tool calls and subagents for one turn: one chip per call, or a single summary chip
    /// when there are several, expanding to the full list.
    fn render_activity(&self, m: &ChatMessage, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let pal = self.pal;
        let acts = &m.activity;
        let id = format!("act-{}", m.id);
        let expanded = self.expanded_events.contains(&id);
        let label = |a: &Activity| if a.is_subagent { format!("Subagent · {}", a.title) } else { a.title.clone() };
        let running = acts.iter().find(|a| a.state == ToolState::Running);
        let failed = acts.iter().any(|a| a.state == ToolState::Failed);
        let state = if running.is_some() {
            Some(ToolState::Running)
        } else if failed {
            Some(ToolState::Failed)
        } else {
            Some(ToolState::Done)
        };
        let (icon, text, expandable) = if acts.len() == 1 {
            let a = &acts[0];
            (hermes_core::transcript::tool_icon(&a.name, a.is_subagent), label(a), a.detail.is_some() || a.summary.is_some() || a.duration_s.is_some())
        } else if let Some(r) = running {
            let done = acts.iter().filter(|a| a.state != ToolState::Running).count();
            (hermes_core::transcript::tool_icon(&r.name, r.is_subagent), format!("{} · {done}/{}", label(r), acts.len()), true)
        } else {
            let subs = acts.iter().filter(|a| a.is_subagent).count();
            let tools = acts.len() - subs;
            let plural = |n: usize, w: &str| if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") };
            let text = match (tools, subs) {
                (t, 0) => format!("Used {}", plural(t, "tool")),
                (0, s) => format!("Ran {}", plural(s, "subagent")),
                (t, s) => format!("Used {} and {}", plural(t, "tool"), plural(s, "subagent")),
            };
            (if tools == 0 { "users" } else { "wrench" }, text, true)
        };
        // "Updated routine · News watch": the object after the separator is emphasised.
        let (text, object) = match text.split_once(" · ") {
            Some((verb, obj)) if acts.len() == 1 => (format!("{verb} ·"), Some((Lead::None, obj.to_owned()))),
            _ => (text, None),
        };
        let chip = self.event_chip(&id, ix, Lead::Icon(icon), text, object, m.timestamp, state, expandable, expanded, cx);
        let panel = expanded.then(|| {
            event_panel(&pal).children(acts.iter().map(|a| {
                let (si, sc) = match a.state {
                    ToolState::Running => ("loader-circle", pal.muted),
                    ToolState::Done => ("check", pal.ok),
                    ToolState::Failed => ("x", pal.danger),
                };
                div()
                    .min_h(px(32.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().size(px(24.)).flex_none().rounded(px(6.)).bg(pal.hover).flex().items_center().justify_center().child(ico(hermes_core::transcript::tool_icon(&a.name, a.is_subagent), 12., pal.muted)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(pal.t(13.)).text_color(pal.text).truncate().child(label(a)))
                            .when_some(a.detail.clone().or(a.summary.clone()), |d, t| d.child(div().text_size(pal.t(12.)).text_color(pal.muted).truncate().child(t))),
                    )
                    .when_some(a.duration_s, |d, s| d.child(div().flex_none().text_size(pal.t(11.)).text_color(pal.faint).child(format!("{s:.1}s"))))
                    .child(ico(si, 14., sc))
            }))
        });
        div().w_full().flex().flex_col().items_center().gap(px(6.)).child(chip).when_some(panel, |d, p| d.child(p)).into_any_element()
    }

    fn render_attachments(&mut self, key: &BotKey, atts: &[Attachment], mine: bool, cx: &mut Context<Self>) -> Div {
        let pal = self.pal;
        let previews = self.config.prefs.image_previews;
        let mut row = div().flex().flex_wrap().gap(px(8.)).when(mine, |d| d.justify_end());
        for (i, a) in atts.iter().enumerate() {
            let local = a.local_path.clone().map(std::path::PathBuf::from).or_else(|| a.remote_path.as_ref().and_then(|r| self.media.get(r).cloned()));
            let id = SharedString::from(format!("att-{}-{}-{}", key.profile, a.remote_path.as_deref().or(a.local_path.as_deref()).unwrap_or(&a.name), i));
            let (k1, a1) = (key.clone(), a.clone());
            let (k2, a2) = (key.clone(), a.clone());
            let (k3, a3) = (key.clone(), a.clone());
            if a.kind == AttachmentKind::Image && previews {
                row = row.child(
                    div()
                        .id(id)
                        .rounded(px(12.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(|s| s.opacity(0.92))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_attachment(k1.clone(), &a1, cx)))
                        .child(match local {
                            Some(p) => img(p).max_w(px(360.)).max_h(px(260.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => skeleton(SharedString::from(format!("imgsk-{i}-{}", a.name)), px(240.).into(), 160., &pal).into_any_element(),
                        }),
                );
            } else {
                row = row.child(
                    div()
                        .id(id)
                        .h(px(56.))
                        .w(px(280.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .pl(px(12.))
                        .pr(px(8.))
                        .rounded(px(12.))
                        .bg(pal.panel)
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(move |s| s.bg(pal.raised))
                        .on_click(cx.listener(move |this, _, _, cx| this.open_attachment(k3.clone(), &a3, cx)))
                        .child(div().size(px(32.)).flex_none().rounded(px(8.)).bg(pal.accent_soft).flex().items_center().justify_center().child(ico("file-text", 16., pal.accent)))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().text_size(pal.t(13.)).font_weight(FontWeight::MEDIUM).truncate().text_color(pal.text).child(a.name.clone()))
                                .child(div().text_size(pal.t(11.)).text_color(pal.faint).truncate().child(a.size.map(fmt_size).unwrap_or_else(|| a.mime.clone()))),
                        )
                        .child(icon_btn(SharedString::from(format!("open-{i}-{}", a.name)), "external-link", &pal).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_attachment(k1.clone(), &a1, cx)
                        })))
                        .child(icon_btn(SharedString::from(format!("save-{i}-{}", a.name)), "download", &pal).on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.save_attachment(k2.clone(), &a2, cx)
                        }))),
                );
            }
        }
        row
    }

    fn render_footer(&mut self, key: &BotKey, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let chat = self.chats.get(key);
        let awaiting = chat.is_some_and(|c| c.awaiting);
        let error = chat.and_then(|c| c.error.clone());
        let loading = chat.is_none_or(|c| c.msgs.is_empty() && !c.loaded);
        let empty = chat.is_some_and(|c| c.msgs.is_empty() && c.loaded);
        let approvals: Vec<_> = self.approvals.iter().filter(|a| a.key.as_ref().is_none_or(|k| k == key)).map(|a| (a.request_id.clone(), a.command.clone(), a.description.clone(), a.choices.clone())).collect();
        let bot = self.bot(key).cloned();
        let is_thread = key.thread.is_some();
        div()
            .w_full()
            .px(px(24.))
            .pt(px(8.))
            .pb(px(16.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .when(loading, |d| d.child(chat_skeleton(&pal)))
            .when(empty, |d| {
                d.child(
                    div()
                        .py(px(64.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(8.))
                        .child(avatar_for(bot.as_ref(), &key.profile, 64.))
                        .child(div().pt(px(4.)).text_size(pal.t(18.)).font_weight(FontWeight::SEMIBOLD).child(bot.as_ref().map(|b| b.title()).unwrap_or_default()))
                        .child(
                            div()
                                .max_w(px(420.))
                                .text_center()
                                .text_size(pal.t(13.))
                                .text_color(pal.muted)
                                .child(if is_thread { "This thread has no messages.".to_owned() } else { bot.as_ref().and_then(|b| b.description.clone()).filter(|d| !d.is_empty()).unwrap_or_else(|| "Say hello to start this Bot Chat.".into()) }),
                        )
                        .child(div().pt(px(8.)).flex().gap(px(8.)).children(["What can you do?", "Summarise my day"].into_iter().enumerate().map(|(i, s)| {
                            chip(SharedString::from(format!("starter-{i}")), s, false, &pal).on_click(cx.listener(move |this, _, window, cx| {
                                this.composer.update(cx, |st, cx| {
                                    st.set_value(s, window, cx);
                                    st.focus(window, cx);
                                });
                            }))
                        }))),
                )
            })
            .when(awaiting, |d| d.child(div().flex().gap(px(12.)).child(div().w(px(28.)).child(avatar_for(bot.as_ref(), &key.profile, 28.))).child(typing_dots(&pal))))
            .children(approvals.into_iter().map(|(id, command, description, choices)| {
                div()
                    .p(px(16.))
                    .rounded(px(12.))
                    .bg(pal.panel)
                    .border_1()
                    .border_color(pal.accent)
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().flex().items_center().gap(px(8.)).child(ico("shield", 16., pal.accent)).child(div().text_size(pal.t(14.)).font_weight(FontWeight::SEMIBOLD).child("Hermes wants to run a command")))
                    .when(!description.is_empty(), |d| d.child(div().text_size(pal.t(13.)).text_color(pal.muted).child(description)))
                    .child(div().px(px(12.)).py(px(8.)).rounded(px(8.)).bg(pal.raised).font_family(cx.theme().mono_font_family.clone()).text_size(pal.t(13.)).child(command))
                    .child(div().pt(px(4.)).flex().flex_wrap().gap(px(8.)).children(choices.into_iter().map(|c| {
                        let (id2, c2) = (id.clone(), c.clone());
                        let label = match c.as_str() {
                            "once" => "Allow once",
                            "session" => "Allow this session",
                            "always" => "Always allow",
                            "deny" => "Deny",
                            other => other,
                        }
                        .to_owned();
                        let kind = match c.as_str() {
                            "once" => Kind::Primary,
                            "deny" => Kind::Danger,
                            _ => Kind::Secondary,
                        };
                        button(SharedString::from(format!("ap-{id}-{c}")), label, kind, &pal).on_click(cx.listener(move |this, _, _, cx| this.approve(&id2, &c2, cx)))
                    })))
            }))
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(10.))
                        .bg(pal.danger_soft)
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(ico("circle-alert", 16., pal.danger))
                        .child(div().flex_1().min_w_0().text_color(pal.danger).text_size(pal.t(13.)).child(e)),
                )
            })
    }

    fn render_composer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let busy = self.current_chat().is_some_and(|c| c.streaming || c.awaiting);
        let pending = self.pending.clone();
        let mention_rows: Vec<(usize, Bot)> = self.mention.as_ref().map(|m| m.items.iter().enumerate().filter_map(|(i, k)| self.bot(k).cloned().map(|b| (i, b))).collect()).unwrap_or_default();
        let sel = self.mention.as_ref().map(|m| m.sel).unwrap_or(0);
        let (at, cursor) = self.mention.as_ref().map(|m| (m.at, m.cursor)).unwrap_or((0, 0));
        let view = cx.entity().downgrade();
        let can_send = busy || !self.composer.read(cx).value().trim().is_empty() || !pending.is_empty();
        let placeholder_bot = self.current_bot().map(|b| b.title()).unwrap_or_default();
        let _ = placeholder_bot;
        div().flex_none().w_full().flex().justify_center().px(px(24.)).pb(px(16.)).pt(px(4.)).child(
            div()
                .w_full()
                .max_w(px(CONTENT_MAX_W - 48.))
                .relative()
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                    if this.mention.is_none() {
                        return;
                    }
                    match ev.keystroke.key.as_str() {
                        "up" => this.move_mention(-1, cx),
                        "down" => this.move_mention(1, cx),
                        "tab" => this.complete_mention(window, cx),
                        "escape" => {
                            this.mention = None;
                            cx.notify();
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                }))
                // @mention autocomplete.
                .when(!mention_rows.is_empty(), |d| {
                    d.child(
                        div()
                            .absolute()
                            .bottom_full()
                            .left_0()
                            .mb(px(8.))
                            .w(px(320.))
                            .p(px(4.))
                            .rounded(px(12.))
                            .bg(pal.panel)
                            .border_1()
                            .border_color(pal.border)
                            .shadow_lg()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().px(px(8.)).child(section_label("MENTION A BOT", &pal)))
                            .children(mention_rows.into_iter().map(|(i, b)| {
                                let key = b.key.clone();
                                let tag = self.tag_for(&b.key);
                                div()
                                    .id(SharedString::from(format!("m-{}-{}", b.key.conn, b.key.profile)))
                                    .h(px(40.))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .px(px(8.))
                                    .rounded(px(8.))
                                    .cursor_pointer()
                                    .when(i == sel, |d| d.bg(pal.selected))
                                    .hover(move |s| s.bg(pal.hover))
                                    .on_click(cx.listener(move |this, _, window, cx| this.pick_mention(at, cursor, key.clone(), window, cx)))
                                    .child(avatar_for(Some(&b), &b.name, 24.))
                                    .child(div().flex_1().min_w_0().truncate().text_size(pal.t(13.)).font_weight(FontWeight::MEDIUM).child(b.title()))
                                    .child(div().flex_none().text_size(pal.t(12.)).text_color(pal.muted).child(format!("@{tag}")))
                            })),
                    )
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .p(px(8.))
                        .rounded(px(24.))
                        .bg(pal.input_bg)
                        .border_1()
                        .border_color(if self.dragging { pal.accent } else { pal.border })
                        .shadow_sm()
                        .when(!pending.is_empty(), |d| {
                            d.child(div().flex().flex_wrap().gap(px(8.)).px(px(4.)).pt(px(4.)).children(pending.into_iter().enumerate().map(|(i, f)| {
                                let path = f.path.clone();
                                div()
                                    .h(px(40.))
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .pl(px(4.))
                                    .pr(px(4.))
                                    .rounded(px(10.))
                                    .bg(pal.selected)
                                    .child(if f.mime.starts_with("image/") {
                                        img(f.path.clone()).size(px(32.)).rounded(px(6.)).object_fit(ObjectFit::Cover).into_any_element()
                                    } else {
                                        div().size(px(32.)).rounded(px(6.)).bg(pal.accent_soft).flex().items_center().justify_center().child(ico("file-text", 16., pal.accent)).into_any_element()
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(div().max_w(px(160.)).truncate().text_size(pal.t(12.)).font_weight(FontWeight::MEDIUM).child(f.name.clone()))
                                            .child(div().text_size(pal.t(11.)).text_color(pal.faint).child(fmt_size(f.size))),
                                    )
                                    .child(icon_btn_sized(SharedString::from(format!("rm-{i}")), "x", 24., 14., &pal).on_click(cx.listener(move |this, _, _, cx| {
                                        this.pending.retain(|p| p.path != path);
                                        cx.notify();
                                    })))
                            })))
                        })
                        // One row: every control is 32 px and bottom-aligned, so they stay
                        // centred on a one-line input and pinned to the last line when it grows.
                        .child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(4.))
                                .child(composer_btn("attach", "plus", &pal).on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))))
                                .child(
                                    div().flex_1().min_w_0().min_h(px(32.)).flex().items_center().px(px(4.)).text_size(pal.t(14.)).child(
                                        Textarea::new(&self.composer).appearance(false).on_paste(move |item, _, cx| {
                                            let mut handled = false;
                                            for entry in item.entries() {
                                                match entry {
                                                    ClipboardEntry::ExternalPaths(p) => {
                                                        let paths = p.paths().to_vec();
                                                        let _ = view.update(cx, |app, cx| app.add_paths(&paths, cx));
                                                        handled = true;
                                                    }
                                                    ClipboardEntry::Image(image) => {
                                                        let ext = match image.format {
                                                            ImageFormat::Jpeg => "jpg",
                                                            ImageFormat::Gif => "gif",
                                                            ImageFormat::Webp => "webp",
                                                            _ => "png",
                                                        };
                                                        let bytes = image.bytes.clone();
                                                        let _ = view.update(cx, |app, cx| app.add_image_bytes(&bytes, ext, cx));
                                                        handled = true;
                                                    }
                                                    _ => {}
                                                }
                                            }
                                            handled
                                        }),
                                    ),
                                )
                                .child(composer_btn("mic", "mic", &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.show_toast("Voice input isn't available yet: Hermes' voice.* gateway methods are not served on remote connections.", false, cx);
                                })))
                                .child(
                                    div()
                                        .id("send")
                                        .size(px(32.))
                                        .flex_none()
                                        .rounded_full()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .cursor_pointer()
                                        .bg(if can_send { pal.accent } else { pal.border_strong })
                                        .when(can_send, |d| d.hover(|s| s.opacity(0.88)).active(|s| s.opacity(0.75)))
                                        .child(ico(if busy { "square" } else { "arrow-up" }, 16., pal.on_accent))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            if busy && this.composer.read(cx).value().trim().is_empty() {
                                                this.stop(cx)
                                            } else {
                                                this.send(window, cx)
                                            }
                                        })),
                                ),
                        ),
                ),
        )
    }

    fn render_info(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.info_open {
            return None;
        }
        let pal = self.pal;
        let bot = self.current_bot().cloned()?;
        let info = self.info.as_ref();
        let editing = info.is_some_and(|i| i.edit.is_some());
        let describe = info.and_then(|i| i.describe.clone());
        let threads = info.and_then(|i| i.threads.clone());
        let current_thread = self.selected.as_ref().and_then(|k| k.thread.clone());
        let hidden = bot.hidden();
        let key = bot.key.clone();

        let mut body = div().flex().flex_col().gap(px(24.)).p(px(16.));
        if let Some(e) = info.and_then(|i| i.edit.as_ref()) {
            let sel_color = e.color;
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    
                    .child(field("Display name", &e.title, &pal))
                    .child(field("Description", &e.description, &pal))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(div().text_size(pal.t(12.)).text_color(pal.muted).child("Colour"))
                            .child(div().flex().flex_wrap().gap(px(8.)).children(ACCENTS.iter().map(|(c, name)| {
                                let c = *c;
                                swatch(SharedString::from(format!("bc-{name}")), c, sel_color == Some(c), &pal).on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(e) = this.info.as_mut().and_then(|i| i.edit.as_mut()) {
                                        e.color = Some(c);
                                    }
                                    cx.notify();
                                }))
                            }))),
                    )
                    .child(div().text_size(pal.t(12.)).text_color(pal.faint).child("Name and colour are Bot Mode metadata; the description is saved to the Hermes profile."))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(button("edit-cancel", "Cancel", Kind::Secondary, &pal).on_click(cx.listener(|this, _, _, cx| {
                                if let Some(i) = &mut this.info {
                                    i.edit = None;
                                }
                                cx.notify();
                            })))
                            .child(button("edit-save", "Save", Kind::Primary, &pal).on_click(cx.listener(|this, _, _, cx| this.save_edit(cx)))),
                    ),
            );
        } else {
            let k1 = key.clone();
            body = body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(4.))
                        .pt(px(8.))
                        .child(avatar_for(Some(&bot), &bot.name, 64.))
                        .child(div().pt(px(8.)).text_size(pal.t(17.)).font_weight(FontWeight::SEMIBOLD).text_center().child(bot.title()))
                        .child(div().text_size(pal.t(13.)).text_color(pal.muted).child(format!("@{}", bot.handle())))
                        .child(
                            div()
                                .pt(px(12.))
                                .flex()
                                .gap(px(8.))
                                .child(button("info-edit", "Edit", Kind::Secondary, &pal).child(ico("pencil", 14., pal.text)).flex_row_reverse().on_click(cx.listener(|this, _, window, cx| this.start_edit(window, cx))))
                                .child(button("info-apps", "Apps", Kind::Secondary, &pal).child(ico("layout-grid", 14., pal.text)).flex_row_reverse().on_click(cx.listener(|this, _, window, cx| this.open_apps(window, cx))))
                                .child(
                                    button("info-hide", if hidden { "Unhide" } else { "Hide" }, Kind::Secondary, &pal)
                                        .child(ico(if hidden { "eye" } else { "eye-off" }, 14., pal.text))
                                        .flex_row_reverse()
                                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_hidden(k1.clone(), cx))),
                                ),
                        ),
                )
                .child(
                    info_group("About", &pal)
                        .child(div().text_size(pal.t(13.)).text_color(if bot.description.as_deref().unwrap_or("").is_empty() { pal.faint } else { pal.text }).child(bot.description.clone().filter(|d| !d.is_empty()).unwrap_or_else(|| "No description yet.".into())))
                        .child(kv("Model", bot.model.clone().unwrap_or_else(|| "Profile default".into()), &pal))
                        .child(kv("Profile", bot.name.clone(), &pal))
                        .when(self.config.connections.len() > 1, |d| d.child(kv("Server", bot.conn_name.clone(), &pal))),
                )
                .child(match &describe {
                    None => info_group("Capabilities", &pal).child(skeleton("cap-sk", relative(1.).into(), 16., &pal)).child(skeleton("cap-sk2", relative(0.6).into(), 16., &pal)),
                    Some(Err(e)) => info_group("Capabilities", &pal).child(div().text_size(pal.t(12.)).text_color(pal.faint).child(e.clone())),
                    Some(Ok(v)) => {
                        let count = |k: &str| v.get(k).and_then(|x| x.as_array()).map(|a| (a.iter().filter(|x| x.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true)).count(), a.len())).unwrap_or((0, 0));
                        let (te, tt) = count("toolsets");
                        let (se, _) = count("skills");
                        let (me, _) = count("mcp_servers");
                        info_group("Capabilities", &pal)
                            .child(kv("Toolsets", format!("{te} of {tt} on"), &pal))
                            .child(kv("Skills", se.to_string(), &pal))
                            .child(kv("MCP servers", me.to_string(), &pal))
                    }
                })
                .child({
                    let mut g = info_group("Threads", &pal);
                    let on_chat = current_thread.is_none();
                    g = g.child(thread_row("thread-canonical", "Bot Chat", "The main conversation", "", on_chat, &pal).on_click(cx.listener(|this, _, window, cx| this.run_act(Act::BotChat, window, cx))));
                    match &threads {
                        None => g = g.child(skeleton("th-sk", relative(1.).into(), 40., &pal)),
                        Some(Err(e)) => g = g.child(div().text_size(pal.t(12.)).text_color(pal.faint).child(e.clone())),
                        Some(Ok(rows)) if rows.is_empty() => g = g.child(div().py(px(4.)).text_size(pal.t(12.)).text_color(pal.faint).child("No other conversations with this bot yet.")),
                        Some(Ok(rows)) => {
                            for r in rows.iter() {
                                let r2 = r.clone();
                                let active = current_thread.as_deref() == Some(r.id.as_str());
                                let head = match r.count { 0 => r.source.clone(), 1 => format!("{} · 1 msg", r.source), n => format!("{} · {n} msgs", r.source) };
                                let sub = if self.config.prefs.hide_previews || r.preview.is_empty() { head } else { format!("{head} · {}", r.preview) };
                                g = g.child(
                                    thread_row(SharedString::from(format!("thread-{}", r.id)), &r.title, &sub, &fmt_time(Some(r.last)), active, &pal)
                                        .on_click(cx.listener(move |this, _, window, cx| this.open_thread(&r2, window, cx))),
                                );
                            }
                        }
                    }
                    g
                });
        }

        Some(
            div()
                .w(px(INFO_W))
                .flex_none()
                .h_full()
                .flex()
                .flex_col()
                .bg(pal.sidebar)
                .border_l_1()
                .border_color(pal.border)
                .child(
                    div()
                        .h(px(TOP_INSET.max(8.) + 32.).max(px(56.)))
                        .flex_none()
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .border_b_1()
                        .border_color(pal.border)
                        .child(div().text_size(pal.t(14.)).font_weight(FontWeight::SEMIBOLD).child(if editing { "Edit bot" } else { "Bot info" }))
                        .child(icon_btn("info-close", "x", &pal).on_click(cx.listener(|this, _, window, cx| this.toggle_info(window, cx)))),
                )
                .child(div().id("info-scroll").flex_1().min_h_0().overflow_y_scroll().child(body)),
        )
    }
}

fn fmt_clock(ts: Option<f64>) -> String {
    let Some(t) = ts.and_then(|t| chrono::DateTime::from_timestamp(t as i64, 0)) else { return String::new() };
    let local = t.with_timezone(&chrono::Local);
    if local.date_naive() == chrono::Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%-d %b, %H:%M").to_string()
    }
}

fn composer_btn(id: &'static str, name: &'static str, pal: &Palette) -> Stateful<Div> {
    let (hover, pressed) = (pal.hover, pal.pressed);
    div()
        .id(id)
        .size(px(32.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(pressed))
        .child(ico(name, 18., pal.muted))
}

use crate::theme::Palette;

pub fn field(label: &'static str, input: &Entity<gpui_component::input::InputState>, pal: &Palette) -> Div {
    div().flex().flex_col().gap(px(8.)).child(div().text_size(pal.t(12.)).font_weight(FontWeight::MEDIUM).text_color(pal.muted).child(label)).child(Input::new(input))
}

pub fn swatch(id: SharedString, c: u32, on: bool, pal: &Palette) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex_none()
        .rounded_full()
        .p(px(3.))
        .border_2()
        .border_color(if on { pal.text } else { rgba(0) })
        .cursor_pointer()
        .child(div().size_full().rounded_full().bg(rgb(c)))
}

fn info_group(title: &'static str, pal: &Palette) -> Div {
    div().flex().flex_col().gap(px(8.)).child(section_label(title.to_uppercase(), pal))
}

fn kv(k: &'static str, v: String, pal: &Palette) -> Div {
    div()
        .h(px(24.))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.))
        .text_size(pal.t(13.))
        .child(div().flex_none().text_color(pal.muted).child(k))
        .child(div().min_w_0().truncate().child(v))
}

fn thread_row(id: impl Into<ElementId>, title: &str, sub: &str, time: &str, active: bool, pal: &Palette) -> Stateful<Div> {
    let (hover, pressed) = (pal.hover, pal.pressed);
    div()
        .id(id.into())
        .h(px(48.))
        .px(px(8.))
        .mx(px(-8.))
        .rounded(px(8.))
        .flex()
        .items_center()
        .gap(px(12.))
        .cursor_pointer()
        .when(active, |d| d.bg(pal.selected))
        .when(!active, move |d| d.hover(move |s| s.bg(hover)).active(move |s| s.bg(pressed)))
        .child(div().size(px(28.)).flex_none().rounded(px(8.)).bg(pal.raised).flex().items_center().justify_center().child(ico("messages-square", 14., pal.muted)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().flex().items_center().gap(px(8.)).child(div().flex_1().min_w_0().truncate().text_size(pal.t(13.)).font_weight(FontWeight::MEDIUM).child(title.to_owned())).child(div().flex_none().text_size(pal.t(11.)).text_color(pal.faint).child(time.to_owned())))
                .child(div().truncate().text_size(pal.t(12.)).text_color(pal.muted).child(sub.to_owned())),
        )
}

fn roster_skeleton(i: usize, pal: &Palette) -> impl IntoElement {
    div()
        .h(px(56.))
        .flex_none()
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().size(px(36.)).flex_none().rounded_full().bg(pal.skeleton))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(skeleton(SharedString::from(format!("rs-a{i}")), relative(0.5).into(), 12., pal))
                .child(skeleton(SharedString::from(format!("rs-b{i}")), relative(0.85 - i as f32 * 0.07).into(), 10., pal)),
        )
}

fn chat_skeleton(pal: &Palette) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .pt(px(24.))
        .child(div().flex().justify_end().child(skeleton("cs1", px(220.).into(), 40., pal)))
        .child(div().flex().gap(px(12.)).child(div().size(px(28.)).flex_none().rounded_full().bg(pal.skeleton)).child(skeleton("cs2", px(380.).into(), 72., pal)))
        .child(div().flex().justify_end().child(skeleton("cs3", px(160.).into(), 40., pal)))
        .child(div().flex().gap(px(12.)).child(div().size(px(28.)).flex_none().rounded_full().bg(pal.skeleton)).child(skeleton("cs4", px(300.).into(), 56., pal)))
}

fn typing_dots(pal: &Palette) -> impl IntoElement {
    div()
        .h(px(36.))
        .px(px(14.))
        .rounded(px(18.))
        .rounded_tl(px(6.))
        .bg(pal.bot_bubble)
        .flex()
        .items_center()
        .gap(px(4.))
        .children((0..3).map(|i| {
            div().size(px(6.)).rounded_full().bg(pal.muted).with_animation(ElementId::Name(format!("dot{i}").into()), Animation::new(std::time::Duration::from_millis(1200)).repeat(), move |d, t| {
                let phase = ((t + i as f32 * 0.2) % 1.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                d.opacity(0.3 + 0.7 * phase)
            })
        }))
}

impl Render for HermesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let view = cx.entity();
        // An empty chat is centred in the pane; anything else is the bottom-anchored list.
        let empty_key = self.selected.clone().filter(|k| {
            self.chats.get(k).is_some_and(|c| c.msgs.is_empty() && c.loaded && !c.awaiting && c.error.is_none()) && !self.approvals.iter().any(|a| a.key.as_ref() == Some(k))
        });
        let messages = match empty_key {
            Some(k) => div().id("empty").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().justify_center().items_center().child(div().w_full().max_w(px(CONTENT_MAX_W)).child(self.render_footer(&k, window, cx))).into_any_element(),
            None => list(self.list.clone(), move |ix, window, cx| view.update(cx, |this, cx| this.render_item(ix, window, cx))).flex_1().w_full().into_any_element(),
        };
        // Narrow windows: the info pane gives way to the conversation.
        let wide = window.viewport_size().width >= px(SIDEBAR_W + INFO_W + 420.);
        let info = if wide { self.render_info(cx).map(|e| e.into_any_element()) } else { None };
        let overlays = self.render_overlays(window, cx);
        div()
            .size_full()
            .relative()
            .flex()
            .bg(pal.bg)
            .text_color(pal.text)
            .text_size(pal.t(14.))
            .key_context("Hermes")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)))
            .on_action(cx.listener(|this, _: &NewBot, window, cx| this.open_new_bot(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|this, _: &OpenApps, window, cx| this.open_apps(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleInfo, window, cx| this.toggle_info(window, cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.searching = true;
                this.search.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextBot, window, cx| this.step_bot(1, window, cx)))
            .on_action(cx.listener(|this, _: &PrevBot, window, cx| this.step_bot(-1, window, cx)))
            .on_action(cx.listener(|this, _: &CopyConversation, _, cx| this.share(cx)))
            .on_action(cx.listener(|this, _: &StopTurn, _, cx| this.stop(cx)))
            .on_action(cx.listener(|this, _: &AttachFiles, _, cx| this.pick_files(cx)))
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.key == "escape" && this.mention.is_none() && this.close_top(window, cx) {
                    cx.stop_propagation();
                } else if k.modifiers.secondary(cx) && !k.modifiers.shift && !k.modifiers.alt && k.key.len() == 1 && !this.any_overlay() {
                    if let Some(d) = k.key.chars().next().and_then(|c| c.to_digit(10)).filter(|d| *d >= 1) {
                        this.select_nth(d as usize - 1, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .on_drag_move(cx.listener(|this, _: &DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.dragging {
                    this.dragging = true;
                    cx.notify();
                }
            }))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.dragging = false;
                this.add_paths(paths.paths(), cx);
            }))
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().flex().flex_col().child(self.render_header(cx)).child(messages).child(self.render_composer(cx)))
            .children(info)
            .when_some(self.toast.clone(), |d, (msg, err)| {
                d.child(
                    div().absolute().bottom(px(96.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .px(px(16.))
                            .py(px(8.))
                            .rounded(px(12.))
                            .bg(if pal.dark { pal.raised } else { rgb(0x18181b) })
                            .text_color(if pal.dark { pal.text } else { rgb(0xfafafa) })
                            .border_1()
                            .border_color(if err { pal.danger } else { pal.border })
                            .shadow_lg()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_size(pal.t(13.))
                            .max_w(px(560.))
                            .when(err, |d| d.child(ico("circle-alert", 16., pal.danger)))
                            .child(msg),
                    ),
                )
            })
            .children(overlays)
    }
}
