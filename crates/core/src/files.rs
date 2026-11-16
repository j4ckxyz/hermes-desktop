//! Safety rules for things that leave the app: links from chat text, agent files handed to the
//! OS, and names of downloaded files. Pure functions, so they are unit-tested on every platform.

use std::path::{Path, PathBuf};

/// What a click on a link in chat text may do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAction {
    /// `http`, `https` and `mailto` open in the default browser or mail app.
    Open(String),
    /// Anything else (`file:`, `smb:`, `shortcuts:`, `search-ms:`, custom app schemes,
    /// relative links) is never handed to the OS. The UI copies it and says so.
    Blocked { scheme: String },
}

pub fn link_action(url: &str) -> LinkAction {
    let t = url.trim();
    match url::Url::parse(t) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty()) => LinkAction::Open(u.to_string()),
        Ok(u) if u.scheme() == "mailto" => LinkAction::Open(u.to_string()),
        Ok(u) => LinkAction::Blocked { scheme: u.scheme().to_owned() },
        Err(_) => LinkAction::Blocked { scheme: String::new() },
    }
}

/// What "Open" may do with a file the agent produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    /// An inert document type: open with the default app.
    Open,
    /// Anything that could run (apps, scripts, installers, shortcuts, HTML, unknown types):
    /// show it in the file manager and let the user decide.
    Reveal,
}

/// Document types that the OS opens in a viewer rather than running. Deliberately excludes
/// `svg`/`html` (scripts in a browser), Office macro formats, archives and disk images.
const INERT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff", "heic", "heif", "avif", "ico", //
    "pdf", "txt", "md", "markdown", "csv", "tsv", "json", "log", "yaml", "yml", "toml", "rtf", //
    "mp3", "wav", "m4a", "aac", "ogg", "oga", "opus", "flac", //
    "mp4", "m4v", "mov", "webm", "mkv", "avi",
];

pub fn open_action(path: &Path) -> OpenAction {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    match ext {
        Some(e) if INERT.contains(&e.as_str()) => OpenAction::Open,
        _ => OpenAction::Reveal,
    }
}

/// A file name that is valid and harmless on Windows, macOS and Linux: the last path component
/// of `remote`, with separators, reserved characters and controls replaced, trailing dots and
/// spaces trimmed, Windows device names prefixed, and the length capped (at a char boundary).
pub fn safe_file_name(remote: &str) -> String {
    let last = remote.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut s: String = last.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') { '_' } else { c }).collect();
    s = s.trim_end_matches(['.', ' ']).trim_start().to_owned();
    if s.is_empty() || s == "." || s == ".." {
        return "file".into();
    }
    let stem = s.split('.').next().unwrap_or_default().to_ascii_uppercase();
    const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"];
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    const MAX: usize = 180;
    if s.len() > MAX {
        let (stem, ext) = match s.rsplit_once('.') {
            Some((a, b)) if b.len() <= 16 => (a.to_owned(), format!(".{b}")),
            _ => (s.clone(), String::new()),
        };
        let mut cut = MAX - ext.len();
        while !stem.is_char_boundary(cut) {
            cut -= 1;
        }
        s = format!("{}{ext}", &stem[..cut]);
    }
    s
}

/// `name`, or `name (2)` etc. when it exists.
pub fn unique(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(p)
}

/// A private temporary name next to `target` (never shared by two downloads).
pub fn part_path(target: &Path) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.part", uuid::Uuid::new_v4().simple()))
}

/// Mark a downloaded file as coming from the network so Gatekeeper (macOS quarantine) and
/// SmartScreen (Windows mark-of-the-web) check it before it can run. Best effort.
pub fn mark_downloaded(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        // flags;timestamp;agent;uuid, as written by browsers.
        let value = format!("0081;{secs:08x};Hermes Desktop;{}", uuid::Uuid::new_v4().to_string().to_uppercase());
        let (Ok(p), Ok(name)) = (std::ffi::CString::new(path.as_os_str().as_bytes()), std::ffi::CString::new("com.apple.quarantine")) else { return };
        // SAFETY: valid NUL-terminated strings and a byte buffer that outlives the call.
        let r = unsafe { libc::setxattr(p.as_ptr(), name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
        if r != 0 {
            log::debug!("quarantine flag not set on {}", path.display());
        }
    }
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        let _ = std::fs::write(ads, "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = path;
}

/// Whether `path` carries the quarantine / mark-of-the-web flag (tests).
pub fn is_marked(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let (Ok(p), Ok(name)) = (std::ffi::CString::new(path.as_os_str().as_bytes()), std::ffi::CString::new("com.apple.quarantine")) else { return false };
        // SAFETY: a null buffer asks only for the size.
        unsafe { libc::getxattr(p.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, 0) > 0 }
    }
    #[cfg(windows)]
    {
        let mut ads = path.as_os_str().to_owned();
        ads.push(":Zone.Identifier");
        std::path::Path::new(&ads).exists()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = path;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_and_mail_links_open() {
        assert_eq!(link_action("https://example.com/a?b"), LinkAction::Open("https://example.com/a?b".into()));
        assert!(matches!(link_action("http://homelab:9119"), LinkAction::Open(_)));
        assert!(matches!(link_action("mailto:a@example.com"), LinkAction::Open(_)));
        for bad in ["file:///Users/x/run.command", "smb://evil/share", "shortcuts://run-shortcut?name=x", "search-ms:query=x", "vscode://file/x", "javascript:alert(1)", "/etc/passwd", "report.pdf", "http://", ""] {
            assert!(matches!(link_action(bad), LinkAction::Blocked { .. }), "{bad}");
        }
        assert_eq!(link_action("FILE:///x"), LinkAction::Blocked { scheme: "file".into() });
    }

    #[test]
    fn only_inert_files_open() {
        for ok in ["a.PNG", "r.pdf", "notes.md", "song.mp3", "clip.mov", "data.csv"] {
            assert_eq!(open_action(Path::new(ok)), OpenAction::Open, "{ok}");
        }
        for bad in ["x.command", "setup.exe", "a.lnk", "b.desktop", "c.sh", "d.app", "e.html", "f.svg", "g.docm", "h.dmg", "i.zip", "noext", "j.pdf.exe", "k.scpt", "l.jar", "m.bat", "n.ps1"] {
            assert_eq!(open_action(Path::new(bad)), OpenAction::Reveal, "{bad}");
        }
    }

    #[test]
    fn file_names_are_portable() {
        assert_eq!(safe_file_name("/home/hermes/out/report.pdf"), "report.pdf");
        assert_eq!(safe_file_name("C:\\x\\a:b<c>.txt"), "a_b_c_.txt");
        assert_eq!(safe_file_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_file_name("lpt1"), "_lpt1");
        assert_eq!(safe_file_name("trailing. . "), "trailing");
        assert_eq!(safe_file_name("/x/"), "file");
        assert_eq!(safe_file_name(".."), "file");
        assert_eq!(safe_file_name("a\u{0}b\nc"), "a_b_c");
        let long = format!("{}.pdf", "é".repeat(200));
        let s = safe_file_name(&long);
        assert!(s.len() <= 180 && s.ends_with(".pdf"));
    }

    #[test]
    fn part_files_never_collide() {
        let t = Path::new("/tmp/report.pdf");
        let (a, b) = (part_path(t), part_path(t));
        assert_ne!(a, b);
        assert_ne!(part_path(Path::new("/tmp/report.docx")).file_name(), a.file_name());
        assert!(a.to_string_lossy().ends_with(".part"));
    }

    #[test]
    fn downloads_get_the_quarantine_flag() {
        let dir = std::env::temp_dir().join(format!("hd-q-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.txt");
        std::fs::write(&f, b"hi").unwrap();
        mark_downloaded(&f);
        assert!(is_marked(&f));
        let _ = std::fs::remove_dir_all(dir);
    }
}
