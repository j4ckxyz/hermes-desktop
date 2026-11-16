//! First run and "Add server": welcome → choose a server (found on this computer, or typed with a
//! live health check) → sign in with a username and password like the official web dashboard (or
//! a session token under Advanced) → pick or create a first bot.
//!
//! The protocol lives in `hermes_core::auth`; this file is only the flow and its looks. Passwords
//! and tokens go straight from the input to `Cmd::SignIn` and from there to the OS keychain.

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::input::{Input, InputEvent, InputState};
use hermes_core::auth::ServerInfo;
use hermes_core::conn::{ConnKind, Connection};
use hermes_core::gateway::LinkState;
use hermes_core::roster::{self, BotKey};
use hermes_core::service::{Cmd, SignInMethod};

use crate::app::HermesApp;
use crate::theme::Palette;
use crate::ui::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    Welcome,
    Server,
    SignIn,
    Token,
    Bot,
}

impl Step {
    pub fn parse(s: &str) -> Step {
        match s {
            "server" => Step::Server,
            "signin" => Step::SignIn,
            "token" => Step::Token,
            "bot" => Step::Bot,
            _ => Step::Welcome,
        }
    }
    fn index(self) -> usize {
        match self {
            Step::Welcome => 0,
            Step::Server => 1,
            Step::SignIn | Step::Token => 2,
            Step::Bot => 3,
        }
    }
}

#[derive(Clone)]
pub enum Probe {
    Idle,
    Checking,
    Ok(ServerInfo),
    Err(String),
}

pub struct Onboard {
    pub step: Step,
    /// No servers yet: the flow fills the window and can't be dismissed.
    pub first_run: bool,
    /// The connection being created, or re-signed-in / switched when it already exists.
    pub conn_id: String,
    pub existing: bool,
    pub url: Entity<InputState>,
    pub username: Entity<InputState>,
    pub password: Entity<InputState>,
    pub token: Entity<InputState>,
    pub bot_title: Entity<InputState>,
    pub show_password: bool,
    pub remember: bool,
    /// The user accepted sending credentials over plain HTTP to this address.
    pub allow_http: bool,
    /// `None` while scanning this computer.
    pub local: Option<Vec<ServerInfo>>,
    pub probe: Probe,
    pub probe_gen: u64,
    pub server: Option<ServerInfo>,
    pub busy: bool,
    pub error: Option<String>,
    pub identity: Option<String>,
    subs: Vec<Subscription>,
}

/// "homelab.tail1234.ts.net" → "Homelab"; loopback → "This computer".
fn server_name(info: &ServerInfo) -> String {
    if info.is_loopback() {
        return "This computer".into();
    }
    let host = info.base.host_str().unwrap_or("Hermes");
    if host.parse::<std::net::IpAddr>().is_ok() {
        return host.to_owned();
    }
    let first = host.split('.').next().unwrap_or(host);
    let mut c = first.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_else(|| "Hermes".into())
}

/// The address as shown to the user: the host for HTTPS, with `http://` spelled out otherwise so
/// an unencrypted connection is never hidden.
fn host_label(info: &ServerInfo) -> String {
    let u = &info.base;
    let host = match (u.host_str(), u.port()) {
        (Some(h), Some(p)) => format!("{h}:{p}"),
        (Some(h), None) => h.to_owned(),
        _ => return u.as_str().to_owned(),
    };
    if u.scheme() == "https" {
        host
    } else {
        format!("{}://{host}", u.scheme())
    }
}

fn spinner(id: &'static str, size: f32, color: impl Into<Hsla>) -> impl IntoElement {
    ico("loader-circle", size, color).with_animation(id, Animation::new(Duration::from_millis(900)).repeat(), |s, t| s.with_transformation(Transformation::rotate(percentage(t))))
}

impl HermesApp {
    /// Open the flow. `conn` re-signs-in (`Step::SignIn`) or switches the address (`Step::Server`)
    /// of a saved server.
    pub fn open_onboarding(&mut self, step: Step, conn: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlays(cx);
        let existing = conn.as_ref().and_then(|id| self.config.connections.iter().find(|c| c.id == *id).cloned());
        let conn_id = existing.as_ref().map(|c| c.id.clone()).unwrap_or_else(|| format!("c{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let account = existing.as_ref().and_then(|c| self.svc.store.get_kv(&format!("account:{}", c.id))).filter(|a| !a.is_empty() && a != "This computer" && a != "Session token");
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("homelab.tail1234.ts.net or https://hermes.example.com").default_value(existing.as_ref().map(|c| c.base_url.clone()).unwrap_or_default()));
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Username").default_value(account.unwrap_or_default()));
        let password = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Password"));
        let token = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Session token"));
        let bot_title = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Research Buddy"));
        let mut subs = vec![
            cx.subscribe_in(&url, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Change => this.schedule_probe(cx),
                InputEvent::PressEnter { .. } => this.onboard_next(window, cx),
                _ => {}
            }),
            cx.subscribe_in(&token, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.onboard_next(window, cx);
                }
            }),
            cx.subscribe_in(&bot_title, window, |this, _, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. }) {
                    this.onboard_next(window, cx);
                }
            }),
        ];
        subs.push(cx.subscribe_in(&username, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                // Enter in the username moves on to the password, unless both are filled.
                let pw_empty = this.onboard.as_ref().is_some_and(|o| o.password.read(cx).value().is_empty());
                if pw_empty {
                    if let Some(o) = &this.onboard {
                        o.password.update(cx, |s, cx| s.focus(window, cx));
                    }
                } else {
                    this.onboard_next(window, cx);
                }
            }
        }));
        self.onboard = Some(Onboard {
            step,
            first_run: self.config.connections.is_empty(),
            conn_id,
            existing: existing.is_some(),
            url,
            username,
            password,
            token,
            bot_title,
            show_password: false,
            // Refresh tokens already renew for 30 days; keeping the password is opt-in.
            remember: false,
            allow_http: false,
            local: None,
            probe: Probe::Idle,
            probe_gen: 0,
            server: None,
            busy: false,
            error: None,
            identity: None,
            subs,
        });
        self.watch_password(window, cx);
        self.svc.send(Cmd::DetectLocal);
        if existing.is_some() {
            // Re-check the saved address so the sign-in step knows its provider.
            self.schedule_probe(cx);
        }
        self.focus_step(window, cx);
        cx.notify();
    }

    fn watch_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        let pw = o.password.clone();
        let sub = cx.subscribe_in(&pw, window, |this, _, ev: &InputEvent, window, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.onboard_next(window, cx);
            }
        });
        o.subs.push(sub);
    }

    /// Show or hide the password (rebuilds the field with the same text and caret at the end).
    pub fn toggle_show_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        o.show_password = !o.show_password;
        let masked = !o.show_password;
        let value = o.password.read(cx).value().to_string();
        let end = value.len();
        let pw = cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder("Password").default_value(value));
        pw.update(cx, |s, cx| {
            s.set_selected_range(end..end, cx);
            s.focus(window, cx);
        });
        o.password = pw;
        self.watch_password(window, cx);
        cx.notify();
    }

    fn focus_step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &self.onboard else { return };
        let target = match o.step {
            Step::Welcome => None,
            Step::Server => Some(o.url.clone()),
            Step::SignIn if o.username.read(cx).value().is_empty() => Some(o.username.clone()),
            Step::SignIn => Some(o.password.clone()),
            Step::Token => Some(o.token.clone()),
            Step::Bot => Some(o.bot_title.clone()),
        };
        match target {
            Some(t) => t.update(cx, |s, cx| s.focus(window, cx)),
            None => window.focus(&self.focus, cx),
        }
    }

    pub fn set_onboard_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(o) = &mut self.onboard {
            o.step = step;
            o.error = None;
            if step == Step::Server && o.local.is_none() {
                self.svc.send(Cmd::DetectLocal);
            }
        }
        self.focus_step(window, cx);
        cx.notify();
    }

    /// Debounced live health check of the typed address.
    pub fn schedule_probe(&mut self, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        o.probe_gen += 1;
        o.allow_http = false;
        o.server = None;
        let gen = o.probe_gen;
        let input = o.url.read(cx).value().trim().to_owned();
        if input.is_empty() {
            o.probe = Probe::Idle;
            cx.notify();
            return;
        }
        o.probe = Probe::Checking;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(450)).await;
            let _ = this.update(cx, |app, _| {
                if app.onboard.as_ref().is_some_and(|o| o.probe_gen == gen) {
                    app.svc.send(Cmd::Probe { input, tag: format!("ob:{gen}") });
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub fn on_probed(&mut self, tag: &str, result: Result<ServerInfo, String>, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        if tag != format!("ob:{}", o.probe_gen) {
            return; // stale: the address changed since
        }
        o.probe = match result {
            Ok(info) => {
                o.server = Some(info.clone());
                Probe::Ok(info)
            }
            Err(e) => Probe::Err(e),
        };
        cx.notify();
    }

    pub fn on_local_servers(&mut self, servers: Vec<ServerInfo>, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        o.local = Some(servers);
        cx.notify();
    }

    /// Choose a server found on this computer.
    pub fn pick_local(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        let Some(info) = o.local.as_ref().and_then(|l| l.get(i)).cloned() else { return };
        o.probe_gen += 1;
        o.allow_http = false;
        o.url.update(cx, |s, cx| s.set_value(info.base.as_str().trim_end_matches('/').to_owned(), window, cx));
        o.server = Some(info.clone());
        o.probe = Probe::Ok(info);
        cx.notify();
    }

    /// The primary action of the current step (also Enter).
    pub fn onboard_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        if o.busy {
            return;
        }
        match o.step {
            Step::Welcome => self.set_onboard_step(Step::Server, window, cx),
            Step::Server => {
                let Some(info) = o.server.clone() else {
                    if matches!(o.probe, Probe::Idle) {
                        o.error = Some("Enter the address you open Hermes at, or pick one found on this computer.".into());
                    }
                    cx.notify();
                    return;
                };
                if info.auth_required && info.insecure() && !o.allow_http {
                    o.error = Some(format!("{} isn't encrypted, so your password could be read on the network. Use its HTTPS or Tailscale address, or confirm below.", host_label(&info)));
                    cx.notify();
                    return;
                }
                if !info.auth_required {
                    // A dashboard on loopback has no accounts: it hands its session token to
                    // whoever loads the page from this machine.
                    self.begin_sign_in(SignInMethod::Loopback, cx);
                } else if info.password_provider().is_some() && info.native {
                    self.set_onboard_step(Step::SignIn, window, cx);
                } else {
                    self.set_onboard_step(Step::Token, window, cx);
                }
            }
            Step::SignIn => {
                let username = o.username.read(cx).value().trim().to_owned();
                let password = o.password.read(cx).value().to_string();
                if username.is_empty() || password.is_empty() {
                    o.error = Some("Enter your username and password.".into());
                    cx.notify();
                    return;
                }
                let provider = o.server.as_ref().and_then(|s| s.password_provider()).map(|p| p.name.clone()).unwrap_or_else(|| "basic".into());
                let remember = o.remember;
                self.begin_sign_in(SignInMethod::Password { provider, username, password, remember }, cx);
            }
            Step::Token => {
                let token = o.token.read(cx).value().trim().to_owned();
                if token.is_empty() {
                    o.error = Some("Paste the session token first.".into());
                    cx.notify();
                    return;
                }
                self.begin_sign_in(SignInMethod::Token { token }, cx);
            }
            Step::Bot => {
                let title = o.bot_title.read(cx).value().trim().to_owned();
                let conn = o.conn_id.clone();
                if title.is_empty() {
                    self.finish_onboarding(cx);
                    return;
                }
                let name = roster::slug(&title, true);
                self.want_select = Some(BotKey::new(&conn, &name));
                self.svc.send(Cmd::CreateBot { conn, name, title: title.clone(), description: String::new() });
                self.show_toast(format!("Creating {title}…"), false, cx);
                self.finish_onboarding(cx);
            }
        }
    }

    fn begin_sign_in(&mut self, method: SignInMethod, cx: &mut Context<Self>) {
        let Some(o) = &mut self.onboard else { return };
        let Some(info) = o.server.clone() else { return };
        o.busy = true;
        o.error = None;
        self.svc.send(Cmd::SignIn { conn_id: o.conn_id.clone(), base: info.base.to_string(), method, tag: "onboard".into() });
        cx.notify();
    }

    pub fn on_signed_in(&mut self, tag: &str, conn_id: String, result: Result<String, String>, cx: &mut Context<Self>) {
        if tag != "onboard" {
            return;
        }
        let Some(o) = &mut self.onboard else { return };
        if o.conn_id != conn_id {
            return;
        }
        o.busy = false;
        let identity = match result {
            Ok(i) => i,
            Err(e) => {
                o.error = Some(e);
                if o.step == Step::SignIn {
                    // Keep the username, select the password for a retry.
                    o.password.update(cx, |s, cx| {
                        let n = s.value().len();
                        s.set_selected_range(0..n, cx);
                    });
                }
                cx.notify();
                return;
            }
        };
        let Some(info) = o.server.clone() else { return };
        o.identity = Some(identity.clone());
        // Clear secrets from the form as soon as they're in the keychain.
        let existing = o.existing;
        let name = self.config.connections.iter().find(|c| c.id == conn_id).map(|c| c.name.clone()).unwrap_or_else(|| server_name(&info));
        let conn = Connection { id: conn_id.clone(), name, base_url: info.base.as_str().trim_end_matches('/').to_owned(), kind: ConnKind::Gateway };
        if let Some(i) = self.config.connections.iter().position(|c| c.id == conn_id) {
            self.config.connections[i] = conn.clone();
        } else {
            self.config.connections.push(conn.clone());
        }
        let _ = self.svc.store.set_kv(&format!("account:{conn_id}"), &identity);
        let _ = self.svc.store.set_kv(&format!("signedout:{conn_id}"), "");
        self.save_config(cx);
        self.svc.send(Cmd::Connect(conn.clone()));
        if existing {
            self.show_toast(format!("Signed in to {} as {identity}.", conn.name), false, cx);
            self.onboard = None;
        } else if let Some(o) = &mut self.onboard {
            o.step = Step::Bot;
            o.error = None;
            o.first_run = false;
        }
        cx.notify();
    }

    pub fn finish_onboarding(&mut self, cx: &mut Context<Self>) {
        self.onboard = None;
        cx.notify();
    }

    /// Settings → Account: forget this server's credentials but keep it in the list.
    pub fn sign_out(&mut self, id: &str, cx: &mut Context<Self>) {
        self.svc.send(Cmd::SignOut { conn_id: id.to_owned() });
        let _ = self.svc.store.set_kv(&format!("signedout:{id}"), "1");
        self.links.insert(id.to_owned(), LinkState::Unauthorized);
        self.bots.retain(|b| b.key.conn != id);
        if self.selected.as_ref().is_some_and(|k| k.conn == id) {
            self.selected = None;
        }
        let name = self.conn_name(id);
        self.show_toast(format!("Signed out of {name}."), false, cx);
        cx.notify();
    }

    // ---- rendering ---------------------------------------------------------------------------

    pub fn render_onboarding(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pal = self.pal;
        let o = self.onboard.as_ref()?;
        let first_run = o.first_run;
        let card = div()
            .w(px(440.))
            .max_w(relative(0.94))
            .flex()
            .flex_col()
            .gap(px(20.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.render_onboard_step(cx));
        let wrapped = if first_run {
            div().id("onboard").absolute().inset_0().bg(pal.bg).flex().flex_col().items_center().justify_center().occlude().child(card).into_any_element()
        } else {
            scrim("onboard-scrim", &pal)
                .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                    if this.onboard.as_ref().is_some_and(|o| !o.busy) {
                        this.finish_onboarding(cx);
                    }
                }))
                .child(sheet(480., &pal).p(px(28.)).child(card))
                .into_any_element()
        };
        Some(wrapped)
    }

    fn render_onboard_step(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let step = o.step;
        let dots = div().flex().justify_center().gap(px(6.)).children((0..4).map(|i| {
            let on = i == step.index();
            div().h(px(6.)).w(px(if on { 18. } else { 6. })).rounded_full().bg(if on { pal.accent } else { pal.border_strong })
        }));
        let error = o.error.clone();
        let busy = o.busy;
        let body: AnyElement = match step {
            Step::Welcome => self.render_welcome(cx).into_any_element(),
            Step::Server => self.render_server_step(cx).into_any_element(),
            Step::SignIn => self.render_signin_step(cx).into_any_element(),
            Step::Token => self.render_token_step(cx).into_any_element(),
            Step::Bot => self.render_bot_step(cx).into_any_element(),
        };
        let (primary, enabled) = match step {
            Step::Welcome => ("Get started", true),
            Step::Server => ("Continue", self.onboard.as_ref().is_some_and(|o| o.server.is_some())),
            Step::SignIn => ("Sign in", true),
            Step::Token => ("Connect", true),
            Step::Bot => (if self.onboard.as_ref().is_some_and(|o| !o.bot_title.read(cx).value().trim().is_empty()) { "Create bot" } else { "Done" }, true),
        };
        let back = match step {
            Step::Server if self.onboard.as_ref().is_some_and(|o| o.first_run) => Some(Step::Welcome),
            Step::SignIn | Step::Token => Some(Step::Server),
            _ => None,
        };
        let can_close = !self.onboard.as_ref().unwrap().first_run && step != Step::Bot;
        div()
            .flex()
            .flex_col()
            .gap(px(20.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(div().w(px(32.)).when_some(back, |d, b| d.child(icon_btn("ob-back", "arrow-left", &pal).on_click(cx.listener(move |this, _, window, cx| this.set_onboard_step(b, window, cx))))))
                    .child(div().flex_1().child(dots))
                    .child(div().w(px(32.)).when(can_close, |d| d.child(icon_btn("ob-close", "x", &pal).on_click(cx.listener(|this, _, _, cx| this.finish_onboarding(cx)))))),
            )
            .child(body)
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .id("ob-error")
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(10.))
                        .bg(pal.danger_soft)
                        .flex()
                        .gap(px(8.))
                        .child(ico("circle-alert", 16., pal.danger))
                        .child(div().flex_1().text_size(pal.t(13.)).text_color(pal.danger).child(e)),
                )
            })
            .child(
                button("ob-next", primary, Kind::Primary, &pal)
                    .w_full()
                    .h(px(40.))
                    .text_size(pal.t(14.))
                    .when(!enabled || busy, |d| d.opacity(0.5))
                    .when(busy, |d| d.child(spinner("ob-busy", 14., pal.on_accent)).flex_row_reverse())
                    .on_click(cx.listener(|this, _, window, cx| this.onboard_next(window, cx))),
            )
            .child(self.render_onboard_footer(cx))
            .into_any_element()
    }

    fn render_onboard_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let link = |id: &'static str, label: &'static str| div().id(id).text_size(pal.t(13.)).text_color(pal.muted).cursor_pointer().hover(|s| s.text_color(pal.text)).child(label);
        div().flex().justify_center().text_size(pal.t(12.)).text_color(pal.faint).map(|d| match o.step {
            Step::Welcome => d.child("Your conversations stay on your Hermes. Nothing goes through us."),
            Step::Server => d.child(link("ob-advanced", "Have a session token? Use it instead").on_click(cx.listener(|this, _, window, cx| {
                if this.onboard.as_ref().is_some_and(|o| o.server.is_some()) {
                    this.set_onboard_step(Step::Token, window, cx);
                } else if let Some(o) = &mut this.onboard {
                    o.error = Some("Choose a server first, then paste its token.".into());
                    cx.notify();
                }
            }))),
            Step::SignIn => d.child(link("ob-token", "Use a session token instead").on_click(cx.listener(|this, _, window, cx| this.set_onboard_step(Step::Token, window, cx)))),
            Step::Token => d.child(link("ob-password", "Sign in with a password instead").on_click(cx.listener(|this, _, window, cx| this.set_onboard_step(Step::SignIn, window, cx)))),
            Step::Bot => d.child(link("ob-skip", "Skip for now").on_click(cx.listener(|this, _, _, cx| this.finish_onboarding(cx)))),
        })
    }

    fn render_welcome(&mut self, _cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let feature = |icon: &'static str, title: &'static str, text: &'static str| {
            div()
                .flex()
                .gap(px(12.))
                .child(div().size(px(32.)).flex_none().rounded(px(8.)).bg(pal.accent_soft).flex().items_center().justify_center().child(ico(icon, 16., pal.accent)))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(2.)).child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child(title)).child(div().text_size(pal.t(13.)).text_color(pal.muted).child(text)))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.))
                    .child(div().size(px(64.)).rounded(px(18.)).bg(pal.accent).flex().items_center().justify_center().shadow_lg().child(ico("bot", 32., pal.on_accent)))
                    .child(div().text_size(pal.t(26.)).font_weight(FontWeight::BOLD).child("Welcome to Hermes"))
                    .child(div().text_size(pal.t(14.)).text_color(pal.muted).text_center().child("A fast, native home for your Hermes agents.")),
            )
            .child(
                div()
                    .p(px(16.))
                    .rounded(px(14.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .child(feature("messages-square", "Every bot, one sidebar", "Each Hermes profile is a bot with its own chat."))
                    .child(feature("server", "Your servers", "This computer, a homelab or a VPS over Tailscale, side by side."))
                    .child(feature("shield-check", "Signed in like the dashboard", "Use your Hermes username and password. Credentials stay in the system keychain.")),
            )
    }

    fn render_server_step(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let url = o.url.clone();
        let chosen = o.server.as_ref().map(|s| s.base.clone());
        let local = o.local.clone();
        let typed_local = local.as_ref().is_some_and(|l| l.iter().any(|s| Some(&s.base) == chosen.as_ref()));
        let probe = o.probe.clone();
        let insecure = o.server.as_ref().is_some_and(|s| s.auth_required && s.insecure());
        let allow_http = o.allow_http;
        let title = if o.existing { "Switch server" } else { "Choose your Hermes" };
        let status: Option<AnyElement> = match (&probe, typed_local) {
            (_, true) | (Probe::Idle, _) => None,
            (Probe::Checking, _) => Some(div().flex().items_center().gap(px(6.)).text_color(pal.muted).child(spinner("ob-probe", 13., pal.muted)).child("Checking…").into_any_element()),
            (Probe::Ok(info), _) => Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(pal.ok)
                    .child(ico("check", 14., pal.ok))
                    .child(format!("Hermes {} found · {}", info.version, if info.auth_required { "sign-in required" } else { "no sign-in needed" }))
                    .into_any_element(),
            ),
            (Probe::Err(e), _) => Some(div().w_full().flex().items_start().gap(px(6.)).text_color(pal.danger).child(ico("circle-alert", 14., pal.danger)).child(div().flex_1().min_w_0().child(e.clone())).into_any_element()),
        };
        let server_card = |i: usize, info: &ServerInfo, on: bool| {
            div()
                .id(SharedString::from(format!("ob-local-{i}")))
                .p(px(12.))
                .rounded(px(12.))
                .border_1()
                .border_color(if on { pal.accent } else { pal.border })
                .bg(if on { pal.accent_soft } else { pal.panel })
                .cursor_pointer()
                .hover(|s| s.border_color(pal.border_strong))
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().size(px(36.)).flex_none().rounded(px(10.)).bg(pal.raised).border_1().border_color(pal.border).flex().items_center().justify_center().child(ico("laptop", 18., pal.muted)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).child(format!("This computer · {}", host_label(info))))
                        .child(div().text_size(pal.t(12.)).text_color(pal.muted).child(format!("Hermes {} · {}", info.version, if info.auth_required { "sign-in required" } else { "ready, no sign-in needed" }))),
                )
                .when(on, |d| d.child(ico("check", 16., pal.accent)))
                .on_click(cx.listener(move |this, _, window, cx| this.pick_local(i, window, cx)))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(div().flex().flex_col().gap(px(6.)).child(div().text_size(pal.t(22.)).font_weight(FontWeight::BOLD).child(title)).child(div().text_size(pal.t(14.)).text_color(pal.muted).child("Connect to the Hermes you run with hermes dashboard: on this computer, a homelab or a VPS.")))
            .child(
                div().flex().flex_col().gap(px(8.)).child(section_label("ON THIS COMPUTER", &pal)).map(|d| match &local {
                    None => d.child(div().h(px(62.)).px(px(12.)).rounded(px(12.)).border_1().border_color(pal.border).flex().items_center().gap(px(8.)).text_size(pal.t(13.)).text_color(pal.muted).child(spinner("ob-scan", 14., pal.muted)).child("Looking for Hermes on ports 9119 and 9120…")),
                    Some(l) if l.is_empty() => d.child(div().px(px(12.)).py(px(12.)).rounded(px(12.)).border_1().border_color(pal.border).text_size(pal.t(13.)).text_color(pal.muted).child("No Hermes running here. Start one with hermes dashboard, or enter a remote address below.")),
                    Some(l) => d.children(l.iter().enumerate().map(|(i, s)| server_card(i, s, Some(&s.base) == chosen.as_ref()))),
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(section_label("REMOTE OR TAILSCALE", &pal))
                    .child(Input::new(&url).prefix(ico("globe", 14., pal.faint)))
                    .child(div().w_full().min_h(px(18.)).text_size(pal.t(12.)).when_some(status, |d, s| d.child(s)))
                    .when(insecure, |d| {
                        d.child(
                            div()
                                .id("ob-allow-http")
                                .p(px(10.))
                                .rounded(px(10.))
                                .bg(pal.danger_soft)
                                .flex()
                                .items_start()
                                .gap(px(10.))
                                .cursor_pointer()
                                .child(checkbox(allow_http, &pal))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(div().text_size(pal.t(13.)).text_color(pal.danger).child("Not encrypted"))
                                        .child(div().text_size(pal.t(12.)).text_color(pal.muted).child("This address uses plain HTTP, so anyone on the network could read your password. Prefer its HTTPS or Tailscale address. Tick to send it anyway.")),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(o) = &mut this.onboard {
                                        o.allow_http = !o.allow_http;
                                        o.error = None;
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
    }

    fn render_signin_step(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let host = o.server.as_ref().map(host_label).unwrap_or_default();
        let provider = o.server.as_ref().and_then(|s| s.password_provider()).filter(|p| p.name != "basic").map(|p| p.display_name.clone()).filter(|d| !d.is_empty());
        let (user, pw, show, remember) = (o.username.clone(), o.password.clone(), o.show_password, o.remember);
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().text_size(pal.t(22.)).font_weight(FontWeight::BOLD).child("Sign in to Hermes"))
                    .child(div().text_size(pal.t(14.)).text_color(pal.muted).child(format!("Use the same username and password as the Hermes dashboard at {host}."))),
            )
            .child(div().flex().flex_col().gap(px(8.)).child(label_text("Username", &pal)).child(Input::new(&user).prefix(ico("user", 14., pal.faint))))
            .child(
                div().flex().flex_col().gap(px(8.)).child(label_text("Password", &pal)).child(
                    Input::new(&pw).prefix(ico("lock", 14., pal.faint)).suffix(
                        div()
                            .id("ob-showpw")
                            .cursor_pointer()
                            .child(ico(if show { "eye-off" } else { "eye" }, 15., pal.muted))
                            .on_click(cx.listener(|this, _, window, cx| this.toggle_show_password(window, cx))),
                    ),
                ),
            )
            .child(
                div()
                    .id("ob-remember")
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .child(checkbox(remember, &pal))
                    .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_size(pal.t(13.)).child("Stay signed in")).child(div().text_size(pal.t(12.)).text_color(pal.muted).child("Sessions renew on their own for 30 days. Tick to also keep your password in the system keychain, so Hermes can sign you back in after that or after a password reset elsewhere.")))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(o) = &mut this.onboard {
                            o.remember = !o.remember;
                        }
                        cx.notify();
                    })),
            )
            .when_some(provider, |d, p| d.child(div().text_size(pal.t(12.)).text_color(pal.faint).child(format!("Signing in with {p}."))))
    }

    fn render_token_step(&mut self, _cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let token = o.token.clone();
        let host = o.server.as_ref().map(host_label).unwrap_or_default();
        let no_password = o.server.as_ref().is_some_and(|s| s.auth_required && (s.password_provider().is_none() || !s.native));
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().text_size(pal.t(22.)).font_weight(FontWeight::BOLD).child("Use a session token"))
                    .child(div().text_size(pal.t(14.)).text_color(pal.muted).child(if no_password {
                        format!("{host} doesn't offer password sign-in for apps. Paste its dashboard session token (HERMES_DASHBOARD_SESSION_TOKEN) instead.")
                    } else {
                        format!("Advanced: paste the dashboard session token for {host} (HERMES_DASHBOARD_SESSION_TOKEN on the server).")
                    })),
            )
            .child(div().flex().flex_col().gap(px(8.)).child(label_text("Session token", &pal)).child(Input::new(&token).mask_toggle().prefix(ico("key-round", 14., pal.faint))))
            .child(div().text_size(pal.t(12.)).text_color(pal.faint).child("Stored in the system keychain, never in a file."))
    }

    fn render_bot_step(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let pal = self.pal;
        let o = self.onboard.as_ref().unwrap();
        let conn = o.conn_id.clone();
        let who = o.identity.clone().unwrap_or_default();
        let title_in = o.bot_title.clone();
        let host = o.server.as_ref().map(host_label).unwrap_or_default();
        let bots: Vec<_> = self.bots.iter().filter(|b| b.key.conn == conn && !b.hidden()).cloned().collect();
        let connected = self.links.get(&conn) == Some(&LinkState::Connected);
        let signed = if who.is_empty() || who == "This computer" { format!("Connected to {host}.") } else { format!("Signed in to {host} as {who}.") };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(div().flex().items_center().gap(px(8.)).child(div().size(px(22.)).rounded_full().bg(pal.ok).flex().items_center().justify_center().child(ico("check", 13., pal.on_accent))).child(div().text_size(pal.t(13.)).text_color(pal.muted).child(signed)))
                    .child(div().text_size(pal.t(22.)).font_weight(FontWeight::BOLD).child("Pick your first bot")),
            )
            .child(
                div()
                    .id("ob-bots")
                    .max_h(px(220.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .map(|d| {
                        if bots.is_empty() && !connected {
                            d.child(div().h(px(52.)).flex().items_center().gap(px(8.)).text_size(pal.t(13.)).text_color(pal.muted).child(spinner("ob-bots-load", 14., pal.muted)).child("Loading your bots…"))
                        } else if bots.is_empty() {
                            d.child(div().text_size(pal.t(13.)).text_color(pal.muted).child("No bots yet. Name one below to create it."))
                        } else {
                            d.children(bots.iter().map(|b| {
                                let key = b.key.clone();
                                div()
                                    .id(SharedString::from(format!("ob-bot-{}", b.name)))
                                    .px(px(10.))
                                    .py(px(8.))
                                    .rounded(px(10.))
                                    .border_1()
                                    .border_color(pal.border)
                                    .cursor_pointer()
                                    .hover(|s| s.bg(pal.hover))
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(avatar_for(Some(b), &b.name, 32.))
                                    .child(div().flex_1().min_w_0().flex().flex_col().child(div().text_size(pal.t(14.)).font_weight(FontWeight::MEDIUM).truncate().child(b.title())).child(div().text_size(pal.t(12.)).text_color(pal.muted).truncate().child(b.description.clone().unwrap_or_default())))
                                    .child(ico("chevron-right", 14., pal.faint))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.finish_onboarding(cx);
                                        this.select(key.clone(), window, cx);
                                    }))
                            }))
                        }
                    }),
            )
            .child(div().flex().flex_col().gap(px(8.)).child(label_text("Or create a new bot", &pal)).child(Input::new(&title_in).prefix(ico("plus", 14., pal.faint))))
    }
}

fn label_text(text: &'static str, pal: &Palette) -> Div {
    div().text_size(pal.t(12.)).font_weight(FontWeight::MEDIUM).text_color(pal.muted).child(text)
}

fn checkbox(on: bool, pal: &Palette) -> Div {
    div()
        .size(px(18.))
        .flex_none()
        .rounded(px(5.))
        .border_1()
        .border_color(if on { pal.accent } else { pal.border_strong })
        .bg(if on { pal.accent } else { pal.input_bg })
        .flex()
        .items_center()
        .justify_center()
        .when(on, |d| d.child(ico("check", 12., pal.on_accent)))
}

#[cfg(test)]
mod tests {
    use super::{host_label, server_name, ServerInfo};

    fn info(u: &str) -> ServerInfo {
        ServerInfo { base: hermes_core::Url::parse(u).unwrap(), version: "1".into(), auth_required: true, native: true, providers: vec![] }
    }

    #[test]
    fn names_servers_from_their_host() {
        assert_eq!(server_name(&info("http://127.0.0.1:9119")), "This computer");
        assert_eq!(server_name(&info("https://homelab.tail1234.ts.net")), "Homelab");
        assert_eq!(server_name(&info("http://100.64.0.7:9119")), "100.64.0.7");
        assert_eq!(host_label(&info("https://homelab.tail1234.ts.net")), "homelab.tail1234.ts.net");
        assert_eq!(host_label(&info("http://192.168.1.5:9119")), "http://192.168.1.5:9119");
    }
}
