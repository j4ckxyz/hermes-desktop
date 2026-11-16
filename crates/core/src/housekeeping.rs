//! Keeps local data bounded. Run once at startup, off the UI thread.
//!
//! * The log rolls over to `hermes-desktop.log.1` past 5 MB (one generation kept).
//! * Pasted images (temporary copies made for an attachment) go after a day.
//! * The media and logo caches drop files older than 30 days, then the oldest until each is
//!   under its size cap.
//! * Cached chats for servers that were removed are deleted.

use std::path::Path;
use std::time::{Duration, SystemTime};

pub const LOG_MAX: u64 = 5 * 1024 * 1024;
pub const PASTED_TTL: Duration = Duration::from_secs(24 * 3600);
pub const CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
pub const MEDIA_CAP: u64 = 256 * 1024 * 1024;
pub const LOGO_CAP: u64 = 16 * 1024 * 1024;

/// Roll `log` over when it's larger than `max`. Call before the logger opens the file.
pub fn rotate_log(log: &Path, max: u64) {
    if std::fs::metadata(log).is_ok_and(|m| m.len() > max) {
        let mut old = log.as_os_str().to_owned();
        old.push(".1");
        let _ = std::fs::rename(log, old);
    }
}

/// Delete files older than `ttl`, then the oldest until the directory holds at most `cap` bytes.
/// Returns how many files were removed.
pub fn trim_dir(dir: &Path, ttl: Duration, cap: u64) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    let now = SystemTime::now();
    let mut files: Vec<(SystemTime, u64, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some((m.modified().unwrap_or(now), m.len(), e.path()))
        })
        .collect();
    let mut removed = 0;
    files.retain(|(t, _, p)| {
        let stale = now.duration_since(*t).is_ok_and(|age| age > ttl);
        if stale && std::fs::remove_file(p).is_ok() {
            removed += 1;
        }
        !stale
    });
    files.sort_by_key(|(t, _, _)| *t);
    let mut total: u64 = files.iter().map(|(_, n, _)| n).sum();
    for (_, n, p) in files {
        if total <= cap {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= n;
            removed += 1;
        }
    }
    removed
}

/// Everything above. `cache` is the service's cache directory (`<data>/cache`).
pub fn run(_data: &Path, cache: &Path, store: &crate::store::Store, conns: &[String]) {
    trim_dir(&cache.join("pasted"), PASTED_TTL, u64::MAX);
    trim_dir(&cache.join("media"), CACHE_TTL, MEDIA_CAP);
    trim_dir(&cache.join("logos"), CACHE_TTL, LOGO_CAP);
    if let Err(e) = store.forget_other_conns(conns) {
        log::warn!("cache prune: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_and_logs_stay_bounded() {
        let dir = std::env::temp_dir().join(format!("hd-hk-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("app.log");
        std::fs::write(&log, vec![b'x'; 64]).unwrap();
        rotate_log(&log, 32);
        assert!(!log.exists() && dir.join("app.log.1").exists());
        rotate_log(&log, 32); // nothing to do
        for i in 0..5 {
            let f = dir.join(format!("m{i}"));
            std::fs::write(&f, vec![0u8; 100]).unwrap();
            let t = SystemTime::now() - Duration::from_secs(100 * (5 - i));
            std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        }
        // Over the cap: the oldest go first (m0..m2; the rolled log is newest).
        assert_eq!(trim_dir(&dir, Duration::from_secs(10_000), 250 + 64), 3);
        assert!(dir.join("m4").exists() && dir.join("m3").exists() && !dir.join("m0").exists());
        // Past the age limit: m3 (200 s old) goes, m4 (100 s) stays.
        assert_eq!(trim_dir(&dir, Duration::from_secs(150), u64::MAX), 1);
        assert!(dir.join("m4").exists() && !dir.join("m3").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
