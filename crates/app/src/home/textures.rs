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
    /// Measured upload time (moving average, ns) for small images
    /// (placeholders) and large ones (artwork), to stop before an upload
    /// that would overrun the frame's budget. Small uploads are mostly
    /// fixed cost, so one per-byte rate would mispredict both.
    upload_ns: [f64; 2],
}

/// Images below this many bytes count as small (BlurHash placeholders).
const SMALL: u64 = 64 << 10;

fn bucket(bytes: u64) -> usize {
    usize::from(bytes >= SMALL)
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
            upload_ns: [0.0; 2],
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

    /// Measured upload time (small, large images), ns, for benchmarks.
    pub fn upload_ns(&self) -> [f64; 2] {
        self.upload_ns
    }

    pub fn uploads_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Uploads queued images while the next one is expected to fit in
    /// `budget` (always at least one per call), then evicts down to the
    /// cap. Returns true if anything new is ready to draw.
    pub fn pump<R: Renderer>(&mut self, r: &mut R, budget: Duration) -> bool {
        let start = Instant::now();
        let mut any = false;
        let budget_ns = budget.as_nanos() as f64;
        let mut first = true;
        while let Some(&(_, w, h, _)) = self.pending.front() {
            let b = bucket(w as u64 * h as u64 * 4);
            let elapsed = start.elapsed().as_nanos() as f64;
            if !first && elapsed + self.upload_ns[b] > budget_ns {
                break;
            }
            first = false;
            let Some((key, w, h, rgba)) = self.pending.pop_front() else { break };
            let t = Instant::now();
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
            let cost = t.elapsed().as_nanos() as f64;
            let avg = &mut self.upload_ns[b];
            *avg = if *avg == 0.0 { cost } else { *avg * 0.8 + cost * 0.2 };
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
