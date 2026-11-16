//! Rendering: sidebar roster, chat pane, composer, overlays.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, Textarea};
use gpui_component::text::{TextView, TextViewState};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{Bot, BotKey};
use hermes_core::transcript::{Activity, Attachment, AttachmentKind, Role, ToolState};

use crate::app::HermesApp;
use crate::assets::icon;
use crate::theme::{avatar_color, Palette};

const SIDEBAR_W: f32 = 290.;

fn ico(name: &'static str, size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(icon(name)).size(px(size)).flex_none().text_color(color.into())
}

fn icon_button(id: impl Into<ElementId>, name: &'static str, pal: &Palette) -> Stateful<Div> {
    let hover = pal.hover;
    div()
        .id(id.into())
        .size(px(30.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .child(ico(name, 16., pal.muted))
}

fn pill_button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool, pal: &Palette) -> Stateful<Div> {
    let (bg, fg) = if primary { (pal.accent, pal.on_accent) } else { (pal.selected, pal.text) };
    div()
        .id(id.into())
        .px(px(14.))
        .py(px(7.))
        .rounded(px(9.))
        .bg(bg)
        .text_color(fg)
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .hover(|s| s.opacity(0.88))
        .child(label.into())
}

pub fn avatar_for(bot: Option<&Bot>, fallback: &str, size: f32) -> Div {
    let (color, initials) = match bot {
        Some(b) => (avatar_color(b.color_hex(), b.color_seed()), b.initials()),
        None => (avatar_color(None, fallback.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))), fallback.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or("?".into())),
    };
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(color)
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px((size * 0.4).max(10.)))
        .font_weight(FontWeight::SEMIBOLD)
        .child(initials)
}

fn fmt_time(ts: Option<f64>) -> String {
    let Some(ts) = ts else { return String::new() };
    let Some(t) = chrono::DateTime::from_timestamp(ts as i64, 0) else { return String::new() };
    let local = t.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else if (now - local).num_days() < 7 {
        local.format("%a %H:%M").to_string()
    } else {
        local.format("%-d %b %H:%M").to_string()
    }
}

fn fmt_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

/// Post an OS notification without extra dependencies. macOS uses `osascript`, Linux uses
/// `notify-send` (freedesktop), and Windows is not implemented yet.
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
        let _ = (title, body);
    }
}

impl HermesApp {
    fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let current = self.selected.as_ref().and_then(|k| self.bot(k)).cloned();
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

        div()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(pal.sidebar)
            .border_r_1()
            .border_color(pal.border)
            // Top: current bot, search, new.
            .child(
                div()
                    .pt(px(if cfg!(target_os = "macos") { 40. } else { 12. }))
                    .px(px(14.))
                    .pb(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(avatar_for(current.as_ref(), "H", 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(current.as_ref().map(|b| b.title()).unwrap_or_else(|| "Hermes".into())),
                    )
                    .child(icon_button("search", "search", &pal).on_click(cx.listener(|this, _, window, cx| {
                        this.searching = !this.searching;
                        if this.searching {
                            this.search.update(cx, |s, cx| s.focus(window, cx));
                        } else {
                            this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        }
                        cx.notify();
                    })))
                    .when(any_hidden, |d| {
                        d.child(
                            div()
                                .relative()
                                .child(icon_button("hidden", if self.show_hidden { "eye" } else { "eye-off" }, &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.show_hidden = !this.show_hidden;
                                    cx.notify();
                                })))
                                .when(hidden_unread, |d| d.child(div().absolute().top(px(4.)).right(px(4.)).size(px(7.)).rounded_full().bg(pal.accent))),
                        )
                    })
                    .child(icon_button("new-bot", "plus", &pal).on_click(cx.listener(|this, _, window, cx| this.open_new_bot(window, cx)))),
            )
            .when(self.searching, |d| d.child(div().px(px(12.)).pb(px(8.)).child(Input::new(&self.search))))
            .child(div().px(px(16.)).pt(px(4.)).pb(px(6.)).text_size(px(11.)).text_color(pal.faint).font_weight(FontWeight::SEMIBOLD).child("BOTS"))
            // Roster.
            .child(
                div()
                    .id("roster")
                    .flex_1()
                    .overflow_y_scroll()
                    .px(px(8.))
                    .children(rows.into_iter().map(|b| self.render_row(b, window, cx)))
                    .when(self.bots.is_empty(), |d| {
                        d.child(div().p(px(16.)).text_size(px(13.)).text_color(pal.faint).child(if self.config.connections.is_empty() { "Connect to your Hermes to see your bots." } else { "Loading bots…" }))
                    }),
            )
            // Account.
            .child(
                div()
                    .border_t_1()
                    .border_color(pal.border)
                    .px(px(14.))
                    .py(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(avatar_for(None, &user, 30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(user))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .child(div().size(px(7.)).rounded_full().bg(if link_ok { pal.ok } else { pal.faint }))
                                    .child(div().text_size(px(11.)).text_color(pal.muted).truncate().child(link_label)),
                            ),
                    )
                    .child(icon_button("settings", "settings", &pal).on_click(cx.listener(|this, _, window, cx| {
                        let id = this.config.connections.first().map(|c| c.id.clone());
                        this.open_setup(id, window, cx);
                    }))),
            )
    }

    fn render_row(&mut self, b: Bot, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let selected = self.selected.as_ref() == Some(&b.key);
        let unread = self.unread.contains(&b.key);
        let typing = self.chats.get(&b.key).is_some_and(|c| c.streaming || c.awaiting);
        let key = b.key.clone();
        let multi = self.config.connections.len() > 1;
        let preview = if typing { "typing…".to_owned() } else { b.preview() };
        div()
            .id(SharedString::from(format!("row-{}-{}", b.key.conn, b.key.profile)))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(8.))
            .py(px(8.))
            .rounded(px(10.))
            .cursor_pointer()
            .when(selected, |d| d.bg(pal.selected))
            .when(!selected, |d| d.hover(move |s| s.bg(pal.hover)))
            .when(b.hidden(), |d| d.opacity(0.5))
            .on_click(cx.listener(move |this, _, window, cx| this.select(key.clone(), window, cx)))
            .child(avatar_for(Some(&b), &b.name, 38.))
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
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(14.)).font_weight(if unread { FontWeight::BOLD } else { FontWeight::MEDIUM }).child(b.title()))
                            .when(multi, |d| d.child(div().text_size(px(10.)).text_color(pal.faint).child(b.conn_name.clone())))
                            .child(div().text_size(px(11.)).text_color(pal.faint).child(fmt_time(Some(b.last_active()).filter(|t| *t > 0.0)))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().flex_1().min_w_0().truncate().text_size(px(12.5)).text_color(if unread { pal.text } else { pal.muted }).child(if preview.is_empty() { "No messages yet".to_owned() } else { preview }))
                            .when(unread, |d| d.child(div().size(px(9.)).rounded_full().bg(pal.accent))),
                    ),
            )
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let bot = self.selected.as_ref().and_then(|k| self.bot(k)).cloned();
        let chat = self.current_chat();
        let sub = match (&bot, chat) {
            (_, Some(c)) if c.streaming || c.awaiting => c.status.clone().unwrap_or_else(|| "thinking…".into()),
            (Some(b), _) => {
                let mut s = format!("@{}", b.handle());
                if let Some(m) = &b.model {
                    s.push_str(&format!(" · {m}"));
                }
                if self.config.connections.len() > 1 {
                    s.push_str(&format!(" · {}", b.conn_name));
                }
                s
            }
            _ => String::new(),
        };
        let hidden = bot.as_ref().is_some_and(|b| b.hidden());
        div()
            .h(px(56.))
            .flex_none()
            .px(px(18.))
            .flex()
            .items_center()
            .gap(px(12.))
            .border_b_1()
            .border_color(pal.border)
            .when_some(bot.clone(), |d, b| {
                d.child(avatar_for(Some(&b), &b.name, 32.)).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).truncate().child(b.title()))
                        .child(div().text_size(px(12.)).text_color(pal.muted).truncate().child(sub)),
                )
            })
            .when(bot.is_none(), |d| d.child(div().flex_1()))
            .when_some(bot.map(|b| b.key), |d, key| {
                d.child(icon_button("hide", if hidden { "eye" } else { "eye-off" }, &pal).on_click(cx.listener(move |this, _, _, cx| this.toggle_hidden(key.clone(), cx))))
            })
            .child(
                div()
                    .id("share")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(6.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(pal.border)
                    .cursor_pointer()
                    .hover(move |s| s.bg(pal.hover))
                    .child(ico("share", 14., pal.text))
                    .child(div().text_size(px(13.)).child("Share"))
                    .on_click(cx.listener(|this, _, _, cx| this.share(cx))),
            )
    }

    pub fn render_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(key) = self.selected.clone() else { return div().into_any_element() };
        let pal = self.pal;
        let n = self.chats.get(&key).map(|c| c.msgs.len()).unwrap_or(0);
        if ix >= n {
            return self.render_footer(&key, window, cx).into_any_element();
        }
        // Lazily create the markdown state (parsed once, then updated incrementally).
        let md = {
            let chat = self.chats.get_mut(&key).unwrap();
            let vm = &mut chat.msgs[ix];
            if vm.md.is_none() && !vm.m.text.is_empty() {
                let text = vm.m.text.clone();
                vm.md = Some(cx.new(|cx| TextViewState::markdown(&text, cx).selectable(true)));
            }
            vm.md.clone()
        };
        let m = self.chats[&key].msgs[ix].m.clone();
        let prev_role = ix.checked_sub(1).and_then(|i| self.chats[&key].msgs.get(i)).map(|p| p.m.role.clone());
        let top_gap = if prev_role.as_ref() == Some(&m.role) { 4. } else { 14. };
        for a in &m.attachments {
            if a.kind == AttachmentKind::Image && a.local_path.is_none() {
                self.want_media(&key, a);
            }
        }
        let bot = self.bot(&key).cloned();
        let body = md.map(|s| TextView::new(&s).selectable(true));
        let time = fmt_time(m.timestamp);
        let atts = self.render_attachments(&key, &m.attachments, matches!(m.role, Role::User), cx);
        let outer = div().w_full().px(px(22.)).pt(px(top_gap));
        match &m.role {
            Role::User => outer
                .flex()
                .flex_col()
                .items_end()
                .child(atts)
                .when(!m.text.is_empty(), |d| {
                    d.child(
                        div()
                            .max_w(relative(0.72))
                            .px(px(14.))
                            .py(px(9.))
                            .rounded(px(18.))
                            .rounded_br(px(6.))
                            .bg(pal.accent)
                            .text_color(pal.on_accent)
                            .text_size(px(14.))
                            .children(body),
                    )
                })
                .child(div().pt(px(3.)).pr(px(4.)).text_size(px(10.5)).text_color(pal.faint).child(time))
                .into_any_element(),
            Role::Assistant => outer
                .flex()
                .gap(px(10.))
                .child(div().w(px(30.)).flex_none().when(prev_role.as_ref() != Some(&Role::Assistant), |d| d.child(avatar_for(bot.as_ref(), &key.profile, 30.))))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(px(6.))
                        .when(!m.activity.is_empty(), |d| d.child(self.render_activity(&m.activity)))
                        .when(!m.text.is_empty(), |d| {
                            d.child(div().max_w(relative(0.86)).px(px(14.)).py(px(9.)).rounded(px(18.)).rounded_bl(px(6.)).bg(pal.bot_bubble).text_size(px(14.)).children(body))
                        })
                        .when(m.streaming && m.text.is_empty() && m.activity.is_empty(), |d| d.child(typing_dots(&pal)))
                        .child(atts)
                        .when(!m.streaming, |d| d.child(div().pl(px(4.)).text_size(px(10.5)).text_color(pal.faint).child(time))),
                )
                .into_any_element(),
            Role::Agent(n) => {
                let sender = self.bots.iter().find(|b| b.tags().contains(&n.handle.split('@').next().unwrap_or("").to_lowercase())).cloned();
                outer
                    .flex()
                    .gap(px(10.))
                    .child(div().w(px(30.)).flex_none().child(avatar_for(sender.as_ref(), &n.name, 30.)))
                    .child(
                        div()
                            .max_w(relative(0.86))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .px(px(14.))
                            .py(px(10.))
                            .rounded(px(14.))
                            .bg(pal.notice_bg)
                            .border_1()
                            .border_color(pal.notice_border)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(12.))
                                    .child(ico("bot", 13., pal.text))
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!("Message from {}", n.name)))
                                    .child(div().text_color(pal.muted).child(format!("@{}", n.handle)))
                                    .child(div().text_color(pal.faint).child(format!("· {time}"))),
                            )
                            .child(div().text_size(px(14.)).child(TextView::markdown(SharedString::from(format!("notice-{}", m.id)), n.body.clone()).selectable(true)))
                            .child(atts),
                    )
                    .into_any_element()
            }
            Role::System => outer
                .flex()
                .justify_center()
                .child(div().px(px(10.)).py(px(3.)).rounded(px(8.)).bg(pal.hover).text_size(px(11.5)).text_color(pal.muted).child(m.text.clone()))
                .into_any_element(),
        }
    }

    fn render_activity(&self, acts: &[Activity]) -> impl IntoElement {
        let pal = self.pal;
        div().flex().flex_col().gap(px(4.)).max_w(relative(0.86)).children(acts.iter().map(|a| {
            let (state_icon, state_color) = match a.state {
                ToolState::Running => ("loader-circle", pal.muted),
                ToolState::Done => ("check", pal.ok),
                ToolState::Failed => ("x", pal.danger),
            };
            div()
                .flex()
                .items_start()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(10.))
                .border_1()
                .border_color(if a.is_subagent { pal.notice_border } else { pal.border })
                .when(a.is_subagent, |d| d.bg(pal.notice_bg))
                .child(ico(if a.is_subagent { "users" } else { "wrench" }, 13., pal.muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .text_size(px(12.5))
                                .child(div().font_weight(FontWeight::MEDIUM).child(if a.is_subagent { format!("Subagent · {}", a.title) } else { a.title.clone() }))
                                .when_some(a.duration_s, |d, s| d.child(div().text_color(pal.faint).child(format!("{s:.1}s")))),
                        )
                        .when_some(a.detail.clone().or(a.summary.clone()), |d, t| d.child(div().text_size(px(11.5)).text_color(pal.muted).truncate().child(t))),
                )
                .child(ico(state_icon, 13., state_color))
        }))
    }

    fn render_attachments(&mut self, key: &BotKey, atts: &[Attachment], mine: bool, cx: &mut Context<Self>) -> Div {
        let pal = self.pal;
        let mut row = div().flex().flex_wrap().gap(px(8.)).when(mine, |d| d.justify_end());
        for (i, a) in atts.iter().enumerate() {
            let local = a.local_path.clone().map(std::path::PathBuf::from).or_else(|| a.remote_path.as_ref().and_then(|r| self.media.get(r).cloned()));
            let id = SharedString::from(format!("att-{}-{}-{}", key.profile, a.remote_path.as_deref().or(a.local_path.as_deref()).unwrap_or(&a.name), i));
            let (k1, a1) = (key.clone(), a.clone());
            let (k2, a2) = (key.clone(), a.clone());
            if a.kind == AttachmentKind::Image {
                row = row.child(
                    div()
                        .id(id)
                        .rounded(px(12.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.open_attachment(k1.clone(), &a1, cx)))
                        .child(match local {
                            Some(p) => img(p).max_w(px(360.)).max_h(px(260.)).object_fit(ObjectFit::Contain).into_any_element(),
                            None => div().w(px(220.)).h(px(140.)).bg(pal.panel).flex().items_center().justify_center().text_size(px(12.)).text_color(pal.faint).child("Loading image…").into_any_element(),
                        }),
                );
            } else {
                row = row.child(
                    div()
                        .id(id)
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(12.))
                        .py(px(9.))
                        .rounded(px(12.))
                        .bg(pal.panel)
                        .border_1()
                        .border_color(pal.border)
                        .min_w(px(220.))
                        .child(ico("file-text", 20., pal.muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).truncate().child(a.name.clone()))
                                .child(div().text_size(px(11.)).text_color(pal.faint).child(a.size.map(fmt_size).unwrap_or_else(|| a.mime.clone()))),
                        )
                        .child(icon_button(SharedString::from(format!("open-{}", a.name)), "external-link", &pal).on_click(cx.listener(move |this, _, _, cx| this.open_attachment(k1.clone(), &a1, cx))))
                        .child(icon_button(SharedString::from(format!("save-{}", a.name)), "download", &pal).on_click(cx.listener(move |this, _, _, cx| this.save_attachment(k2.clone(), &a2, cx)))),
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
        let empty = chat.is_none_or(|c| c.msgs.is_empty() && c.loaded);
        let approvals: Vec<_> = self.approvals.iter().filter(|a| a.key.as_ref().is_none_or(|k| k == key)).map(|a| (a.request_id.clone(), a.command.clone(), a.description.clone(), a.choices.clone())).collect();
        let bot = self.bot(key).cloned();
        div()
            .w_full()
            .px(px(22.))
            .pt(px(10.))
            .pb(px(18.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .when(empty, |d| {
                d.child(
                    div()
                        .py(px(60.))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(10.))
                        .child(avatar_for(bot.as_ref(), &key.profile, 64.))
                        .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child(bot.as_ref().map(|b| b.title()).unwrap_or_default()))
                        .child(div().text_size(px(13.)).text_color(pal.muted).child(bot.as_ref().and_then(|b| b.description.clone()).unwrap_or_else(|| "Say hello to start this Bot Chat.".into()))),
                )
            })
            .when(awaiting, |d| d.child(div().flex().gap(px(10.)).child(div().w(px(30.)).child(avatar_for(bot.as_ref(), &key.profile, 30.))).child(typing_dots(&pal))))
            .children(approvals.into_iter().map(|(id, command, description, choices)| {
                div()
                    .p(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(pal.accent)
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child("Hermes wants to run a command"))
                    .when(!description.is_empty(), |d| d.child(div().text_size(px(12.5)).text_color(pal.muted).child(description)))
                    .child(div().px(px(10.)).py(px(6.)).rounded(px(8.)).bg(pal.panel).font_family("Menlo").text_size(px(12.5)).child(command))
                    .child(div().flex().gap(px(8.)).children(choices.into_iter().map(|c| {
                        let (id2, c2) = (id.clone(), c.clone());
                        let label = match c.as_str() {
                            "once" => "Allow once",
                            "session" => "Allow this session",
                            "always" => "Always allow",
                            "deny" => "Deny",
                            other => other,
                        }
                        .to_owned();
                        pill_button(SharedString::from(format!("ap-{id}-{c}")), label, c == "once", &pal).on_click(cx.listener(move |this, _, _, cx| this.approve(&id2, &c2, cx)))
                    })))
            }))
            .when_some(error, |d, e| d.child(div().px(px(12.)).py(px(8.)).rounded(px(10.)).bg(rgba(0xf8717122)).text_color(pal.danger).text_size(px(13.)).child(e)))
    }

    fn render_composer(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let busy = self.current_chat().is_some_and(|c| c.streaming || c.awaiting);
        let pending = self.pending.clone();
        let mention_rows: Vec<(usize, Bot)> = self.mention.as_ref().map(|m| m.items.iter().enumerate().filter_map(|(i, k)| self.bot(k).cloned().map(|b| (i, b))).collect()).unwrap_or_default();
        let sel = self.mention.as_ref().map(|m| m.sel).unwrap_or(0);
        let (at, cursor) = self.mention.as_ref().map(|m| (m.at, m.cursor)).unwrap_or((0, 0));
        let view = cx.entity().downgrade();
        div()
            .flex_none()
            .px(px(22.))
            .pb(px(18.))
            .pt(px(4.))
            .child(
                div()
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
                                .left(px(8.))
                                .mb(px(6.))
                                .w(px(320.))
                                .p(px(4.))
                                .rounded(px(12.))
                                .bg(pal.panel)
                                .border_1()
                                .border_color(pal.border)
                                .shadow_lg()
                                .children(mention_rows.into_iter().map(|(i, b)| {
                                    let key = b.key.clone();
                                    let tag = self.tag_for(&b.key);
                                    div()
                                        .id(SharedString::from(format!("m-{}-{}", b.key.conn, b.key.profile)))
                                        .flex()
                                        .items_center()
                                        .gap(px(8.))
                                        .px(px(8.))
                                        .py(px(6.))
                                        .rounded(px(8.))
                                        .cursor_pointer()
                                        .when(i == sel, |d| d.bg(pal.selected))
                                        .hover(move |s| s.bg(pal.hover))
                                        .on_click(cx.listener(move |this, _, window, cx| this.pick_mention(at, cursor, key.clone(), window, cx)))
                                        .child(avatar_for(Some(&b), &b.name, 24.))
                                        .child(div().flex_1().min_w_0().truncate().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(b.title()))
                                        .child(div().text_size(px(12.)).text_color(pal.muted).child(format!("@{tag}")))
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(24.))
                            .bg(pal.input_bg)
                            .border_1()
                            .border_color(if self.dragging { pal.accent } else { pal.border })
                            .when(!pending.is_empty(), |d| {
                                d.child(div().flex().flex_wrap().gap(px(6.)).px(px(6.)).pt(px(4.)).children(pending.into_iter().enumerate().map(|(i, f)| {
                                    let path = f.path.clone();
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .pl(px(4.))
                                        .pr(px(2.))
                                        .py(px(3.))
                                        .rounded(px(10.))
                                        .bg(pal.selected)
                                        .child(if f.mime.starts_with("image/") {
                                            img(f.path.clone()).size(px(30.)).rounded(px(6.)).object_fit(ObjectFit::Cover).into_any_element()
                                        } else {
                                            ico("file-text", 16., pal.muted).into_any_element()
                                        })
                                        .child(div().max_w(px(160.)).truncate().text_size(px(12.)).child(f.name.clone()))
                                        .child(div().text_size(px(11.)).text_color(pal.faint).child(fmt_size(f.size)))
                                        .child(icon_button(SharedString::from(format!("rm-{i}")), "x", &pal).size(px(22.)).on_click(cx.listener(move |this, _, _, cx| {
                                            this.pending.retain(|p| p.path != path);
                                            cx.notify();
                                        })))
                                })))
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_end()
                                    .gap(px(4.))
                                    .child(icon_button("attach", "plus", &pal).on_click(cx.listener(|this, _, _, cx| this.pick_files(cx))))
                                    .child(
                                        div().flex_1().min_w_0().py(px(2.)).child(
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
                                    .child(icon_button("mic", "mic", &pal).on_click(cx.listener(|this, _, _, cx| {
                                        this.show_toast("Voice input isn't available yet: Hermes' voice.* gateway methods are not served on remote connections.", false, cx);
                                    })))
                                    .child(
                                        div()
                                            .id("send")
                                            .size(px(32.))
                                            .rounded_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .bg(pal.accent)
                                            .hover(|s| s.opacity(0.85))
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

    fn render_setup(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let pal = self.pal;
        let f = self.setup.as_ref()?;
        let can_cancel = !self.config.connections.is_empty();
        let field = |label: &'static str, input: &Entity<gpui_component::input::InputState>| {
            div().flex().flex_col().gap(px(5.)).child(div().text_size(px(12.)).text_color(pal.muted).child(label)).child(Input::new(input))
        };
        Some(
            modal(&pal).child(
                card(&pal)
                    .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("Connect to Hermes"))
                    .child(div().text_size(px(13.)).text_color(pal.muted).child("Use the address of your Hermes dashboard (`hermes serve`), local or over Tailscale, and its session token. The token is stored in your system keychain."))
                    .child(field("Name", &f.name))
                    .child(field("Address", &f.url))
                    .child(field("Session token", &f.token))
                    .when_some(f.error.clone(), |d, e| d.child(div().text_size(px(12.5)).text_color(pal.danger).child(e)))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .when(can_cancel, |d| {
                                d.child(pill_button("cancel-setup", "Cancel", false, &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.setup = None;
                                    cx.notify();
                                })))
                            })
                            .child(pill_button("save-setup", "Connect", true, &pal).on_click(cx.listener(|this, _, window, cx| this.save_setup(window, cx)))),
                    ),
            ),
        )
    }

    fn render_new_bot(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let pal = self.pal;
        let f = self.new_bot.as_ref()?;
        let field = |label: &'static str, input: &Entity<gpui_component::input::InputState>| {
            div().flex().flex_col().gap(px(5.)).child(div().text_size(px(12.)).text_color(pal.muted).child(label)).child(Input::new(input))
        };
        Some(
            modal(&pal).child(
                card(&pal)
                    .child(div().text_size(px(18.)).font_weight(FontWeight::SEMIBOLD).child("New bot"))
                    .child(div().text_size(px(13.)).text_color(pal.muted).child("Creates a Hermes profile on the connected backend (profiles.create), so it shows up in the official Hermes tools too."))
                    .child(field("Name", &f.name))
                    .child(field("Title", &f.title))
                    .child(field("Description", &f.description))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(pill_button("cancel-bot", "Cancel", false, &pal).on_click(cx.listener(|this, _, _, cx| {
                                this.new_bot = None;
                                cx.notify();
                            })))
                            .child(pill_button("create-bot", "Create", true, &pal).on_click(cx.listener(|this, _, _, cx| this.create_bot(cx)))),
                    ),
            ),
        )
    }
}

fn modal(pal: &Palette) -> Div {
    let _ = pal;
    div().absolute().inset_0().bg(rgba(0x00000099)).flex().items_center().justify_center()
}

fn card(pal: &Palette) -> Div {
    div().w(px(440.)).p(px(22.)).rounded(px(16.)).bg(pal.panel).border_1().border_color(pal.border).shadow_lg().flex().flex_col().gap(px(14.))
}

fn typing_dots(pal: &Palette) -> impl IntoElement {
    div()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(18.))
        .bg(pal.bot_bubble)
        .flex()
        .gap(px(5.))
        .children((0..3).map(|i| {
            div().size(px(7.)).rounded_full().bg(pal.muted).with_animation(
                ElementId::Name(format!("dot{i}").into()),
                Animation::new(std::time::Duration::from_millis(1200)).repeat(),
                move |d, t| {
                    let phase = ((t + i as f32 * 0.2) % 1.0 * std::f32::consts::TAU).sin() * 0.5 + 0.5;
                    d.opacity(0.3 + 0.7 * phase)
                },
            )
        }))
}

impl Render for HermesApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let view = cx.entity();
        let messages = list(self.list.clone(), move |ix, window, cx| view.update(cx, |this, cx| this.render_item(ix, window, cx))).flex_1().w_full();
        let setup = self.render_setup(cx);
        let new_bot = self.render_new_bot(cx);
        div()
            .size_full()
            .relative()
            .flex()
            .bg(pal.bg)
            .text_color(pal.text)
            .text_size(px(14.))
            .track_focus(&self.focus)
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
            .child(self.render_sidebar(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(self.render_header(cx))
                    .child(messages)
                    .child(self.render_composer(window, cx)),
            )
            .when_some(self.toast.clone(), |d, (msg, err)| {
                d.child(
                    div().absolute().top(px(14.)).left_0().right_0().flex().justify_center().child(
                        div()
                            .px(px(14.))
                            .py(px(8.))
                            .rounded(px(10.))
                            .bg(pal.panel)
                            .border_1()
                            .border_color(if err { pal.danger } else { pal.border })
                            .shadow_lg()
                            .text_size(px(13.))
                            .max_w(px(560.))
                            .child(msg),
                    ),
                )
            })
            .children(setup)
            .children(new_bot)
    }
}
