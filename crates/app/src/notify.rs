//! OS notifications and the reply sound.
//!
//! Notifications go through `notify-rust`, which talks to each platform natively: the macOS
//! notification centre, freedesktop D-Bus on Linux (no `notify-send` process), and WinRT toasts
//! on Windows. Text is never pasted into a shell or script, so model output can't run commands.
//! Delivery happens on a short-lived thread because some backends block until the toast is
//! accepted; nothing is left running or unreaped afterwards.

/// What to do with a finished reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alert {
    pub mark_unread: bool,
    pub notify: bool,
    pub sound: bool,
}

/// `selected`: the reply is for the bot on screen. `window_active`: Hermes has focus.
pub fn decide(selected: bool, window_active: bool, notifications: bool, only_unfocused: bool, sounds: bool) -> Alert {
    let visible = selected && window_active;
    if visible {
        return Alert { mark_unread: false, notify: false, sound: false };
    }
    Alert { mark_unread: !selected, notify: notifications && (!only_unfocused || !window_active), sound: sounds }
}

/// Plain one-line text for a notification: control characters (including bidi overrides) become
/// spaces and the result is cut to `max` characters on a character boundary.
pub fn clean(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max * 4));
    let mut n = 0;
    let mut last_space = true;
    for c in s.chars() {
        let c = if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}') { ' ' } else { c };
        if c == ' ' && last_space {
            continue;
        }
        if n == max {
            out.truncate(out.trim_end().len());
            out.push('…');
            return out;
        }
        last_space = c == ' ';
        out.push(c);
        n += 1;
    }
    out.trim_end().to_owned()
}

/// Freedesktop notification servers may interpret a small HTML subset in the body.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Show a notification (and play the notification sound with it when `sound`).
pub fn post(title: &str, body: &str, sound: bool) {
    let title = clean(title, 80);
    let body = clean(body, 160);
    std::thread::Builder::new()
        .name("notify".into())
        .spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.summary(&title).appname("Hermes");
            #[cfg(all(unix, not(target_os = "macos")))]
            {
                n.body(&escape_markup(&body));
                if sound {
                    n.sound_name("message-new-instant");
                }
            }
            #[cfg(not(all(unix, not(target_os = "macos"))))]
            {
                n.body(&body);
                if sound {
                    n.sound_name(if cfg!(target_os = "macos") { "Tink" } else { "Default" });
                }
            }
            if let Err(e) = n.show() {
                log::warn!("notification failed: {e}");
            }
        })
        .ok();
}

/// The reply sound on its own (notifications off, sounds on). The helper is waited for on a
/// background thread, so no zombie process is left behind.
pub fn play_sound() {
    #[cfg(target_os = "macos")]
    let cmd = Some(("afplay", vec!["/System/Library/Sounds/Tink.aiff"]));
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = Some(("canberra-gtk-play", vec!["-i", "message-new-instant"]));
    #[cfg(windows)]
    let cmd: Option<(&str, Vec<&str>)> = None;
    match cmd {
        Some((prog, args)) => {
            std::thread::Builder::new()
                .name("sound".into())
                .spawn(move || {
                    if let Ok(mut c) = std::process::Command::new(prog).args(&args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn() {
                        let _ = c.wait();
                    }
                })
                .ok();
        }
        // Windows: a silent toast carries the default sound without spawning anything.
        None => post("Hermes", "New message", true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_text_is_flattened_not_interpreted() {
        // Curly quotes and newlines survive only as text; nothing here reaches a shell.
        let s = clean("it’s fine’; Start-Process calc; ‘\r\nnext\u{202e}line", 200);
        assert_eq!(s, "it’s fine’; Start-Process calc; ‘ next line");
        assert!(!s.chars().any(char::is_control));
        assert_eq!(escape_markup("<b>a & b</b>"), "&lt;b&gt;a &amp; b&lt;/b&gt;");
    }

    #[test]
    fn truncates_on_char_boundaries() {
        let s = clean(&"é".repeat(300), 10);
        assert_eq!(s.chars().count(), 11);
        assert!(s.ends_with('…'));
        assert_eq!(clean("-n starts with a dash", 50), "-n starts with a dash");
    }

    #[test]
    fn alerts_follow_focus_and_prefs() {
        // On screen and focused: nothing.
        assert_eq!(decide(true, true, true, true, true), Alert { mark_unread: false, notify: false, sound: false });
        // Selected bot but Hermes in the background: notify (was silently dropped before).
        assert_eq!(decide(true, false, true, true, true), Alert { mark_unread: false, notify: true, sound: true });
        // Another bot while focused: unread, and notify only if not "only when unfocused".
        assert_eq!(decide(false, true, true, true, false), Alert { mark_unread: true, notify: false, sound: false });
        assert_eq!(decide(false, true, true, false, false), Alert { mark_unread: true, notify: true, sound: false });
        // Notifications off.
        assert!(!decide(false, false, false, false, true).notify);
    }
}
