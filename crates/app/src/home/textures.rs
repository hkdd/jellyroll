//! GPU textures for server artwork (spec 9, tier 2): decoded images arrive
//! from the network thread, upload within a per-frame time budget so they
//! never cause a hitch, and are evicted least-recently-drawn first when
//! the byte cap is exceeded.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use jellyroll_render::{ImageId, Renderer};

struct Tex {
    id: ImageId,
    bytes: u64,
    /// Frame number it was last drawn.
    used: u64,
}

pub struct Textures {
    map: HashMap<u64, Tex>,
    pending: VecDeque<(u64, u32, u32, Vec<u8>)>,
    failed: HashSet<u64>,
    bytes: u64,
    cap: u64,
    frame: u64,
}

impl Textures {
    pub fn new(cap_bytes: u64) -> Self {
        Self {
            map: HashMap::new(),
            pending: VecDeque::new(),
            failed: HashSet::new(),
            bytes: 0,
            cap: cap_bytes,
            frame: 0,
        }
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }

    /// The texture for `key`, marked as used this frame.
    pub fn get(&mut self, key: u64) -> Option<ImageId> {
        let t = self.map.get_mut(&key)?;
        t.used = self.frame;
        Some(t.id)
    }

    /// Uploaded, waiting to upload, or failed: don't request it again.
    pub fn known(&self, key: u64) -> bool {
        self.map.contains_key(&key)
            || self.failed.contains(&key)
            || self.pending.iter().any(|p| p.0 == key)
    }

    pub fn loaded(&self, key: u64) -> bool {
        self.map.contains_key(&key)
    }

    pub fn queue(&mut self, key: u64, w: u32, h: u32, rgba: Vec<u8>) {
        self.failed.remove(&key);
        self.pending.push_back((key, w, h, rgba));
    }

    pub fn fail(&mut self, key: u64) {
        self.failed.insert(key);
    }

    /// Lets failed images be requested again (e.g. back online).
    pub fn forget_failures(&mut self) {
        self.failed.clear();
    }

    pub fn uploads_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Uploads queued images until `budget` is spent (at least one per
    /// call), then evicts down to the cap. Returns true if anything new is
    /// ready to draw.
    pub fn pump<R: Renderer>(&mut self, r: &mut R, budget: Duration) -> bool {
        let start = Instant::now();
        let mut any = false;
        while let Some((key, w, h, rgba)) = self.pending.pop_front() {
            if let Some(old) = self.map.remove(&key) {
                r.release_image(old.id);
                self.bytes -= old.bytes;
            }
            match r.create_image(w, h, &rgba) {
                Some(id) => {
                    let bytes = w as u64 * h as u64 * 4;
                    self.bytes += bytes;
                    self.map.insert(key, Tex { id, bytes, used: self.frame });
                    any = true;
                }
                None => {
                    self.failed.insert(key);
                }
            }
            if start.elapsed() >= budget {
                break;
            }
        }
        if self.bytes > self.cap {
            self.evict(r);
        }
        any
    }

    fn evict<R: Renderer>(&mut self, r: &mut R) {
        let mut by_age: Vec<(u64, u64)> = self
            .map
            .iter()
            .filter(|(_, t)| t.used < self.frame)
            .map(|(k, t)| (t.used, *k))
            .collect();
        by_age.sort_unstable();
        let target = self.cap / 10 * 9;
        for (_, key) in by_age {
            if self.bytes <= target {
                break;
            }
            if let Some(t) = self.map.remove(&key) {
                r.release_image(t.id);
                self.bytes -= t.bytes;
            }
        }
        tracing::debug!(mb = self.bytes >> 20, "evicted textures over the cap");
    }
}
