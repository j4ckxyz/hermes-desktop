//! Shared building blocks. Every control is a fixed height on the 4/8 px grid (24, 28, 32, 36),
//! uses one radius scale (6, 8, 12, 16) and has hover and pressed states.

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use hermes_core::roster::Bot;

use crate::assets::icon;
use crate::theme::{avatar_color, Palette};

pub type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub fn ico(name: &'static str, size: f32, color: impl Into<Hsla>) -> Svg {
    svg().path(icon(name)).size(px(size)).flex_none().text_color(color.into())
}

/// What an icon-only button does, for its tooltip and accessible name.
pub fn icon_label(name: &str) -> &'static str {
    match name {
        "x" => "Close",
        "arrow-left" => "Back",
        "chart-column" => "Usage",
        "download" => "Save",
        "external-link" => "Open",
        "eye" => "Hide hidden bots",
        "eye-off" => "Show hidden bots",
        "layout-grid" => "Apps & Connectors",
        "panel-left" => "Toggle sidebar",
        "panel-right" => "Bot info",
        "plus" => "New bot",
        "search" => "Search",
        "settings" => "Settings",
        "share" => "Share",
        "trash-2" => "Remove server",
        "minus" => "Minimise",
        "square" | "copy" => "Maximise",
        _ => "Button",
    }
}

/// Keyboard and screen-reader access for a clickable div: a tab stop (Enter / Space click it),
/// a visible focus ring, the button role and an accessible name.
pub fn accessible(el: Stateful<Div>, label: impl Into<SharedString>, pal: &Palette) -> Stateful<Div> {
    let ring = pal.accent;
    el.tab_index(0).role(Role::Button).aria_label(label).focus_visible(move |s| s.border_2().border_color(ring))
}

/// Square icon button: 32 px hit target, 16 px glyph, centred on both axes, with a tooltip.
pub fn icon_btn(id: impl Into<ElementId>, name: &'static str, pal: &Palette) -> Stateful<Div> {
    icon_btn_sized(id, name, 32., 16., pal)
}

pub fn icon_btn_sized(id: impl Into<ElementId>, name: &'static str, box_size: f32, glyph: f32, pal: &Palette) -> Stateful<Div> {
    let (hover, pressed) = (pal.hover, pal.pressed);
    let label = icon_label(name);
    let el = div()
        .id(id.into())
        .size(px(box_size))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .hover(move |s| s.bg(hover))
        .active(move |s| s.bg(pressed))
        .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(label).build(window, cx))
        .child(ico(name, glyph, pal.muted));
    accessible(el, label, pal)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind, pal: &Palette) -> Stateful<Div> {
    let label: SharedString = label.into();
    let ring = pal.accent;
    let (bg, fg, border) = match kind {
        Kind::Primary => (pal.accent, pal.on_accent, pal.accent),
        Kind::Secondary => (pal.raised, pal.text, pal.border),
        Kind::Ghost => (rgba(0), pal.text, rgba(0)),
        Kind::Danger => (pal.danger_soft, pal.danger, pal.danger_soft),
    };
    let (hover, pressed) = (pal.hover, pal.pressed);
    div()
        .id(id.into())
        .h(px(32.))
        .px(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .rounded(px(8.))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(fg)
        .text_size(pal.t(13.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .when(kind == Kind::Primary, |d| d.hover(|s| s.opacity(0.9)).active(|s| s.opacity(0.78)))
        .when(kind != Kind::Primary, move |d| d.hover(move |s| s.bg(hover)).active(move |s| s.bg(pressed)))
        .tab_index(0)
        .role(Role::Button)
        .aria_label(label.clone())
        .focus_visible(move |s| s.border_2().border_color(ring))
        .child(label)
}

pub fn chip(id: impl Into<ElementId>, label: impl Into<SharedString>, selected: bool, pal: &Palette) -> Stateful<Div> {
    let (hover, pressed) = (pal.hover, pal.pressed);
    div()
        .id(id.into())
        .h(px(28.))
        .px(px(12.))
        .flex()
        .flex_none()
        .items_center()
        .rounded_full()
        .text_size(pal.t(12.))
        .font_weight(FontWeight::MEDIUM)
        .cursor_pointer()
        .border_1()
        .when(selected, |d| d.bg(pal.text).text_color(pal.bg).border_color(pal.text))
        .when(!selected, move |d| d.bg(pal.raised).text_color(pal.muted).border_color(pal.border).hover(move |s| s.bg(hover)).active(move |s| s.bg(pressed)))
        .child(label.into())
}

/// A 36×20 switch.
pub fn toggle(id: impl Into<ElementId>, on: bool, pal: &Palette) -> Stateful<Div> {
    let ring = pal.accent;
    div()
        .id(id.into())
        .tab_index(0)
        .role(Role::Switch)
        .aria_toggled(if on { gpui::Toggled::True } else { gpui::Toggled::False })
        .focus_visible(move |s| s.border_2().border_color(ring))
        .w(px(36.))
        .h(px(20.))
        .flex_none()
        .rounded_full()
        .p(px(2.))
        .flex()
        .when(on, |d| d.justify_end())
        .bg(if on { pal.accent } else { pal.border_strong })
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .child(div().size(px(16.)).rounded_full().bg(rgb(0xffffff)).shadow_sm())
}

/// Segmented control: equal-height options inside one rounded track.
pub fn segmented(id: &str, items: Vec<(SharedString, bool, Handler)>, pal: &Palette) -> Div {
    let (hover, sel_bg) = (pal.hover, if pal.dark { pal.pressed } else { pal.panel });
    div().h(px(32.)).p(px(2.)).flex().flex_none().gap(px(2.)).rounded(px(8.)).bg(pal.raised).border_1().border_color(pal.border).children(items.into_iter().enumerate().map(
        move |(i, (label, on, h))| {
            div()
                .id(SharedString::from(format!("{id}-{i}")))
                .h_full()
                .px(px(12.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .text_size(pal.t(12.))
                .font_weight(FontWeight::MEDIUM)
                .cursor_pointer()
                .when(on, |d| d.bg(sel_bg).text_color(pal.text).shadow_sm())
                .when(!on, move |d| d.text_color(pal.muted).hover(move |s| s.bg(hover)))
                .on_click(move |e, w, cx| h(e, w, cx))
                .child(label)
        },
    ))
}

pub fn kbd(text: impl Into<SharedString>, pal: &Palette) -> Div {
    div()
        .h(px(20.))
        .min_w(px(20.))
        .px(px(6.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .bg(pal.raised)
        .border_1()
        .border_color(pal.border)
        .text_size(px(11.))
        .text_color(pal.muted)
        .child(text.into())
}

/// Human label for a key binding, e.g. `secondary-shift-a` → "⇧⌘A" on macOS, "Ctrl+Shift+A" elsewhere.
pub fn keys(binding: &str) -> String {
    let parts: Vec<&str> = binding.split('-').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);
    let key = match key[0] {
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "enter" => "↩".to_owned(),
        "escape" => "Esc".to_owned(),
        "tab" => "Tab".to_owned(),
        "," => ",".to_owned(),
        k => k.to_uppercase(),
    };
    if cfg!(target_os = "macos") {
        let mut s = String::new();
        for m in ["ctrl", "alt", "shift", "secondary", "cmd"] {
            if mods.contains(&m) {
                s.push_str(match m {
                    "ctrl" => "⌃",
                    "alt" => "⌥",
                    "shift" => "⇧",
                    _ => "⌘",
                });
            }
        }
        s + &key
    } else {
        let mut v: Vec<String> = vec![];
        for m in ["secondary", "ctrl", "alt", "shift"] {
            if mods.contains(&m) {
                v.push(match m {
                    "secondary" | "ctrl" => "Ctrl",
                    "alt" => "Alt",
                    _ => "Shift",
                }
                .to_owned());
            }
        }
        v.dedup();
        v.push(key);
        v.join("+")
    }
}

/// Pulsing placeholder block for loading states.
pub fn skeleton(id: impl Into<ElementId>, w: Length, h: f32, pal: &Palette) -> impl IntoElement {
    div().w(w).h(px(h)).flex_none().rounded(px(6.)).bg(pal.skeleton).with_animation(
        id.into(),
        Animation::new(std::time::Duration::from_millis(1400)).repeat(),
        |d, t| d.opacity(0.45 + 0.55 * ((t * std::f32::consts::TAU).cos() * 0.5 + 0.5)),
    )
}

pub fn section_label(text: impl Into<SharedString>, pal: &Palette) -> Div {
    div().h(px(24.)).flex().items_center().text_size(pal.t(11.)).font_weight(FontWeight::SEMIBOLD).text_color(pal.faint).child(text.into())
}

pub fn avatar_for(bot: Option<&Bot>, fallback: &str, size: f32) -> Div {
    let (color, initials) = match bot {
        Some(b) => (avatar_color(b.color_hex(), b.color_seed()), b.initials()),
        None => (
            avatar_color(None, fallback.bytes().fold(7u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))),
            fallback.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or("?".into()),
        ),
    };
    letter_tile(initials, color, size, true)
}

/// Coloured square/circle with centred initials (avatars, app logos).
pub fn letter_tile(text: impl Into<SharedString>, color: impl Into<Hsla>, size: f32, round: bool) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .when(round, |d| d.rounded_full())
        .when(!round, |d| d.rounded(px((size * 0.25).round())))
        .bg(color.into())
        .flex()
        .items_center()
        .justify_center()
        .text_color(rgb(0xffffff))
        .text_size(px((size * 0.4).round().max(10.)))
        .font_weight(FontWeight::SEMIBOLD)
        .line_height(px(size))
        .child(text.into())
}

/// Full-window scrim that centres its child; clicks on the scrim call `on_dismiss`.
pub fn scrim(id: &'static str, pal: &Palette) -> Stateful<Div> {
    div().id(id).absolute().inset_0().bg(pal.scrim).flex().items_center().justify_center().occlude()
}

pub fn sheet(w: f32, pal: &Palette) -> Div {
    div().w(px(w)).max_w(relative(0.94)).rounded(px(16.)).bg(pal.panel).border_1().border_color(pal.border).shadow_lg().flex().flex_col().overflow_hidden()
}

pub fn divider(pal: &Palette) -> Div {
    div().h(px(1.)).w_full().flex_none().bg(pal.border)
}

pub fn fmt_time(ts: Option<f64>) -> String {
    let Some(ts) = ts.filter(|t| *t > 0.0) else { return String::new() };
    let Some(t) = chrono::DateTime::from_timestamp(ts as i64, 0) else { return String::new() };
    let local = t.with_timezone(&chrono::Local);
    let now = chrono::Local::now();
    if local.date_naive() == now.date_naive() {
        local.format("%H:%M").to_string()
    } else if (now - local).num_days() < 7 {
        local.format("%a").to_string()
    } else {
        local.format("%-d %b").to_string()
    }
}

pub fn fmt_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn key_labels() {
        if cfg!(target_os = "macos") {
            assert_eq!(super::keys("secondary-shift-a"), "⇧⌘A");
            assert_eq!(super::keys("secondary-,"), "⌘,");
        } else {
            assert_eq!(super::keys("secondary-shift-a"), "Ctrl+Shift+A");
        }
    }
}
