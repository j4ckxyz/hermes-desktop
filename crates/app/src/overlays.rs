//! Overlays: command palette, Settings, Apps and the new-bot form (onboarding lives in `onboard.rs`).

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::Input;
use hermes_core::apps::{self, AppItem, AppKind};
use hermes_core::conn::{Density, ThemePref};
use hermes_core::gateway::LinkState;

use crate::app::*;
use crate::theme::{Palette, ACCENTS};
use crate::ui::*;
use crate::view::{field, swatch};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl HermesApp {
    pub fn render_overlays(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut v = vec![];
        if self.settings.is_some() {
            v.push(self.render_settings(window, cx).into_any_element());
        }
        if self.apps.is_some() {
            v.push(self.render_apps(cx).into_any_element());
        }
        if self.usage.is_some() {
            v.push(self.render_usage(cx).into_any_element());
        }
        if self.palette.is_some() {
            v.push(self.render_palette(cx).into_any_element());
        }
        if let Some(e) = self.render_new_bot(cx) {
            v.push(e.into_any_element());
        }
        if let Some(e) = self.render_onboarding(cx) {
            v.push(e);
        }
        v
    }

    fn dismiss_scrim(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        scrim(id, &self.pal).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, window, cx| {
                this.close_top(window, cx);
            }),
        )
    }

    // ---- palette ----------------------------------------------------------------------------

    fn render_palette(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let items = self.palette_items(cx);
        let sel = self.palette.as_ref().map(|p| p.sel).unwrap_or(0).min(items.len().saturating_sub(1));
        let input = self.palette.as_ref().unwrap().input.clone();
        let n = items.len();
        let mut groups: Vec<(&'static str, Vec<(usize, PaletteItem)>)> = vec![("BOTS", vec![]), ("THREADS", vec![]), ("ACTIONS", vec![])];
        for (i, it) in items.iter().cloned().enumerate() {
            let g = match it.target {
                PaletteTarget::Bot(_) => 0,
                PaletteTarget::Thread(..) => 1,
                PaletteTarget::Action(_) => 2,
            };
            groups[g].1.push((i, it));
        }
        let list = div().id("palette-list").max_h(px(400.)).overflow_y_scroll().p(px(8.)).flex().flex_col().children(groups.into_iter().filter(|g| !g.1.is_empty()).map(|(label, rows)| {
            div().flex().flex_col().gap(px(2.)).pb(px(4.)).child(div().px(px(8.)).child(section_label(label, &pal))).children(rows.into_iter().map(|(i, it)| {
                let bot = match &it.target {
                    PaletteTarget::Bot(k) => self.bot(k).cloned(),
                    _ => None,
                };
                let leading: AnyElement = match &it.target {
                    PaletteTarget::Bot(_) => avatar_for(bot.as_ref(), &it.title, 28.).into_any_element(),
                    PaletteTarget::Thread(..) => tile_icon("messages-square", &pal).into_any_element(),
                    PaletteTarget::Action(a) => tile_icon(act_icon(*a), &pal).into_any_element(),
                };
                let it2 = it.clone();
                div()
                    .id(SharedString::from(format!("pi-{i}")))
                    .h(px(44.))
                    .px(px(8.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .cursor_pointer()
                    .when(i == sel, |d| d.bg(pal.selected))
                    .hover(move |s| s.bg(pal.hover))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_palette(it2.clone(), window, cx)))
                    .child(leading)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_none().max_w(relative(0.6)).truncate().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child(it.title.clone()))
                            .when(!it.subtitle.is_empty(), |d| d.child(div().flex_1().min_w_0().truncate().text_size(pal.t(13.)).text_color(pal.faint).child(it.subtitle.clone()))),
                    )
                    .when_some(it.keys, |d, k| d.child(kbd(keys(k), &pal)))
            }))
        }));
        self.dismiss_scrim("palette-scrim", cx).items_start().pt(px(96.)).child(
            sheet(600., &pal)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .capture_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| {
                    let Some(p) = &mut this.palette else { return };
                    match ev.keystroke.key.as_str() {
                        "up" => p.sel = (p.sel + n.max(1) - 1) % n.max(1),
                        "down" => p.sel = (p.sel + 1) % n.max(1),
                        "enter" => {
                            let items = this.palette_items(cx);
                            if let Some(it) = items.get(sel).cloned() {
                                this.run_palette(it, window, cx);
                            }
                        }
                        _ => return,
                    }
                    cx.stop_propagation();
                    cx.notify();
                }))
                .child(
                    div()
                        .h(px(56.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(ico("search", 18., pal.muted))
                        .child(div().flex_1().child(Input::new(&input).appearance(false)))
                        .child(kbd("Esc", &pal)),
                )
                .child(divider(&pal))
                .child(if n == 0 { div().id("palette-empty").p(px(24.)).text_center().text_size(pal.t(13.)).text_color(pal.faint).child("Nothing matches.") } else { list })
                .child(divider(&pal))
                .child(
                    div()
                        .h(px(36.))
                        .px(px(16.))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .text_size(pal.t(12.))
                        .text_color(pal.faint)
                        .child(div().flex().items_center().gap(px(4.)).child(kbd("↑", &pal)).child(kbd("↓", &pal)).child("navigate"))
                        .child(div().flex().items_center().gap(px(4.)).child(kbd("↩", &pal)).child("open"))
                        .child(div().flex_1())
                        .child(div().flex().items_center().gap(px(4.)).child(kbd(keys("secondary-k"), &pal)).child("toggle")),
                ),
        )
    }

    // ---- settings -----------------------------------------------------------------------------

    fn render_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let section = self.settings.as_ref().unwrap().section;
        let nav = div().w(px(208.)).flex_none().h_full().bg(pal.sidebar).border_r_1().border_color(pal.border).p(px(8.)).flex().flex_col().gap(px(2.))
            .child(div().h(px(48.)).px(px(8.)).flex().items_center().text_size(pal.t(16.)).font_weight(FontWeight::SEMIBOLD).child("Settings"))
            .children(Section::ALL.into_iter().map(|s| {
                let on = s == section;
                let (hover, pressed) = (pal.hover, pal.pressed);
                div()
                    .id(SharedString::from(format!("nav-{s:?}")))
                    .h(px(32.))
                    .px(px(8.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_size(pal.t(13.))
                    .when(on, |d| d.bg(pal.selected).text_color(pal.text).font_weight(FontWeight::MEDIUM))
                    .when(!on, move |d| d.text_color(pal.muted).hover(move |st| st.bg(hover)).active(move |st| st.bg(pressed)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(st) = &mut this.settings {
                            st.section = s;
                        }
                        cx.notify();
                    }))
                    .child(ico(s.icon(), 16., if on { pal.text } else { pal.muted }))
                    .child(s.label())
            }));
        let content = self.render_settings_section(section, cx);
        self.dismiss_scrim("settings-scrim", cx).child(
            sheet(800., &pal)
                .h(px(580.))
                .max_h(relative(0.92))
                .flex_row()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(nav)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(64.))
                                .flex_none()
                                .px(px(24.))
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(pal.t(18.)).font_weight(FontWeight::SEMIBOLD).child(section.label()))
                                .child(icon_btn("settings-close", "x", &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.settings = None;
                                    cx.notify();
                                }))),
                        )
                        .child(div().id("settings-body").flex_1().min_h_0().overflow_y_scroll().px(px(24.)).pb(px(24.)).child(content)),
                ),
        )
    }

    fn render_settings_section(&mut self, s: Section, cx: &mut Context<Self>) -> Div {
        let pal = self.pal;
        let p = self.config.prefs.clone();
        let body = div().flex().flex_col();
        macro_rules! pref_toggle {
            ($id:literal, $title:expr, $desc:expr, $field:ident) => {{
                let on = p.$field;
                row($title, $desc, &pal).child(toggle($id, on, &pal).on_click(cx.listener(move |this, _, window, cx| this.set_pref(|p| p.$field = !on, window, cx))))
            }};
        }
        match s {
            Section::Account => {
                let name = self.settings.as_ref().unwrap().name.clone();
                let conns = self.config.connections.clone();
                body.child(group("PROFILE", &pal).child(
                    row("Display name", "Shown at the bottom of the sidebar.", &pal)
                        .child(div().w(px(200.)).child(Input::new(&name)))
                        .child(button("save-name", "Save", Kind::Secondary, &pal).on_click(cx.listener(|this, _, _, cx| this.save_user_name(cx)))),
                ))
                .child(
                    group("HERMES SERVERS", &pal)
                        .child(div().pb(px(8.)).text_size(pal.t(12.)).text_color(pal.muted).child("Each server is a Hermes dashboard (hermes dashboard or hermes serve): your laptop, a homelab or a VPS over Tailscale. Their bots appear together in the sidebar. Sign-ins are kept in the system keychain and renewed automatically."))
                        .children(conns.into_iter().map(|c| {
                            let state = self.links.get(&c.id).cloned();
                            let (dot, label) = match state {
                                Some(LinkState::Connected) => (pal.ok, "Connected"),
                                Some(LinkState::Unauthorized) => (pal.danger, "Sign in required"),
                                Some(LinkState::Reconnecting { .. }) => (pal.warn, "Reconnecting"),
                                Some(LinkState::Connecting) | None => (pal.faint, "Connecting"),
                                Some(LinkState::Stopped) => (pal.faint, "Stopped"),
                            };
                            let bots = self.bots.iter().filter(|b| b.key.conn == c.id).count();
                            let (id1, id2, id3, id4) = (c.id.clone(), c.id.clone(), c.id.clone(), c.id.clone());
                            let dev = c.id == "env";
                            let needs_sign_in = state == Some(LinkState::Unauthorized);
                            let account = self.svc.store.get_kv(&format!("account:{}", c.id)).filter(|a| !a.is_empty());
                            let who = match account.as_deref() {
                                _ if needs_sign_in => None,
                                Some("This computer") => Some("No sign-in needed (this computer)".to_owned()),
                                Some("Session token") => Some("Using a session token".to_owned()),
                                Some(a) => Some(format!("Signed in as {a}")),
                                None => None,
                            };
                            div()
                                .min_h(px(64.))
                                .py(px(12.))
                                .flex()
                                .items_center()
                                .gap(px(12.))
                                .border_t_1()
                                .border_color(pal.border)
                                .child(tile_icon("server", &pal))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(div().flex().items_center().gap(px(8.)).child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).truncate().child(c.name.clone())).child(div().size(px(6.)).rounded_full().bg(dot)).child(div().text_size(pal.t(12.)).text_color(pal.muted).child(format!("{label} · {bots} bots"))))
                                        .child(div().text_size(pal.t(12.)).text_color(pal.faint).truncate().child(if dev { format!("{} (from the environment, not saved)", c.base_url) } else { c.base_url.clone() }))
                                        .when_some(who, |d, w| d.child(div().flex().items_center().gap(px(4.)).text_size(pal.t(12.)).text_color(pal.muted).child(ico("user", 11., pal.faint)).child(w))),
                                )
                                .when(!dev, |d| {
                                    d.when(needs_sign_in, |d| {
                                        d.child(button(SharedString::from(format!("signin-{}", c.id)), "Sign in", Kind::Primary, &pal).on_click(cx.listener(move |this, _, window, cx| this.open_onboarding(crate::onboard::Step::SignIn, Some(id1.clone()), window, cx))))
                                    })
                                    .when(!needs_sign_in, |d| {
                                        d.child(button(SharedString::from(format!("signout-{}", c.id)), "Sign out", Kind::Secondary, &pal).child(ico("log-out", 13., pal.muted)).flex_row_reverse().on_click(cx.listener(move |this, _, _, cx| this.sign_out(&id4, cx))))
                                    })
                                    .child(button(SharedString::from(format!("switch-{}", c.id)), "Switch server", Kind::Secondary, &pal).on_click(cx.listener(move |this, _, window, cx| this.open_onboarding(crate::onboard::Step::Server, Some(id3.clone()), window, cx))))
                                    .child(icon_btn(SharedString::from(format!("rm-{}", c.id)), "trash-2", &pal).on_click(cx.listener(move |this, _, _, cx| this.remove_connection(&id2, cx))))
                                })
                        }))
                        .child(div().pt(px(12.)).flex().child(button("add-server", "Add server", Kind::Primary, &pal).child(ico("plus", 14., pal.on_accent)).flex_row_reverse().on_click(cx.listener(|this, _, window, cx| this.open_onboarding(crate::onboard::Step::Server, None, window, cx))))),
                )
            }
            Section::Appearance => {
                let theme_items: Vec<(SharedString, bool, Handler)> = [(ThemePref::System, "System"), (ThemePref::Light, "Light"), (ThemePref::Dark, "Dark")]
                    .into_iter()
                    .map(|(t, l)| (SharedString::from(l), p.theme == t, self.pref_handler(move |p| p.theme = t, cx)))
                    .collect();
                let density_items: Vec<(SharedString, bool, Handler)> = [(Density::Compact, "Compact"), (Density::Comfortable, "Comfortable"), (Density::Spacious, "Spacious")]
                    .into_iter()
                    .map(|(t, l)| (SharedString::from(l), p.density == t, self.pref_handler(move |p| p.density = t, cx)))
                    .collect();
                let font_items: Vec<(SharedString, bool, Handler)> =
                    [12.0f32, 13., 14., 15., 16., 18.].into_iter().map(|f| (SharedString::from(format!("{f:.0}")), (p.font_size() - f).abs() < 0.1, self.pref_handler(move |p| p.font_size = f, cx))).collect();
                let accent = accent_of(&self.config);
                body.child(
                    group("THEME", &pal)
                        .child(row("Appearance", "Follow the system, or always light or dark.", &pal).child(segmented("theme", theme_items, &pal)))
                        .child(row("Accent colour", "Buttons, your messages and highlights.", &pal).child(div().flex().gap(px(4.)).children(ACCENTS.iter().map(|(c, name)| {
                            let c = *c;
                            swatch(SharedString::from(format!("ac-{name}")), c, c == accent, &pal).on_click(cx.listener(move |this, _, window, cx| this.set_accent(c, window, cx)))
                        })))),
                )
                .child(
                    group("LAYOUT", &pal)
                        .child(row("Text size", "Base size for messages and the interface.", &pal).child(segmented("font", font_items, &pal)))
                        .child(row("Density", "Compact fits more on screen.", &pal).child(segmented("density", density_items, &pal))),
                )
            }
            Section::Notifications => body
                .child(
                    group("NOTIFICATIONS", &pal)
                        .child(pref_toggle!("n-on", "Desktop notifications", "When a bot replies in a chat you're not looking at.", notifications))
                        .child(pref_toggle!("n-focus", "Only when Hermes is in the background", "Skip notifications while the window is active.", notify_only_unfocused))
                        .child(row("Test", "Send a sample notification.", &pal).child(button("n-test", "Send test", Kind::Secondary, &pal).on_click(|_, _, _| crate::notify::post("Hermes", "Notifications are working.", false)))),
                )
                .child(group("SOUNDS", &pal).child(pref_toggle!("n-sound", "Play a sound", "A short chime for new replies.", sounds))),
            Section::Chat => body.child(
                group("COMPOSER", &pal).child(pref_toggle!("c-enter", "Enter to send", if p.enter_to_send { "Shift+Enter adds a new line." } else { "Enter adds a new line; Cmd/Ctrl+Enter sends." }, enter_to_send)),
            )
            .child({
                use hermes_core::conn::GapAction;
                let ga = p.gap_action;
                let gh = p.gap_hours;
                let action_items: Vec<(SharedString, bool, Handler)> = [(GapAction::Off, "Keep going"), (GapAction::Compact, "Compact"), (GapAction::Fresh, "New session")]
                    .into_iter()
                    .map(|(a, l)| (SharedString::from(l), ga == a, self.pref_handler(move |p| p.gap_action = a, cx)))
                    .collect();
                let hour_items: Vec<(SharedString, bool, Handler)> = [1u32, 3, 6, 12, 24]
                    .into_iter()
                    .map(|h| (SharedString::from(if h == 24 { "1 day".to_owned() } else { format!("{h}h") }), gh == h, self.pref_handler(move |p| p.gap_hours = h, cx)))
                    .collect();
                let desc = match ga {
                    GapAction::Off => "Always continue with the full context. Uses the most tokens.",
                    GapAction::Compact => "Hermes summarises the older context before your next message (session.compress). Same session.",
                    GapAction::Fresh => "Your next message starts a new Bot Chat session. Earlier sessions stay in the stream.",
                };
                group("ONE CONTINUOUS CHAT PER BOT", &pal)
                    .child(row("After a long break", desc, &pal).child(segmented("gap-action", action_items, &pal)))
                    .when(ga != GapAction::Off, |d| d.child(row("A long break is", "Time since the bot's last message.", &pal).child(segmented("gap-hours", hour_items, &pal))))
            })
            .child(
                group("MESSAGES", &pal)
                    .child(pref_toggle!("c-md", "Render Markdown", "Formatting, links, tables and highlighted code blocks.", render_markdown))
                    .child(pref_toggle!("c-ts", "Show timestamps", "Time under each message.", show_timestamps))
                    .child(pref_toggle!("c-tools", "Show tool activity", "Tool calls and subagents while a bot works.", show_tool_activity)),
            ),
            Section::Files => {
                let dir = p.download_dir.clone().unwrap_or_else(|| "Downloads (default)".into());
                body.child(
                    group("DOWNLOADS", &pal).child(
                        row("Save files to", &dir, &pal)
                            .child(button("dl-choose", "Choose…", Kind::Secondary, &pal).on_click(cx.listener(|this, _, _, cx| this.pick_download_dir(cx))))
                            .when(p.download_dir.is_some(), |d| {
                                d.child(button("dl-reset", "Reset", Kind::Ghost, &pal).on_click(cx.listener(|this, _, window, cx| this.set_pref(|p| p.download_dir = None, window, cx))))
                            }),
                    ),
                )
                .child(group("PREVIEWS", &pal).child(pref_toggle!("f-prev", "Inline image previews", "Show images bots send right in the chat.", image_previews)))
                .child(group("LIMITS", &pal).child(row("Uploads", "Images up to 25 MB, other files up to 256 MB (Hermes' limits). Big files are read off the UI thread.", &pal)))
            }
            Section::Shortcuts => body.child(group("KEYBOARD", &pal).children(SHORTCUTS.iter().map(|(bindings, d)| {
                let label = |k: &str| if k.ends_with("-1") { keys(k) + "–9" } else { keys(k) };
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .border_t_1()
                    .border_color(pal.border)
                    .text_size(pal.t(13.))
                    .child(div().min_w_0().truncate().child(*d))
                    .child(div().flex().flex_none().gap(px(4.)).children(bindings.iter().map(|k| kbd(label(k), &pal))))
            }))
            .child(div().h(px(40.)).flex().items_center().justify_between().border_t_1().border_color(pal.border).text_size(pal.t(13.)).child("Send / new line").child(div().flex().gap(px(4.)).child(kbd("↩", &pal)).child(kbd("⇧↩", &pal))))
            .child(div().h(px(40.)).flex().items_center().justify_between().border_t_1().border_color(pal.border).text_size(pal.t(13.)).child("Slash commands").child(kbd("/", &pal)))
            .child(div().h(px(40.)).flex().items_center().justify_between().border_t_1().border_color(pal.border).text_size(pal.t(13.)).child("Mention a bot").child(kbd("@", &pal)))),
            Section::Privacy => body
                .child(group("PRIVACY", &pal).child(pref_toggle!("p-prev", "Hide message previews", "In the sidebar, palette and notifications.", hide_previews)))
                .child(
                    group("LOCAL DATA", &pal)
                        .child(row("Clear cache", "Removes cached conversations and downloaded media from this computer. Your Hermes keeps everything.", &pal).child(button("p-clear", "Clear cache", Kind::Danger, &pal).on_click(cx.listener(|this, _, _, cx| this.clear_cache(cx)))))
                        .child(row("Tokens", "Session tokens live in the system keychain, never in files.", &pal)),
                ),
            Section::Advanced => {
                let lv: Vec<(SharedString, bool, Handler)> = ["error", "warn", "info", "debug"].into_iter().map(|l| (SharedString::from(l), p.log_level == l, self.pref_handler(move |p| p.log_level = l.into(), cx))).collect();
                let log_dir = hermes_core::conn::data_dir().join("logs");
                body.child(
                    group("LOGS", &pal)
                        .child(row("Log level", "How much Hermes Desktop writes to its log file.", &pal).child(segmented("loglevel", lv, &pal)))
                        .child(pref_toggle!("a-net", "Network debug", "Log every gateway call and event by name (never payloads).", network_debug))
                        .child(row("Log folder", &log_dir.display().to_string(), &pal).child(button("a-logs", "Open", Kind::Secondary, &pal).on_click(move |_, _, cx| {
                            let _ = std::fs::create_dir_all(&log_dir);
                            cx.open_with_system(&log_dir);
                        }))),
                )
                .child(group("CONNECTION", &pal).child(row("Reconnect", "Drop and re-open every Hermes connection.", &pal).child(button("a-reconnect", "Reconnect", Kind::Secondary, &pal).on_click(cx.listener(|this, _, _, cx| {
                    for c in this.config.connections.clone() {
                        this.svc.send(hermes_core::Cmd::Connect(c));
                    }
                    this.show_toast("Reconnecting…", false, cx);
                })))))
            }
            Section::About => body.child(
                div()
                    .py(px(24.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(letter_tile("H", pal.accent, 64., false))
                    .child(div().pt(px(8.)).text_size(pal.t(18.)).font_weight(FontWeight::SEMIBOLD).child("Hermes Desktop"))
                    .child(div().text_size(pal.t(13.)).text_color(pal.muted).child(format!("Version {VERSION} · GPUI · {}", match std::env::consts::OS { "macos" => "macOS", "windows" => "Windows", "linux" => "Linux", o => o }))),
            )
            .child(
                group("UPDATES", &pal)
                    .child(row("Updates", "There is no update server yet. To update, pull the repo and rebuild in release mode. Release notes are in CHANGELOG.md.", &pal).child(button("ab-repo", "Open repo", Kind::Secondary, &pal).on_click(|_, _, cx| cx.open_url("https://github.com/j4ckxyz/hermes-desktop"))))
                    .child(row("Protocol", "Only official Hermes APIs: gateway WebSocket /api/ws, dashboard REST and the /v1 API server.", &pal)),
            ),
        }
    }

    fn pref_handler(&self, f: impl Fn(&mut hermes_core::conn::Prefs) + 'static, cx: &mut Context<Self>) -> Handler {
        let f = std::rc::Rc::new(f);
        Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
            let f = f.clone();
            this.set_pref(move |p| f(p), window, cx)
        }))
    }

    // ---- usage ---------------------------------------------------------------------------------

    fn render_usage(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        use hermes_core::usage::{fmt_cost, fmt_count, merge_daily, Totals, Usage};
        let pal = self.pal;
        let u = self.usage.as_ref().unwrap();
        let (days, sel) = (u.days, u.bot.clone());
        let loading = u.loading(&self.bots);
        // Per-bot results, in roster order.
        let rows: Vec<(hermes_core::roster::Bot, Result<Usage, String>)> = self.bots.iter().filter_map(|b| u.data.get(&b.key.base()).map(|r| (b.clone(), r.clone()))).collect();
        let shown: Vec<&Usage> = rows.iter().filter(|(b, _)| sel.as_ref().is_none_or(|k| b.key.same_bot(k))).filter_map(|(_, r)| r.as_ref().ok()).collect();
        let mut totals = Totals::default();
        for x in &shown {
            totals.add(&x.totals);
        }
        let daily = merge_daily(&shown);
        let mut models: Vec<(String, Totals)> = vec![];
        for x in &shown {
            for m in &x.by_model {
                match models.iter_mut().find(|(n, _)| *n == m.model) {
                    Some((_, t)) => t.add(&m.t),
                    None => models.push((m.model.clone(), m.t.clone())),
                }
            }
        }
        models.sort_by_key(|(_, t)| -t.tokens());
        let errors: Vec<(String, String)> = rows.iter().filter_map(|(b, r)| r.as_ref().err().map(|e| (b.title(), e.clone()))).collect();
        let any_actual = shown.iter().any(|x| x.totals.actual_cost > 0.0);

        // Daily series over the whole range (quiet days are zero).
        let today = chrono::Local::now().date_naive();
        let series: Vec<(String, i64)> = (0..days as i64)
            .rev()
            .map(|d| {
                let day = (today - chrono::Duration::days(d)).format("%Y-%m-%d").to_string();
                let v = daily.iter().find(|x| x.day == day).map(|x| x.t.tokens()).unwrap_or(0);
                (day, v)
            })
            .collect();
        let peak = series.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1);

        let range_items: Vec<(SharedString, bool, Handler)> = [7u32, 30, 90]
            .into_iter()
            .map(|d| {
                let h: Handler = Box::new(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    if let Some(u) = &mut this.usage {
                        u.days = d;
                    }
                    this.reload_usage(cx);
                }));
                (SharedString::from(format!("{d} days")), days == d, h)
            })
            .collect();

        let tile = |label: &'static str, value: String, sub: String| {
            div()
                .flex_1()
                .min_w(px(120.))
                .p(px(12.))
                .flex()
                .flex_col()
                .gap(px(4.))
                .rounded(px(12.))
                .bg(pal.raised)
                .border_1()
                .border_color(pal.border)
                .child(div().text_size(pal.t(12.)).text_color(pal.muted).child(label))
                .child(div().text_size(pal.t(22.)).font_weight(FontWeight::SEMIBOLD).text_color(pal.text).child(value))
                .child(div().text_size(pal.t(11.)).text_color(pal.faint).truncate().child(sub))
        };

        let mut chips = div().flex().flex_wrap().gap(px(8.)).child(chip("u-all", "All bots", sel.is_none(), &pal).on_click(cx.listener(|this, _, _, cx| {
            if let Some(u) = &mut this.usage {
                u.bot = None;
            }
            cx.notify();
        })));
        for b in &self.bots {
            if b.hidden() && !self.show_hidden {
                continue;
            }
            let k = b.key.base();
            let on = sel.as_ref().is_some_and(|s| s.same_bot(&k));
            chips = chips.child(chip(SharedString::from(format!("u-{}-{}", k.conn, k.profile)), b.title(), on, &pal).on_click(cx.listener(move |this, _, _, cx| {
                if let Some(u) = &mut this.usage {
                    u.bot = Some(k.clone());
                }
                cx.notify();
            })));
        }

        let chart = div()
            .h(px(172.))
            .p(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(12.))
            .bg(pal.raised)
            .border_1()
            .border_color(pal.border)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(pal.t(11.))
                    .text_color(pal.faint)
                    .child(format!("Tokens per day · peak {}", fmt_count(peak)))
                    .child(if loading { "Loading…".to_owned() } else { String::new() }),
            )
            .child(div().flex_1().flex().items_end().gap(px(if days > 60 { 1. } else { 2. })).children(series.iter().map(|(_, v)| {
                let h = if *v == 0 { 2.0 } else { 4.0 + 88.0 * (*v as f32 / peak as f32) };
                div().flex_1().h(px(h)).rounded_t(px(2.)).bg(if *v == 0 { pal.border } else { pal.accent })
            })))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(pal.t(11.))
                    .text_color(pal.faint)
                    .child(series.first().map(|(d, _)| short_day(d)).unwrap_or_default())
                    .child(series.last().map(|(d, _)| short_day(d)).unwrap_or_default()),
            );

        // Per-bot breakdown (All bots) or per-model (one bot).
        let all_tokens = rows.iter().filter_map(|(_, r)| r.as_ref().ok()).map(|x| x.totals.tokens()).sum::<i64>().max(1);
        let table_head = |cols: [&'static str; 4]| {
            div()
                .h(px(28.))
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(pal.t(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(pal.faint)
                .child(div().flex_1().child(cols[0]))
                .child(div().w(px(84.)).flex().justify_end().child(cols[1]))
                .child(div().w(px(72.)).flex().justify_end().child(cols[2]))
                .child(div().w(px(72.)).flex().justify_end().child(cols[3]))
        };
        let table = if sel.is_none() {
            let mut sorted: Vec<&(hermes_core::roster::Bot, Result<Usage, String>)> = rows.iter().collect();
            sorted.sort_by_key(|(_, r)| -r.as_ref().map(|x| x.totals.tokens()).unwrap_or(-1));
            div().flex().flex_col().child(section_label("BY BOT", &pal)).child(table_head(["Bot", "Tokens", "Cost", "Requests"])).children(sorted.into_iter().map(|(b, r)| {
                let k = b.key.base();
                let t = r.as_ref().map(|x| x.totals.clone()).unwrap_or_default();
                let share = t.tokens() as f32 / all_tokens as f32;
                div()
                    .id(SharedString::from(format!("ub-{}-{}", k.conn, k.profile)))
                    .h(px(44.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(pal.border)
                    .cursor_pointer()
                    .hover(move |s| s.bg(pal.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(u) = &mut this.usage {
                            u.bot = Some(k.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(avatar_for(Some(b), &b.name, 24.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap(px(4.))
                                    .child(div().text_size(pal.t(13.)).truncate().child(b.title()))
                                    .child(div().h(px(4.)).w_full().rounded_full().bg(pal.border).child(div().h_full().w(relative(share.clamp(0.0, 1.0))).rounded_full().bg(pal.accent))),
                            ),
                    )
                    .child(div().w(px(84.)).flex().justify_end().text_size(pal.t(13.)).child(match r {
                        Ok(_) => fmt_count(t.tokens()),
                        Err(_) => "—".into(),
                    }))
                    .child(div().w(px(72.)).flex().justify_end().text_size(pal.t(13.)).text_color(pal.muted).child(if r.is_ok() { fmt_cost(t.cost()) } else { "—".into() }))
                    .child(div().w(px(72.)).flex().justify_end().text_size(pal.t(13.)).text_color(pal.muted).child(if r.is_ok() { fmt_count(t.requests) } else { "—".into() }))
            }))
        } else {
            div().flex().flex_col().child(section_label("BY MODEL", &pal)).child(table_head(["Model", "Tokens", "Cost", "Requests"])).children(models.iter().map(|(name, t)| {
                div()
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(pal.border)
                    .text_size(pal.t(13.))
                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                    .child(div().w(px(84.)).flex().justify_end().child(fmt_count(t.tokens())))
                    .child(div().w(px(72.)).flex().justify_end().text_color(pal.muted).child(fmt_cost(t.cost())))
                    .child(div().w(px(72.)).flex().justify_end().text_color(pal.muted).child(fmt_count(t.requests)))
            }))
        };

        let title = match &sel {
            Some(k) => self.bot(k).map(|b| b.title()).unwrap_or_default(),
            None => "All bots".into(),
        };
        let cost_sub = if any_actual { "Provider-reported".to_owned() } else { "Estimated".to_owned() };
        let body = div()
            .id("usage-body")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(24.))
            .pb(px(24.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(div().flex().items_center().justify_between().gap(px(12.)).flex_wrap().child(div().text_size(pal.t(15.)).font_weight(FontWeight::SEMIBOLD).child(title)).child(segmented("u-range", range_items, &pal)))
            .child(chips)
            .child(if loading && shown.is_empty() {
                div().flex().gap(px(12.)).children((0..4).map(|i| div().flex_1().child(skeleton(SharedString::from(format!("usk-{i}")), relative(1.).into(), 84., &pal)))).into_any_element()
            } else {
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(12.))
                    .child(tile("Tokens", fmt_count(totals.tokens()), format!("{} in · {} out", fmt_count(totals.input_tokens), fmt_count(totals.output_tokens))))
                    .child(tile("Cost", fmt_cost(totals.cost()), cost_sub))
                    .child(tile("Requests", fmt_count(totals.requests), "Model API calls".into()))
                    .child(tile("Sessions", fmt_count(totals.sessions), format!("Last {days} days")))
                    .into_any_element()
            })
            .child(chart)
            .child(table)
            .when(!errors.is_empty(), |d| {
                d.child(div().flex().flex_col().gap(px(4.)).children(errors.iter().map(|(b, e)| {
                    div().flex().items_center().gap(px(8.)).text_size(pal.t(12.)).text_color(pal.muted).child(ico("circle-alert", 14., pal.warn)).child(format!("{b}: {e}"))
                })))
            })
            .child(
                div()
                    .text_size(pal.t(11.))
                    .text_color(pal.faint)
                    .child("From Hermes' session store (GET /api/analytics/usage), per bot profile. Not available from Hermes: per-message cost, live rate limits (usage.bars needs a Nous account), and real cost for providers that don't report it, which is shown as an estimate."),
            );

        self.dismiss_scrim("usage-scrim", cx).child(
            sheet(760., &pal)
                .h(px(660.))
                .max_h(relative(0.92))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .h(px(64.))
                        .flex_none()
                        .px(px(24.))
                        .flex()
                        .items_center()
                        .gap(px(12.))
                        .child(tile_icon("chart-column", &pal))
                        .child(div().flex_1().flex().flex_col().child(div().text_size(pal.t(16.)).font_weight(FontWeight::SEMIBOLD).child("Usage")).child(div().text_size(pal.t(12.)).text_color(pal.muted).child("Tokens, cost, requests and sessions per bot")))
                        .child(icon_btn("u-close", "x", &pal).on_click(cx.listener(|this, _, _, cx| {
                            this.usage = None;
                            cx.notify();
                        }))),
                )
                .child(body),
        )
    }

    // ---- apps -----------------------------------------------------------------------------------

    fn render_apps(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let st = self.apps.as_ref().unwrap();
        let snap = st.snapshot();
        let loading = st.loading();
        let q = st.search.read(cx).value().to_string();
        let cat = st.category.clone();
        let search = st.search.clone();
        let busy = st.busy.clone();
        let expanded = st.expanded.clone();
        let tools = st.tools.clone();
        let bot_title = self.bot(&st.key).map(|b| b.title()).unwrap_or_default();
        let mut chips = vec![String::new(), "Connected".to_owned()];
        chips.extend(apps::categories(&snap.items));
        let filtered: Vec<AppItem> = if cat == "Connected" { apps::filter(&snap.items, &q, "").into_iter().filter(|i| i.connected).cloned().collect() } else { apps::filter(&snap.items, &q, &cat).into_iter().cloned().collect() };
        let sections: Vec<(&str, Vec<AppItem>)> = vec![
            ("CONNECTED", filtered.iter().filter(|i| i.connected && matches!(i.kind, AppKind::Connector | AppKind::Mcp)).cloned().collect()),
            ("RECOMMENDED", filtered.iter().filter(|i| apps::is_recommended(i)).cloned().collect()),
            ("TOOLSETS", filtered.iter().filter(|i| i.kind == AppKind::Toolset).cloned().collect()),
            ("SKILLS", filtered.iter().filter(|i| i.kind == AppKind::Skill).cloned().collect()),
        ];
        let any = sections.iter().any(|s| !s.1.is_empty());

        let mut body = div().flex().flex_col().gap(px(24.)).p(px(24.)).pt(px(8.));
        if loading && snap.items.is_empty() {
            body = body.child(div().flex().flex_wrap().gap(px(12.)).children((0..6).map(|i| app_card_skeleton(i, &pal))));
        }
        for (label, items) in sections.into_iter().filter(|s| !s.1.is_empty()) {
            let mut grid = div().flex().flex_wrap().gap(px(12.));
            for it in items {
                let open = expanded.as_deref() == Some(it.id.as_str());
                grid = grid.child(self.app_card(&it, busy.contains(&it.id), open, cx));
                if open {
                    grid = grid.child(self.app_detail(&it, tools.get(&it.id), cx));
                }
            }
            body = body.child(div().flex().flex_col().gap(px(8.)).child(section_label(label, &pal)).child(grid));
        }
        if !loading && !any {
            body = body.child(
                div()
                    .py(px(48.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(8.))
                    .child(ico("search", 24., pal.faint))
                    .child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child("No apps match"))
                    .child(div().text_size(pal.t(13.)).text_color(pal.muted).child("Try another search or category.")),
            );
        }
        if !snap.unavailable.is_empty() {
            body = body.child(div().flex().flex_col().gap(px(8.)).children(snap.unavailable.iter().map(|(k, e)| {
                div()
                    .px(px(12.))
                    .py(px(8.))
                    .rounded(px(10.))
                    .bg(pal.raised)
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .text_size(pal.t(12.))
                    .text_color(pal.muted)
                    .child(ico("info", 14., pal.faint))
                    .child(div().flex_1().min_w_0().child(format!(
                        "{} aren't available from this Hermes: {e}",
                        match k {
                            AppKind::Connector => "Hosted apps (connectors)",
                            AppKind::Mcp => "MCP presets",
                            AppKind::Toolset => "Toolsets",
                            AppKind::Skill => "Skills",
                        }
                    )))
            })));
        }

        self.dismiss_scrim("apps-scrim", cx).child(
            sheet(760., &pal)
                .h(px(640.))
                .max_h(relative(0.92))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .px(px(24.))
                        .pt(px(20.))
                        .pb(px(16.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .justify_between()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(4.))
                                        .child(div().text_size(pal.t(20.)).font_weight(FontWeight::SEMIBOLD).child("Connect apps"))
                                        .child(div().text_size(pal.t(13.)).text_color(pal.muted).child(format!("Apps, MCP servers, toolsets and skills for {bot_title}, from this Hermes profile."))),
                                )
                                .child(icon_btn("apps-close", "x", &pal).on_click(cx.listener(|this, _, _, cx| {
                                    this.apps = None;
                                    cx.notify();
                                }))),
                        )
                        .child(
                            div()
                                .h(px(40.))
                                .px(px(12.))
                                .rounded(px(10.))
                                .bg(pal.input_bg)
                                .border_1()
                                .border_color(pal.border)
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(ico("search", 16., pal.muted))
                                .child(div().flex_1().child(Input::new(&search).appearance(false)))
                                .when(loading, |d| d.child(ico("loader-circle", 14., pal.faint).with_animation("apps-spin", Animation::new(std::time::Duration::from_millis(900)).repeat(), |s, t| s.with_transformation(Transformation::rotate(percentage(t)))))),
                        )
                        .child(div().id("apps-chips").flex().gap(px(8.)).overflow_x_scroll().children(chips.into_iter().map(|c| {
                            let label = if c.is_empty() { "All".to_owned() } else { c.clone() };
                            chip(SharedString::from(format!("cat-{label}")), label, c == cat, &pal).on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(a) = &mut this.apps {
                                    a.category = c.clone();
                                    a.expanded = None;
                                }
                                cx.notify();
                            }))
                        }))),
                )
                .child(divider(&pal))
                .child(div().id("apps-body").flex_1().min_h_0().overflow_y_scroll().child(body)),
        )
    }

    fn app_card(&mut self, it: &AppItem, busy: bool, open: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let it1 = it.clone();
        let it2 = it.clone();
        let action: AnyElement = match it.kind {
            AppKind::Toolset => toggle(SharedString::from(format!("tg-{}", it.id)), it.connected, &pal).on_click(cx.listener(move |this, _, _, cx| this.app_action(&it1, cx))).into_any_element(),
            AppKind::Skill => div().h(px(24.)).px(px(8.)).flex().items_center().rounded_full().bg(pal.raised).text_size(pal.t(11.)).text_color(pal.muted).child("Installed").into_any_element(),
            _ if busy => button(SharedString::from(format!("busy-{}", it.id)), "…", Kind::Secondary, &pal).w(px(88.)).into_any_element(),
            _ if it.connected => button(SharedString::from(format!("mg-{}", it.id)), if open { "Close" } else { "Manage" }, Kind::Secondary, &pal)
                .w(px(88.))
                .on_click(cx.listener(move |this, _, _, cx| this.app_manage(&it2, cx)))
                .into_any_element(),
            _ => button(SharedString::from(format!("cn-{}", it.id)), if it.kind == AppKind::Mcp { "Install" } else { "Connect" }, Kind::Primary, &pal)
                .w(px(88.))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if it1.kind == AppKind::Mcp && !it1.requires.is_empty() {
                        this.app_manage(&it1, cx);
                    } else {
                        this.app_action(&it1, cx);
                    }
                }))
                .into_any_element(),
        };
        let seed = it.id.bytes().fold(11u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
        // Logos are fetched by the service (HTTPS, size cap, no credentials) and drawn from the
        // local cache; the letter tile shows until then or if the fetch fails.
        let logo_file = it.logo_url.as_ref().and_then(|url| {
            let f = self.media.get(url).cloned();
            if f.is_none() && self.media_requested.insert(url.clone()) {
                self.svc.send(hermes_core::Cmd::FetchLogo { url: url.clone() });
            }
            f
        });
        let logo: AnyElement = match (logo_file, it.kind) {
            (Some(file), _) => img(file).size(px(40.)).rounded(px(10.)).flex_none().into_any_element(),
            (None, AppKind::Toolset) => tile_icon_lg("wrench", &pal).into_any_element(),
            (None, AppKind::Skill) => tile_icon_lg("sparkles", &pal).into_any_element(),
            (None, AppKind::Mcp) => tile_icon_lg("plug", &pal).into_any_element(),
            (None, AppKind::Connector) => letter_tile(it.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default(), crate::theme::avatar_color(None, seed), 40., false).into_any_element(),
        };
        div()
            .w(px(350.))
            .h(px(72.))
            .px(px(12.))
            .rounded(px(12.))
            .border_1()
            .border_color(if open { pal.border_strong } else { pal.border })
            .bg(pal.panel)
            .flex()
            .items_center()
            .gap(px(12.))
            .child(logo)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().flex().items_center().gap(px(8.)).child(div().min_w_0().truncate().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child(it.name.clone())).when(it.kind == AppKind::Mcp, |d| d.child(tag("MCP", &pal))))
                    .child(div().truncate().text_size(pal.t(12.)).text_color(if it.attention.is_some() { pal.warn } else { pal.muted }).child(it.attention.clone().unwrap_or_else(|| it.description.clone()))),
            )
            .child(action)
    }

    fn app_detail(&mut self, it: &AppItem, tools: Option<&crate::app::ToolRows>, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let it1 = it.clone();
        let it2 = it.clone();
        let mut d = div().w_full().p(px(16.)).rounded(px(12.)).bg(pal.raised).flex().flex_col().gap(px(12.)).text_size(pal.t(13.));
        match it.kind {
            AppKind::Connector => {
                d = d.child(div().text_color(pal.muted).child(it.description.clone()));
                d = d.child(section_label("TOOLS THIS APP GIVES THE BOT", &pal));
                d = match tools {
                    None => d.child(skeleton("tools-sk", relative(0.7).into(), 14., &pal)),
                    Some(Err(e)) => d.child(div().text_color(pal.faint).child(e.clone())),
                    Some(Ok(t)) if t.is_empty() => d.child(div().text_color(pal.faint).child("No tools listed.")),
                    Some(Ok(t)) => d.children(t.iter().take(12).map(|(n, desc, facet)| {
                        div().flex().items_center().gap(px(8.)).child(tag(facet, &pal)).child(div().font_weight(FontWeight::MEDIUM).child(n.clone())).child(div().min_w_0().truncate().text_color(pal.muted).child(desc.clone()))
                    })),
                };
                d = d.child(div().flex().gap(px(8.)).child(button("reconnect", "Reconnect", Kind::Secondary, &pal).on_click(cx.listener(move |this, _, _, cx| this.app_action(&it1, cx)))));
            }
            AppKind::Mcp => {
                d = d.child(div().text_color(pal.muted).child(it.description.clone()));
                if !it.requires.is_empty() {
                    d = d.child(div().text_color(pal.muted).child(format!(
                        "Needs {} set on the Hermes host (in the profile's .env, or hermes mcp add). Secrets are never sent from this app.",
                        it.requires.join(", ")
                    )));
                }
                d = d.child(div().flex().gap(px(8.)).child(if it.connected {
                    button("mcp-remove", "Remove", Kind::Danger, &pal).on_click(cx.listener(move |this, _, _, cx| this.app_action(&it2, cx)))
                } else {
                    button("mcp-install", "Install anyway", Kind::Primary, &pal).on_click(cx.listener(move |this, _, _, cx| this.app_action(&it2, cx)))
                }));
            }
            _ => {}
        }
        d
    }

    // ---- forms -------------------------------------------------------------------------------------

    fn render_new_bot(&mut self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let pal = self.pal;
        let f = self.new_bot.as_ref()?;
        let server = self.selected.as_ref().map(|k| self.conn_name(&k.conn)).or_else(|| self.config.connections.first().map(|c| c.name.clone())).unwrap_or_default();
        Some(
            self.dismiss_scrim("newbot-scrim", cx).child(
                sheet(460., &pal)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .p(px(24.))
                    .gap(px(16.))
                    .child(div().flex().flex_col().gap(px(4.)).child(div().text_size(pal.t(18.)).font_weight(FontWeight::SEMIBOLD).child("New bot")).child(
                        div().text_size(pal.t(13.)).text_color(pal.muted).child(format!("Creates a Hermes profile on {server} (profiles.create), so the bot also exists in the CLI and the official apps.")),
                    ))
                    .child(field("Name", &f.title, &pal))
                    .child(field("Profile id (optional)", &f.name, &pal))
                    .child(field("Description", &f.description, &pal))
                    .child(
                        div()
                            .pt(px(8.))
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(button("cancel-bot", "Cancel", Kind::Secondary, &pal).on_click(cx.listener(|this, _, _, cx| {
                                this.new_bot = None;
                                cx.notify();
                            })))
                            .child(button("create-bot", "Create bot", Kind::Primary, &pal).on_click(cx.listener(|this, _, _, cx| this.create_bot(cx)))),
                    ),
            ),
        )
    }
}

fn act_icon(a: Act) -> &'static str {
    match a {
        Act::NewBot => "plus",
        Act::Usage => "chart-column",
        Act::Pin => "pin",
        Act::Sidebar => "panel-left",
        Act::Settings => "settings",
        Act::Apps => "layout-grid",
        Act::Info => "panel-right",
        Act::Share => "copy",
        Act::Theme => "sun",
        Act::Hidden => "eye",
        Act::Compact => "refresh-cw",
        Act::AddServer => "server",
        Act::BotChat => "arrow-left",
    }
}

fn tile_icon(name: &'static str, pal: &Palette) -> Div {
    div().size(px(28.)).flex_none().rounded(px(8.)).bg(pal.raised).border_1().border_color(pal.border).flex().items_center().justify_center().child(ico(name, 14., pal.muted))
}

fn tile_icon_lg(name: &'static str, pal: &Palette) -> Div {
    div().size(px(40.)).flex_none().rounded(px(10.)).bg(pal.accent_soft).flex().items_center().justify_center().child(ico(name, 18., pal.accent))
}

fn tag(text: &str, pal: &Palette) -> Div {
    div().h(px(18.)).px(px(6.)).flex().flex_none().items_center().rounded(px(4.)).bg(pal.hover).text_size(px(10.)).font_weight(FontWeight::MEDIUM).text_color(pal.muted).child(text.to_owned())
}

fn group(title: &'static str, pal: &Palette) -> Div {
    div().pt(px(16.)).flex().flex_col().child(div().pb(px(4.)).child(section_label(title, pal)))
}

/// Settings row: title and description on the left, controls on the right, at least 56 px tall.
fn row(title: &str, desc: &str, pal: &Palette) -> Div {
    div()
        .min_h(px(56.))
        .py(px(12.))
        .flex()
        .items_center()
        .gap(px(12.))
        .border_t_1()
        .border_color(pal.border)
        .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child(title.to_owned())).when(!desc.is_empty(), |d| d.child(div().text_size(pal.t(12.)).text_color(pal.muted).child(desc.to_owned()))))
}

fn app_card_skeleton(i: usize, pal: &Palette) -> impl IntoElement {
    div()
        .w(px(350.))
        .h(px(72.))
        .px(px(12.))
        .rounded(px(12.))
        .border_1()
        .border_color(pal.border)
        .flex()
        .items_center()
        .gap(px(12.))
        .child(div().size(px(40.)).rounded(px(10.)).bg(pal.skeleton))
        .child(div().flex_1().flex().flex_col().gap(px(8.)).child(skeleton(SharedString::from(format!("ak-a{i}")), relative(0.4).into(), 12., pal)).child(skeleton(SharedString::from(format!("ak-b{i}")), relative(0.8).into(), 10., pal)))
}

/// "2026-11-16" → "16 Nov".
fn short_day(d: &str) -> String {
    chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map(|x| x.format("%-d %b").to_string()).unwrap_or_else(|_| d.to_owned())
}
