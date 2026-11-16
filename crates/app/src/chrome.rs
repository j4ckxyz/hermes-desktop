//! Window chrome where the platform doesn't draw it.
//!
//! * macOS: a transparent title bar with the traffic lights over our own header.
//! * Windows: the native title bar (caption buttons, drag, snap layouts).
//! * Linux: server-side decorations are requested. Compositors that refuse them (GNOME on
//!   Wayland) report client-side decorations; then this module draws a slim title bar with drag,
//!   double-click to maximise, the window menu on right-click, minimise / maximise / close, and
//!   resize handles along the edges.

use gpui::*;

use crate::theme::Palette;
use crate::ui::*;

pub const CSD_BAR_H: f32 = 34.;
const EDGE: f32 = 6.;

pub fn titlebar_options() -> TitlebarOptions {
    TitlebarOptions { title: Some("Hermes".into()), appears_transparent: cfg!(target_os = "macos"), traffic_light_position: Some(point(px(16.), px(16.))) }
}

pub fn window_decorations() -> Option<WindowDecorations> {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        Some(WindowDecorations::Server)
    } else {
        None
    }
}

/// Whether we must draw the title bar and resize handles ourselves.
pub fn needs_client_chrome(decorations: Decorations) -> bool {
    !cfg!(target_os = "macos") && matches!(decorations, Decorations::Client { .. })
}

fn tiled(d: Decorations) -> Tiling {
    match d {
        Decorations::Client { tiling } => tiling,
        Decorations::Server => Tiling { top: true, left: true, right: true, bottom: true },
    }
}

/// The title bar for client-side decorations.
pub fn csd_bar(window: &Window, pal: &Palette) -> impl IntoElement {
    let maximized = window.is_maximized();
    let control = |id: &'static str, icon: &'static str, area: WindowControlArea, pal: &Palette| {
        let (hover, danger) = (pal.hover, pal.danger_soft);
        div()
            .id(id)
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .window_control_area(area)
            .hover(move |s| s.bg(if area == WindowControlArea::Close { danger } else { hover }))
            .child(ico(icon, 14., pal.muted))
    };
    div()
        .id("csd-bar")
        .h(px(CSD_BAR_H))
        .w_full()
        .flex_none()
        .flex()
        .items_center()
        .px(px(10.))
        .gap(px(6.))
        .bg(pal.sidebar)
        .border_b_1()
        .border_color(pal.border)
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |ev, window, _| {
            if ev.click_count >= 2 {
                window.zoom_window();
            } else {
                window.start_window_move();
            }
        })
        .on_mouse_down(MouseButton::Right, |ev, window, _| window.show_window_menu(ev.position))
        .child(div().flex_1().text_size(pal.t(12.)).font_weight(FontWeight::MEDIUM).text_color(pal.muted).child("Hermes"))
        .child(control("csd-min", "minus", WindowControlArea::Min, pal).on_click(|_, window, _| window.minimize_window()))
        .child(control("csd-max", if maximized { "copy" } else { "square" }, WindowControlArea::Max, pal).on_click(|_, window, _| window.zoom_window()))
        .child(control("csd-close", "x", WindowControlArea::Close, pal).on_click(|_, window, _| window.remove_window()))
}

/// Invisible resize handles along untiled edges and corners.
pub fn resize_handles(window: &Window) -> Vec<AnyElement> {
    let t = tiled(window.window_decorations());
    let mut v = vec![];
    let mut edge = |id: &'static str, e: ResizeEdge, cursor: CursorStyle, f: fn(Div) -> Div| {
        v.push(
            f(div().absolute()).id(id)
                .cursor(cursor)
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    cx.stop_propagation();
                    window.start_window_resize(e);
                })
                .into_any_element(),
        );
    };
    if !t.top {
        edge("rz-t", ResizeEdge::Top, CursorStyle::ResizeUpDown, |d| d.top_0().left(px(EDGE)).right(px(EDGE)).h(px(EDGE / 2.)));
    }
    if !t.bottom {
        edge("rz-b", ResizeEdge::Bottom, CursorStyle::ResizeUpDown, |d| d.bottom_0().left(px(EDGE)).right(px(EDGE)).h(px(EDGE)));
    }
    if !t.left {
        edge("rz-l", ResizeEdge::Left, CursorStyle::ResizeLeftRight, |d| d.left_0().top(px(EDGE)).bottom(px(EDGE)).w(px(EDGE)));
    }
    if !t.right {
        edge("rz-r", ResizeEdge::Right, CursorStyle::ResizeLeftRight, |d| d.right_0().top(px(EDGE)).bottom(px(EDGE)).w(px(EDGE)));
    }
    if !t.top && !t.left {
        edge("rz-tl", ResizeEdge::TopLeft, CursorStyle::ResizeUpLeftDownRight, |d| d.top_0().left_0().size(px(EDGE * 2.)));
    }
    if !t.top && !t.right {
        edge("rz-tr", ResizeEdge::TopRight, CursorStyle::ResizeUpRightDownLeft, |d| d.top_0().right_0().size(px(EDGE * 2.)));
    }
    if !t.bottom && !t.left {
        edge("rz-bl", ResizeEdge::BottomLeft, CursorStyle::ResizeUpRightDownLeft, |d| d.bottom_0().left_0().size(px(EDGE * 2.)));
    }
    if !t.bottom && !t.right {
        edge("rz-br", ResizeEdge::BottomRight, CursorStyle::ResizeUpLeftDownRight, |d| d.bottom_0().right_0().size(px(EDGE * 2.)));
    }
    v
}

/// Wrap the app in client-side chrome when the platform needs it.
pub fn wrap(content: Div, window: &Window, pal: &Palette) -> AnyElement {
    if !needs_client_chrome(window.window_decorations()) {
        return content.into_any_element();
    }
    div()
        .size_full()
        .relative()
        .flex()
        .flex_col()
        .bg(pal.bg)
        .child(csd_bar(window, pal))
        .child(content.flex_1().min_h_0())
        .children(resize_handles(window))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn chrome_only_when_the_compositor_wants_it() {
        assert!(!needs_client_chrome(Decorations::Server));
        let csd = Decorations::Client { tiling: Tiling::default() };
        assert_eq!(needs_client_chrome(csd), !cfg!(target_os = "macos"));
        assert_eq!(titlebar_options().appears_transparent, cfg!(target_os = "macos"));
    }
}
