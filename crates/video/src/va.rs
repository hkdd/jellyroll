//! Runtime hardware-decode capability probe through VA-API (Intel, AMD,
//! and others with a VA-API driver). Asks the driver which codec profiles
//! have a decode (VLD) entry point, the same query `vainfo` makes, so the
//! app knows what this machine can direct-play without hardcoding any
//! hardware (spec 7.2; feeds the Jellyfin device profile in M5).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::os::fd::AsRawFd;

type VADisplay = *mut c_void;
const VA_STATUS_SUCCESS: c_int = 0;
const VA_ENTRYPOINT_VLD: c_int = 1;

#[link(name = "va")]
unsafe extern "C" {
    fn vaInitialize(dpy: VADisplay, major: *mut c_int, minor: *mut c_int) -> c_int;
    fn vaTerminate(dpy: VADisplay) -> c_int;
    fn vaMaxNumProfiles(dpy: VADisplay) -> c_int;
    fn vaMaxNumEntrypoints(dpy: VADisplay) -> c_int;
    fn vaQueryConfigProfiles(dpy: VADisplay, list: *mut c_int, num: *mut c_int) -> c_int;
    fn vaQueryConfigEntrypoints(
        dpy: VADisplay,
        profile: c_int,
        list: *mut c_int,
        num: *mut c_int,
    ) -> c_int;
    fn vaQueryVendorString(dpy: VADisplay) -> *const c_char;
    fn vaSetInfoCallback(
        dpy: VADisplay,
        cb: Option<unsafe extern "C" fn(*mut c_void, *const c_char)>,
        ctx: *mut c_void,
    ) -> *mut c_void;
}

#[link(name = "va-drm")]
unsafe extern "C" {
    fn vaGetDisplayDRM(fd: c_int) -> VADisplay;
}

/// Codecs as the app and the Jellyfin device profile care about them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Codec {
    Mpeg2,
    Vc1,
    H264,
    Vp8,
    Hevc8,
    Hevc10,
    Vp9_8,
    Vp9_10,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 9] = [
        Codec::Mpeg2,
        Codec::Vc1,
        Codec::H264,
        Codec::Vp8,
        Codec::Hevc8,
        Codec::Hevc10,
        Codec::Vp9_8,
        Codec::Vp9_10,
        Codec::Av1,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Codec::Mpeg2 => "MPEG-2",
            Codec::Vc1 => "VC-1",
            Codec::H264 => "H.264",
            Codec::Vp8 => "VP8",
            Codec::Hevc8 => "HEVC 8-bit",
            Codec::Hevc10 => "HEVC 10-bit",
            Codec::Vp9_8 => "VP9 8-bit",
            Codec::Vp9_10 => "VP9 10-bit",
            Codec::Av1 => "AV1",
        }
    }

    /// VA-API profiles (va.h `VAProfile` values) that provide this codec.
    fn va_profiles(self) -> &'static [c_int] {
        match self {
            Codec::Mpeg2 => &[0, 1],
            Codec::Vc1 => &[8, 9, 10],
            Codec::H264 => &[6, 7, 13],
            Codec::Vp8 => &[14],
            Codec::Hevc8 => &[17],
            Codec::Hevc10 => &[18],
            Codec::Vp9_8 => &[19],
            Codec::Vp9_10 => &[21],
            // AV1 Main (profile 0) covers 8- and 10-bit 4:2:0.
            Codec::Av1 => &[32],
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodeCaps {
    /// DRM render node probed, e.g. /dev/dri/renderD128.
    pub device: String,
    pub driver: String,
    /// VA-API profiles with a decode entry point.
    pub va_profiles: Vec<c_int>,
}

impl DecodeCaps {
    pub fn supports(&self, c: Codec) -> bool {
        c.va_profiles().iter().any(|p| self.va_profiles.contains(p))
    }

    pub fn supported(&self) -> Vec<Codec> {
        Codec::ALL.into_iter().filter(|&c| self.supports(c)).collect()
    }

    pub fn unsupported(&self) -> Vec<Codec> {
        Codec::ALL.into_iter().filter(|&c| !self.supports(c)).collect()
    }
}

/// Probes each DRM render node until one has a working VA-API driver.
pub fn probe() -> Result<DecodeCaps, String> {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .map_err(|e| format!("/dev/dri: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("renderD")))
        .collect();
    nodes.sort();
    let mut last_err = "no DRM render nodes in /dev/dri".to_string();
    for node in nodes {
        match probe_node(&node) {
            Ok(caps) => return Ok(caps),
            Err(e) => last_err = format!("{}: {e}", node.display()),
        }
    }
    Err(last_err)
}

fn probe_node(node: &std::path::Path) -> Result<DecodeCaps, String> {
    let file = std::fs::File::open(node).map_err(|e| e.to_string())?;
    // SAFETY: `file` is an open DRM render node and outlives the display
    // (terminated below before it is closed).
    let dpy = unsafe { vaGetDisplayDRM(file.as_raw_fd()) };
    if dpy.is_null() {
        return Err("vaGetDisplayDRM failed".into());
    }
    // SAFETY: dpy is a valid VADisplay; silences libva's stderr info lines.
    unsafe { vaSetInfoCallback(dpy, None, std::ptr::null_mut()) };
    let (mut major, mut minor) = (0, 0);
    // SAFETY: dpy valid; out-pointers valid.
    let st = unsafe { vaInitialize(dpy, &mut major, &mut minor) };
    if st != VA_STATUS_SUCCESS {
        // SAFETY: dpy valid.
        unsafe { vaTerminate(dpy) };
        return Err(format!("vaInitialize failed (status {st}); is a VA-API driver installed?"));
    }
    let result = (|| {
        // SAFETY: dpy initialized; buffers sized by vaMax* as required.
        unsafe {
            let driver = {
                let p = vaQueryVendorString(dpy);
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                }
            };
            let mut profiles = vec![0 as c_int; vaMaxNumProfiles(dpy).max(0) as usize];
            let mut n: c_int = 0;
            if vaQueryConfigProfiles(dpy, profiles.as_mut_ptr(), &mut n) != VA_STATUS_SUCCESS {
                return Err("vaQueryConfigProfiles failed".to_string());
            }
            profiles.truncate(n.max(0) as usize);
            let mut entry = vec![0 as c_int; vaMaxNumEntrypoints(dpy).max(0) as usize];
            let mut decodable = Vec::new();
            for &p in &profiles {
                let mut ne: c_int = 0;
                if vaQueryConfigEntrypoints(dpy, p, entry.as_mut_ptr(), &mut ne)
                    == VA_STATUS_SUCCESS
                    && entry[..ne.max(0) as usize].contains(&VA_ENTRYPOINT_VLD)
                {
                    decodable.push(p);
                }
            }
            decodable.sort_unstable();
            Ok(DecodeCaps { device: node.display().to_string(), driver, va_profiles: decodable })
        }
    })();
    // SAFETY: dpy initialized above; terminated exactly once.
    unsafe { vaTerminate(dpy) };
    drop(file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skylake_profile_list_maps_to_codecs() {
        // Profiles `vainfo` lists with VAEntrypointVLD on the HTPC
        // (Intel HD 530, iHD driver): MPEG-2, H.264, VC-1, JPEG, VP8, HEVC Main.
        let caps = DecodeCaps {
            va_profiles: vec![0, 1, 6, 7, 8, 9, 10, 12, 13, 14, 17],
            ..Default::default()
        };
        assert_eq!(
            caps.supported(),
            vec![Codec::Mpeg2, Codec::Vc1, Codec::H264, Codec::Vp8, Codec::Hevc8]
        );
        assert_eq!(
            caps.unsupported(),
            vec![Codec::Hevc10, Codec::Vp9_8, Codec::Vp9_10, Codec::Av1]
        );
    }

    #[test]
    fn modern_gpu_supports_everything() {
        let caps = DecodeCaps {
            va_profiles: vec![0, 1, 6, 7, 8, 13, 14, 17, 18, 19, 21, 32],
            ..Default::default()
        };
        assert!(caps.unsupported().is_empty(), "{:?}", caps.unsupported());
    }

    #[test]
    fn probe_without_gpu_is_an_error_not_a_panic() {
        // CI and containers have no /dev/dri; must fail cleanly.
        if !std::path::Path::new("/dev/dri").exists() {
            assert!(probe().is_err());
        }
    }
}
