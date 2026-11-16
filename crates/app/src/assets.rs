//! A handful of Lucide icons (ISC licence, see assets/icons/LICENSE), embedded in the binary.

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_bytes!(concat!("../assets/icons/", $name, ".svg")) as &[u8])),*]
    };
}

static ICONS: &[(&str, &[u8])] = icons![
    "plus", "search", "arrow-up", "mic", "share", "settings", "paperclip", "file-text", "eye", "eye-off", "copy",
    "download", "bot", "check", "x", "loader-circle", "wrench", "external-link", "square", "users", "sparkles",
    "server", "palette", "bell", "message-circle", "folder", "keyboard", "shield", "sliders-horizontal", "info",
    "chevron-left", "chevron-right", "pencil", "plug", "arrow-left", "messages-square", "panel-right", "command",
    "sun", "moon", "trash-2", "refresh-cw", "circle-alert", "layout-grid", "clock", "globe", "terminal", "send",
    "image", "brain", "chevron-down", "shield-check", "shield-x", "history",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let name = path.trim_start_matches("icons/").trim_end_matches(".svg");
        Ok(ICONS.iter().find(|(n, _)| *n == name).map(|(_, b)| Cow::Borrowed(*b)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS.iter().map(|(n, _)| SharedString::from(format!("icons/{n}.svg"))).filter(|p| p.starts_with(path)).collect())
    }
}

/// Path of an embedded icon for `svg().path(...)`.
pub fn icon(name: &'static str) -> SharedString {
    SharedString::from(format!("icons/{name}.svg"))
}
