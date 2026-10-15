//! What pages can ask of the app: fetch data, open another page, play.
//! Pages live on a back stack (spec 10.2); each keeps its own focus and
//! scroll, so Back returns exactly where you were.

use jellyfin_api::dto::CollectionType;

use crate::net::{Cmd, Fetch};

#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    Library {
        id: String,
        title: String,
        collection: Option<CollectionType>,
    },
    Detail {
        id: String,
    },
    Search,
    Settings,
    /// A choice made in Settings.
    Setting(Change),
    /// Direct-play an item (D19), from `start_ticks` (resume).
    Play {
        id: String,
        media_source: String,
        start_ticks: i64,
        title: String,
        tracks: crate::net::TrackChoice,
    },
    /// Write a debug report for an item (D83).
    Debug {
        id: String,
        title: String,
    },
}

/// A setting changed on the Settings page; HomeApp applies and saves it.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    UiResolution(crate::config::UiResolution),
    /// Refresh-rate matching at the current output resolution.
    MatchRefresh(bool),
    AudioDevice(String),
    /// Passthrough on the current audio device.
    Passthrough(bool),
    Channels(String),
    Interpolation(bool),
    Deinterlace(crate::config::Deinterlace),
    ToneMapping(String),
    RenderQuality(String),
    Hdr(String),
    /// Artwork disk cache size in MB.
    ImageCacheMb(u64),
    /// Minutes before the screensaver; 0 = off.
    ScreensaverMinutes(u64),
    ClearCache,
}

/// Collects a page's requests while it handles input or data.
pub struct Ctx<'a> {
    next_id: &'a mut u64,
    pub cmds: Vec<Cmd>,
    pub open: Option<Open>,
}

impl<'a> Ctx<'a> {
    pub fn new(next_id: &'a mut u64) -> Self {
        Self { next_id, cmds: Vec::new(), open: None }
    }

    /// Requests data; the answer arrives as `Fetched` with the returned id.
    pub fn fetch(&mut self, req: Fetch) -> u64 {
        *self.next_id += 1;
        let id = *self.next_id;
        self.cmds.push(Cmd::Fetch { id, req });
        id
    }

    pub fn send(&mut self, c: Cmd) {
        self.cmds.push(c);
    }
}
