//! On-disk state (spec 9, 11, 12):
//! - `~/.local/share/jellyroll/state.json`: device id, servers, users and
//!   their access tokens. Mode 0600, atomic writes, never a password.
//! - `~/.cache/jellyroll/metadata.redb`: last home screen per server/user,
//!   shown instantly at startup while a fresh copy loads (D30).
//! - `~/.cache/jellyroll/images/`: server images as downloaded (already
//!   sized by the server), content-addressed, size-capped LRU by mtime.
//!
//! All of this runs on the network thread's blocking pool, never on the
//! render thread.

use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

fn xdg_dir(var: &str, fallback: &str) -> Option<PathBuf> {
    let base = std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)))?;
    Some(base.join("jellyroll"))
}

/// `$XDG_DATA_HOME/jellyroll` or `~/.local/share/jellyroll`.
pub fn data_dir() -> Option<PathBuf> {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

/// `$XDG_CACHE_HOME/jellyroll` or `~/.cache/jellyroll`.
pub fn cache_dir() -> Option<PathBuf> {
    xdg_dir("XDG_CACHE_HOME", ".cache")
}

/// Write to a temp file, fsync, rename over the target, fsync the
/// directory (spec 11). A crash leaves either the old or the new file.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// FNV-1a 64: stable across Rust versions (unlike `DefaultHasher`), so
/// cache file names survive upgrades.
pub fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Separator so ("ab","c") and ("a","bc") differ.
        h ^= 0xff;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ---- auth state ------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub device_id: String,
    pub servers: Vec<SavedServer>,
    /// Server id and user id used last; picked at startup.
    pub last_server: Option<String>,
    pub last_user: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedServer {
    pub id: String,
    pub name: String,
    pub url: String,
    pub users: Vec<SavedUser>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedUser {
    pub id: String,
    pub name: String,
    pub token: String,
}

impl State {
    /// Loads the state; a missing or unreadable file yields a fresh state
    /// with a new device id (logged, never fatal).
    pub fn load(path: Option<&Path>) -> State {
        let mut s = path
            .and_then(|p| match std::fs::read(p) {
                Ok(b) => serde_json::from_slice::<State>(&b)
                    .map_err(|e| {
                        tracing::warn!("{}: unreadable ({e}); starting fresh", p.display())
                    })
                    .ok(),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => {
                    tracing::warn!("{}: {e}", p.display());
                    None
                }
            })
            .unwrap_or_default();
        if s.device_id.is_empty() {
            s.device_id = new_device_id();
        }
        s
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        atomic_write(path, &json, 0o600)
    }

    /// Every signed-in user on every saved server (the account picker).
    pub fn accounts(&self) -> Vec<super::Account> {
        self.servers
            .iter()
            .flat_map(|s| {
                s.users.iter().filter(|u| !u.token.is_empty()).map(|u| super::Account {
                    server_id: s.id.clone(),
                    server_name: s.name.clone(),
                    user_id: u.id.clone(),
                    user_name: u.name.clone(),
                })
            })
            .collect()
    }

    pub fn server(&self, id: &str) -> Option<&SavedServer> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// The server and user to resume with, if any.
    pub fn last(&self) -> Option<(&SavedServer, Option<&SavedUser>)> {
        let server = self.server(self.last_server.as_deref()?)?;
        let user = self
            .last_user
            .as_deref()
            .and_then(|u| server.users.iter().find(|x| x.id == u && !x.token.is_empty()));
        Some((server, user))
    }

    pub fn remember_server(&mut self, id: &str, name: &str, url: &str) {
        match self.servers.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                s.name = name.into();
                s.url = url.into();
            }
            None => self.servers.push(SavedServer {
                id: id.into(),
                name: name.into(),
                url: url.into(),
                users: Vec::new(),
            }),
        }
        self.last_server = Some(id.into());
    }

    pub fn remember_user(&mut self, server_id: &str, id: &str, name: &str, token: &str) {
        let Some(s) = self.servers.iter_mut().find(|s| s.id == server_id) else { return };
        match s.users.iter_mut().find(|u| u.id == id) {
            Some(u) => {
                u.name = name.into();
                u.token = token.into();
            }
            None => {
                s.users.push(SavedUser { id: id.into(), name: name.into(), token: token.into() })
            }
        }
        self.last_user = Some(id.into());
    }

    /// The server rejected the token (401): forget it, keep the user.
    pub fn forget_token(&mut self, server_id: &str, user_id: &str) {
        if let Some(u) = self
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .and_then(|s| s.users.iter_mut().find(|u| u.id == user_id))
        {
            u.token.clear();
        }
    }
}

/// 128 random bits as hex, from the kernel's CSPRNG.
fn new_device_id() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = io::Read::read_exact(&mut f, &mut b);
    }
    if b == [0; 16] {
        // No /dev/urandom (never on Linux in practice): time-based fallback.
        let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
        b[..16].copy_from_slice(&t.as_nanos().to_le_bytes());
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// This machine's name for the server's device list.
pub fn device_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HTPC".into())
}

// ---- metadata cache --------------------------------------------------------

const KV: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new("kv");

pub struct MetaCache {
    db: redb::Database,
}

impl MetaCache {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        match redb::Database::create(path) {
            Ok(db) => Ok(Self { db }),
            // A corrupt or outdated cache is only a cache: start over. Any
            // other error (another jellyroll has it open, I/O, permissions)
            // leaves the file alone.
            Err(
                e @ (redb::DatabaseError::Storage(redb::StorageError::Corrupted(_))
                | redb::DatabaseError::UpgradeRequired(_)),
            ) => {
                tracing::warn!("{}: {e}; recreating", path.display());
                let _ = std::fs::remove_file(path);
                redb::Database::create(path)
                    .map(|db| Self { db })
                    .map_err(|e| format!("{}: {e}", path.display()))
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        use redb::ReadableDatabase;
        let r = self.db.begin_read().ok()?;
        let t = r.open_table(KV).ok()?;
        let v = t.get(key).ok()??;
        Some(v.value().to_vec())
    }

    /// Forgets everything (Settings: Clear cache).
    pub fn clear(&self) -> Result<(), String> {
        let w = self.db.begin_write().map_err(|e| e.to_string())?;
        w.delete_table(KV).map_err(|e| e.to_string())?;
        w.commit().map_err(|e| e.to_string())
    }

    pub fn put(&self, key: &str, value: &[u8]) -> Result<(), String> {
        let w = self.db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut t = w.open_table(KV).map_err(|e| e.to_string())?;
            t.insert(key, value).map_err(|e| e.to_string())?;
        }
        w.commit().map_err(|e| e.to_string())
    }
}

// ---- image disk cache ------------------------------------------------------

pub struct ImageCache {
    dir: PathBuf,
    cap: u64,
    used: u64,
}

impl ImageCache {
    /// Scans the directory for its current size and trims it to the cap.
    pub fn open(dir: PathBuf, cap: u64) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let mut c = Self { dir, cap, used: 0 };
        c.used = c.entries().iter().map(|e| e.1).sum();
        c.trim();
        c
    }

    fn path(&self, key: u64) -> PathBuf {
        self.dir.join(format!("{key:016x}"))
    }

    /// Cached bytes, marking the entry recently used.
    pub fn get(&self, key: u64) -> Option<Vec<u8>> {
        let p = self.path(key);
        let bytes = std::fs::read(&p).ok()?;
        if let Ok(f) = std::fs::File::options().write(true).open(&p) {
            let _ = f.set_modified(SystemTime::now());
        }
        Some(bytes)
    }

    pub fn put(&mut self, key: u64, bytes: &[u8]) {
        let p = self.path(key);
        // Not fsynced: a lost cache entry is just downloaded again.
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &p).is_ok() {
            self.used += bytes.len() as u64;
            if self.used > self.cap {
                self.trim();
            }
        }
    }

    fn entries(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return Vec::new() };
        rd.filter_map(|e| e.ok())
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                m.is_file()
                    .then(|| (e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)))
            })
            .collect()
    }

    /// Bytes on disk and the cap.
    pub fn usage(&self) -> (u64, u64) {
        (self.used, self.cap)
    }

    /// A new cap (Settings); trims at once if over it.
    pub fn set_cap(&mut self, cap: u64) {
        self.cap = cap;
        self.trim();
    }

    /// Deletes every cached image (Settings: Clear cache).
    pub fn clear(&mut self) {
        for (path, len, _) in self.entries() {
            if std::fs::remove_file(&path).is_ok() {
                self.used = self.used.saturating_sub(len);
            }
        }
        self.used = self.entries().iter().map(|e| e.1).sum();
    }

    /// Deletes least recently used files until under 90% of the cap.
    fn trim(&mut self) {
        if self.used <= self.cap {
            return;
        }
        let mut entries = self.entries();
        entries.sort_by_key(|e| e.2);
        self.used = entries.iter().map(|e| e.1).sum();
        let target = self.cap / 10 * 9;
        for (path, len, _) in entries {
            if self.used <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                self.used -= len;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jellyroll-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fnv_is_stable_and_separates_parts() {
        assert_eq!(fnv1a(&[b"abc"]), fnv1a(&[b"abc"]));
        assert_ne!(fnv1a(&[b"ab", b"c"]), fnv1a(&[b"a", b"bc"]));
        // Pinned value: changing the hash would orphan every cached file.
        assert_eq!(fnv1a(&[b"jellyroll"]), 2934608409550290529);
    }

    #[test]
    fn state_round_trip_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("state");
        let p = d.join("state.json");
        let mut s = State::load(Some(&p));
        assert_eq!(s.device_id.len(), 32);
        s.remember_server("srv", "Home", "http://h:8096");
        s.remember_user("srv", "u1", "alice", "tok");
        s.save(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let back = State::load(Some(&p));
        assert_eq!(back, s);
        let (srv, user) = back.last().unwrap();
        assert_eq!(srv.url, "http://h:8096");
        assert_eq!(user.unwrap().token, "tok");
        let mut s2 = back.clone();
        s2.forget_token("srv", "u1");
        assert!(s2.last().unwrap().1.is_none(), "no token, no auto sign-in");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn meta_cache_round_trip() {
        let d = tmpdir("meta");
        let c = MetaCache::open(&d.join("m.redb")).unwrap();
        assert_eq!(c.get("k"), None);
        c.put("k", b"v1").unwrap();
        c.put("k", b"v2").unwrap();
        assert_eq!(c.get("k").as_deref(), Some(&b"v2"[..]));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn meta_cache_never_deletes_an_open_database() {
        let d = tmpdir("meta-locked");
        let p = d.join("m.redb");
        let a = MetaCache::open(&p).unwrap();
        a.put("k", b"keep").unwrap();
        assert!(MetaCache::open(&p).is_err(), "second opener gets an error");
        assert_eq!(a.get("k").as_deref(), Some(&b"keep"[..]), "data survives");
        drop(a);
        assert_eq!(MetaCache::open(&p).unwrap().get("k").as_deref(), Some(&b"keep"[..]));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn image_cache_evicts_least_recently_used() {
        let d = tmpdir("img");
        let mut c = ImageCache::open(d.clone(), 250);
        c.put(1, &[0; 100]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        c.put(2, &[0; 100]);
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(c.get(1).is_some(), "touching 1 makes 2 the oldest");
        std::thread::sleep(std::time::Duration::from_millis(20));
        c.put(3, &[0; 100]);
        assert!(c.get(2).is_none(), "LRU entry evicted");
        assert!(c.get(1).is_some() && c.get(3).is_some());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn image_cache_resizes_and_clears() {
        let d = tmpdir("img-clear");
        let mut c = ImageCache::open(d.clone(), 10_000);
        for k in 0..5 {
            c.put(k, &[0u8; 1000]);
        }
        assert_eq!(c.usage(), (5000, 10_000));
        c.set_cap(3000);
        assert!(c.usage().0 <= 2700, "trimmed under 90% of the new cap: {:?}", c.usage());
        c.clear();
        assert_eq!(c.usage().0, 0);
        assert!(c.get(4).is_none());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn meta_cache_clears() {
        let d = tmpdir("meta-clear");
        let m = MetaCache::open(&d.join("m.redb")).unwrap();
        m.put("home/a", b"x").unwrap();
        m.clear().unwrap();
        assert_eq!(m.get("home/a"), None);
        m.put("home/a", b"y").unwrap();
        assert_eq!(m.get("home/a").as_deref(), Some(&b"y"[..]), "usable after clearing");
        let _ = std::fs::remove_dir_all(d);
    }
}
