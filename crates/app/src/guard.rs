//! The only paths by which chat content reaches the OS: link clicks and "Open" on agent files.
//! Rules live in `hermes_core::files`; this applies them.

use std::path::Path;
use std::sync::Mutex;

use gpui::*;
use hermes_core::files::{self, LinkAction, OpenAction};

/// Messages for the toast, queued by handlers that only have `&mut App` (markdown link clicks)
/// and shown on the next frame by `HermesApp::render`.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn take_pending() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(|e| e.into_inner()))
}

fn say(msg: String, window: &mut Window) {
    PENDING.lock().unwrap_or_else(|e| e.into_inner()).push(msg);
    window.refresh();
}

/// Link click handler for every markdown view: web and mail links open, anything else is copied
/// to the clipboard instead of being handed to the OS.
pub fn on_link(url: &SharedString, _: &ClickEvent, window: &mut Window, cx: &mut App) {
    match files::link_action(url) {
        LinkAction::Open(u) => cx.open_url(&u),
        LinkAction::Blocked { scheme } => {
            cx.write_to_clipboard(ClipboardItem::new_string(url.to_string()));
            let what = if scheme.is_empty() { "That link".to_owned() } else { format!("A {scheme}: link") };
            say(format!("{what} wasn't opened: only web and mail links open from chats. It's on your clipboard."), window);
        }
    }
}

/// "Open" for a file from the agent: inert document types open in their viewer, anything that
/// could run is shown in the file manager instead.
pub fn open_file(path: &Path, cx: &mut App) -> Option<String> {
    match files::open_action(path) {
        OpenAction::Open => {
            cx.open_with_system(path);
            None
        }
        OpenAction::Reveal => {
            cx.reveal_path(path);
            Some(format!("{} could run code, so it's shown in its folder instead of opened.", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
        }
    }
}
