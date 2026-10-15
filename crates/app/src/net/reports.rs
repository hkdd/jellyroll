//! Playback reports (spec 7.2): start, progress and stop, sent in order.
//! A report that can't be sent (offline, server restarting) stays queued
//! and is retried with backoff; the queue is saved to disk, so quitting
//! while offline doesn't lose where you stopped. Progress reports for the
//! same play session coalesce: only the latest position matters.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jellyfin_api::Error;
use jellyfin_api::playback::{Report, ReportKind};
use tokio::sync::{Notify, watch};

use super::fetch::Session;
use super::store::atomic_write;

const RETRY_MIN: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(120);
/// Unsent reports kept at most (oldest progress dropped first).
const MAX_QUEUED: usize = 200;

pub struct ReportQueue {
    items: Mutex<VecDeque<Report>>,
    notify: Notify,
    path: Option<PathBuf>,
}

impl ReportQueue {
    /// Opens the queue saved at `path` (reports left from a previous run).
    pub fn open(path: Option<PathBuf>) -> Self {
        let items: VecDeque<Report> = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if !items.is_empty() {
            tracing::info!(count = items.len(), "unsent playback reports from last time");
        }
        Self { items: Mutex::new(items), notify: Notify::new(), path }
    }

    pub fn push(&self, r: Report) {
        {
            let mut q = self.lock();
            let replaces = r.kind == ReportKind::Progress
                && q.back().is_some_and(|b| {
                    b.kind == ReportKind::Progress && b.play_session_id == r.play_session_id
                });
            if replaces {
                q.pop_back();
            }
            q.push_back(r);
            while q.len() > MAX_QUEUED {
                match q.iter().position(|x| x.kind == ReportKind::Progress) {
                    Some(i) => q.remove(i),
                    None => q.pop_front(),
                };
            }
            self.save(&q);
        }
        self.notify.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<Report>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The first report this user may send: theirs, or untagged.
    fn front_for(&self, user: &str) -> Option<Report> {
        self.lock().iter().find(|r| r.user_id.is_empty() || r.user_id == user).cloned()
    }

    /// Removes `sent` if it is still queued unchanged (a newer progress
    /// report may have replaced it meanwhile).
    fn done(&self, sent: &Report) {
        let mut q = self.lock();
        if let Some(i) = q.iter().position(|r| r == sent) {
            q.remove(i);
        }
        self.save(&q);
    }

    fn save(&self, q: &VecDeque<Report>) {
        let Some(p) = &self.path else { return };
        if q.is_empty() {
            let _ = std::fs::remove_file(p);
            return;
        }
        if let Err(e) = serde_json::to_vec(q)
            .map_err(std::io::Error::other)
            .and_then(|b| atomic_write(p, &b, 0o600))
        {
            tracing::warn!("{}: {e}", p.display());
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> Vec<Report> {
        self.lock().iter().cloned().collect()
    }
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("pending-reports.json")
}

/// Sends queued reports while signed in; retries failures with backoff.
pub async fn worker(q: Arc<ReportQueue>, mut session: watch::Receiver<Session>) {
    let mut delay = RETRY_MIN;
    loop {
        let signed = session.borrow_and_update().clone();
        let Some((client, user)) = signed else {
            // Wait to be signed in (again).
            let _ = session.changed().await;
            continue;
        };
        // Another user's reports wait until they are signed in again.
        let Some(r) = q.front_for(&user) else {
            tokio::select! {
                _ = q.notify.notified() => {}
                _ = session.changed() => {}
            }
            continue;
        };
        match client.send_no_content(&r.request()).await {
            Ok(()) => {
                delay = RETRY_MIN;
                q.done(&r);
            }
            // The server rejected this report for good (e.g. the item was
            // deleted): retrying won't help.
            Err(Error::Status(s))
                if (400..500).contains(&s) && s != 401 && s != 408 && s != 429 =>
            {
                tracing::warn!(status = s, item = %r.item_id, "playback report rejected; dropped");
                q.done(&r);
            }
            Err(e) => {
                tracing::info!("playback report not sent ({e}); retrying in {}s", delay.as_secs());
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = session.changed() => {}
                }
                delay = (delay * 2).min(RETRY_MAX);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jellyfin_api::playback::PlayMethod;

    fn rep(kind: ReportKind, session: &str, pos: i64) -> Report {
        Report {
            kind,
            item_id: "i".into(),
            media_source_id: "i".into(),
            play_session_id: session.into(),
            position_ticks: pos,
            paused: false,
            audio_stream_index: None,
            subtitle_stream_index: None,
            play_method: PlayMethod::DirectPlay,
            volume: 100,
            muted: false,
            user_id: String::new(),
        }
    }

    #[test]
    fn progress_coalesces_and_queue_persists() {
        let dir = std::env::temp_dir().join(format!("jellyroll-reports-{}", std::process::id()));
        let p = path(&dir);
        let q = ReportQueue::open(Some(p.clone()));
        q.push(rep(ReportKind::Start, "a", 0));
        q.push(rep(ReportKind::Progress, "a", 10));
        q.push(rep(ReportKind::Progress, "a", 20));
        q.push(rep(ReportKind::Stopped, "a", 25));
        let kinds: Vec<_> = q.snapshot().iter().map(|r| (r.kind, r.position_ticks)).collect();
        assert_eq!(
            kinds,
            [(ReportKind::Start, 0), (ReportKind::Progress, 20), (ReportKind::Stopped, 25)]
        );
        // A restart finds them.
        let again = ReportQueue::open(Some(p.clone()));
        assert_eq!(again.snapshot().len(), 3);
        // Sent: the file goes away when the queue empties.
        for r in again.snapshot() {
            again.done(&r);
        }
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn done_keeps_a_newer_replacement() {
        let q = ReportQueue::open(None);
        q.push(rep(ReportKind::Progress, "a", 10));
        let sent = q.front_for("").unwrap();
        q.push(rep(ReportKind::Progress, "a", 20));
        q.done(&sent);
        assert_eq!(q.snapshot()[0].position_ticks, 20, "the newer position is still sent");
    }

    #[test]
    fn each_users_reports_wait_for_that_user() {
        let q = ReportQueue::open(None);
        let mut a = rep(ReportKind::Stopped, "a", 10);
        a.user_id = "alice".into();
        let mut b = rep(ReportKind::Stopped, "b", 20);
        b.user_id = "bob".into();
        let old = rep(ReportKind::Stopped, "o", 30);
        q.push(a.clone());
        q.push(b.clone());
        q.push(old.clone());
        assert_eq!(q.front_for("bob"), Some(b.clone()), "alice's waits for alice");
        q.done(&b);
        assert_eq!(q.front_for("bob"), Some(old.clone()), "untagged: whoever is signed in");
        assert_eq!(q.front_for("alice"), Some(a));
    }
}
