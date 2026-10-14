//! Output connection watcher (spec 5.2: survive a TV power cycle).
//!
//! SDL's KMSDRM backend does not report connector hotplug, so a thread
//! polls the kernel's connector status in sysfs
//! (`/sys/class/drm/card*-*/status`, a cached value: reading it does not
//! probe the display) and reports when the outputs go from some connected
//! to none, or back. TVs differ: many keep the HDMI hotplug line up in
//! standby (then nothing changes here), others drop it.
//!
//! It also watches the HDMI audio sinks' ELD (`/proc/asound/card*/eld#*`:
//! what the TV or receiver says it can play). That changes when the sink
//! goes away or renegotiates (TV or soundbar power cycle) even while the
//! video link stays up, and an audio stream opened before can stay silent
//! afterwards; the app reopens it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::PlatformEvent;

const POLL: Duration = Duration::from_secs(1);

/// Stops the thread when dropped.
pub struct Watcher {
    stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Connector status files under `root` (e.g. /sys/class/drm).
fn status_files(root: &Path) -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir(root) else { return Vec::new() };
    let mut v: Vec<PathBuf> = dir
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            // "card0-HDMI-A-1", not "card0" or "renderD128".
            n.starts_with("card") && n.contains('-')
        })
        .map(|e| e.path().join("status"))
        .filter(|p| p.exists())
        .collect();
    v.sort();
    v
}

/// Whether any connector reports "connected" (None: no connectors).
fn any_connected(files: &[PathBuf]) -> Option<bool> {
    if files.is_empty() {
        return None;
    }
    Some(files.iter().any(|f| std::fs::read_to_string(f).is_ok_and(|s| s.trim() == "connected")))
}

/// A fingerprint of every HDMI audio sink's ELD under `asound` (e.g.
/// /proc/asound); 0 when there are none.
fn eld_signature(asound: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut files: Vec<PathBuf> = std::fs::read_dir(asound)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("card"))
        .flat_map(|card| std::fs::read_dir(card.path()).into_iter().flatten().flatten())
        .filter(|e| e.file_name().to_string_lossy().starts_with("eld#"))
        .map(|e| e.path())
        .collect();
    if files.is_empty() {
        return 0;
    }
    files.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in files {
        f.hash(&mut h);
        std::fs::read(&f).unwrap_or_default().hash(&mut h);
    }
    h.finish()
}

/// Starts watching; None when there are no connectors to watch (e.g. not
/// a DRM system).
pub fn start(tx: Sender<PlatformEvent>, wake: Arc<dyn Fn() + Send + Sync>) -> Option<Watcher> {
    start_at(PathBuf::from("/sys/class/drm"), PathBuf::from("/proc/asound"), POLL, tx, wake)
}

fn start_at(
    root: PathBuf,
    asound: PathBuf,
    poll: Duration,
    tx: Sender<PlatformEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Option<Watcher> {
    let files = status_files(&root);
    let mut last = any_connected(&files)?;
    tracing::info!(connectors = files.len(), connected = last, "watching display connectors");
    let mut last_eld = eld_signature(&asound);
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    std::thread::Builder::new()
        .name("hotplug".into())
        .spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                std::thread::sleep(poll);
                let eld = eld_signature(&asound);
                if eld != last_eld {
                    last_eld = eld;
                    tracing::info!("HDMI audio sink changed");
                    if tx.send(PlatformEvent::AudioSinkChanged).is_err() {
                        return;
                    }
                    wake();
                }
                // Connectors can appear (MST, adapters): rescan each time.
                let Some(now) = any_connected(&status_files(&root)) else { continue };
                if now != last {
                    last = now;
                    if tx.send(PlatformEvent::OutputConnected { connected: now }).is_err() {
                        return;
                    }
                    wake();
                }
            }
        })
        .ok()?;
    Some(Watcher { stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_connect_and_disconnect() {
        let root = std::env::temp_dir().join(format!("jellyroll-hotplug-{}", std::process::id()));
        let conn = root.join("card0-HDMI-A-1");
        std::fs::create_dir_all(&conn).unwrap();
        std::fs::create_dir_all(root.join("renderD128")).unwrap();
        std::fs::write(conn.join("status"), "connected\n").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let asound = root.join("asound");
        let eld = asound.join("card0");
        std::fs::create_dir_all(&eld).unwrap();
        std::fs::write(eld.join("eld#0.0"), "monitor_present\t1\neld_valid\t1\n").unwrap();
        let w =
            start_at(root.clone(), asound, Duration::from_millis(10), tx, Arc::new(|| {})).unwrap();
        std::fs::write(conn.join("status"), "disconnected\n").unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ev, PlatformEvent::OutputConnected { connected: false }));
        std::fs::write(conn.join("status"), "connected\n").unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ev, PlatformEvent::OutputConnected { connected: true }));
        // The audio sink renegotiates while the video link stays up.
        std::fs::write(eld.join("eld#0.0"), "monitor_present\t1\neld_valid\t0\n").unwrap();
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(ev, PlatformEvent::AudioSinkChanged));
        drop(w);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_to_watch() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let missing = PathBuf::from("/nonexistent/drm");
        assert!(start_at(missing.clone(), missing, POLL, tx, Arc::new(|| {})).is_none());
    }
}
