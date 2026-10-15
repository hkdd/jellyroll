//! Which HDMI audio outputs have a screen on them (D82).
//!
//! `/proc/asound/card*/eld#*` says a screen is connected but not to which
//! PCM device (on Intel the pin-to-PCM pairing is dynamic). The ALSA
//! control interface does: every HDMI PCM has an "ELD" control (iface PCM,
//! `device` = the PCM device number) holding the connected screen's ELD,
//! empty when nothing is connected. So the controls are listed and read
//! with the same ioctls `amixer` uses, without linking alsa-lib.

use std::ffi::{c_int, c_ulong, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

/// An HDMI output with a screen connected.
#[derive(Clone, Debug, PartialEq)]
pub struct HdmiSink {
    /// ALSA card id, e.g. "PCH" (as in `hdmi:CARD=PCH,DEV=0`).
    pub card: String,
    /// PCM device number, e.g. 3.
    pub device: u32,
    /// PCM name, e.g. "HDMI 0".
    pub pcm_name: String,
    /// The screen's name from its ELD, e.g. "LG TV SSCR2" (may be empty).
    pub monitor: String,
}

/// Every HDMI output with a screen connected, by card then device. Errors
/// (no sound cards, no access) give an empty list: callers fall back.
pub fn connected() -> Vec<HdmiSink> {
    connected_in(Path::new("/proc/asound"), Path::new("/dev/snd"))
}

fn connected_in(proc_asound: &Path, dev_snd: &Path) -> Vec<HdmiSink> {
    let mut out = Vec::new();
    for card in 0..32u32 {
        let card_dir = proc_asound.join(format!("card{card}"));
        if !card_dir.exists() {
            continue;
        }
        let Ok(id) = std::fs::read_to_string(card_dir.join("id")) else { continue };
        let id = id.trim().to_string();
        let ctl = dev_snd.join(format!("controlC{card}"));
        let elds = match read_elds(&ctl) {
            Ok(e) => e,
            Err(e) => {
                tracing::debug!("{}: {e}", ctl.display());
                continue;
            }
        };
        for (device, eld) in elds {
            let Some(monitor) = parse_eld(&eld) else { continue };
            let pcm_name = std::fs::read_to_string(card_dir.join(format!("pcm{device}p/info")))
                .ok()
                .and_then(|info| pcm_name(&info))
                .unwrap_or_default();
            out.push(HdmiSink { card: id.clone(), device, pcm_name, monitor });
        }
    }
    out
}

/// The `name:` line of `/proc/asound/cardN/pcmXp/info`.
fn pcm_name(info: &str) -> Option<String> {
    info.lines()
        .find_map(|l| l.strip_prefix("name:"))
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
}

/// The screen's name if the ELD is valid (a screen is connected), else
/// None. Layout per the HDA spec (7.3.3.34): header byte 0 bits 7..3 are
/// the ELD version (2); baseline byte 4 bits 4..0 are the monitor name
/// length, the name starts at byte 20.
pub fn parse_eld(eld: &[u8]) -> Option<String> {
    if eld.len() < 20 || eld[0] >> 3 == 0 {
        return None;
    }
    let len = (eld[4] & 0x1f) as usize;
    let name = eld.get(20..20 + len).unwrap_or(&[]);
    Some(String::from_utf8_lossy(name).trim_end_matches('\0').trim().to_string())
}

// ---- ALSA control ioctls (<sound/asound.h>, 64-bit layouts) -----------------

const IFACE_PCM: c_int = 3;
const ID_NAME_LEN: usize = 44;

#[repr(C)]
#[derive(Copy, Clone)]
struct ElemId {
    numid: u32,
    iface: c_int,
    device: u32,
    subdevice: u32,
    name: [u8; ID_NAME_LEN],
    index: u32,
}

#[repr(C)]
struct ElemList {
    offset: u32,
    space: u32,
    used: u32,
    count: u32,
    pids: *mut ElemId,
    reserved: [u8; 50],
}

/// `struct snd_ctl_elem_value`: the id, the `indirect` bitfield word, then
/// a 1024-byte value union (8-byte aligned) and 128 reserved bytes.
#[repr(C)]
struct ElemValue {
    id: ElemId,
    indirect: u32,
    _pad: u32,
    value: [u8; 1024],
    reserved: [u8; 128],
}

const fn iowr(nr: c_ulong, size: usize) -> c_ulong {
    (3 << 30) | ((size as c_ulong) << 16) | ((b'U' as c_ulong) << 8) | nr
}
const ELEM_LIST: c_ulong = iowr(0x10, size_of::<ElemList>());
const ELEM_READ: c_ulong = iowr(0x12, size_of::<ElemValue>());

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

fn ioctl_mut<T>(f: &File, request: c_ulong, arg: &mut T) -> std::io::Result<()> {
    // SAFETY: `request` is one of the ioctls above whose argument is a `T`
    // (sizes encoded in the request and checked in tests); `arg` is valid
    // and exclusively borrowed for the call.
    let rc = unsafe { ioctl(f.as_raw_fd(), request, arg as *mut T as *mut c_void) };
    if rc < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn zeroed<T>() -> T {
    // SAFETY: only used for the plain-data `#[repr(C)]` structs above (and
    // a raw pointer field, for which null is valid).
    unsafe { std::mem::zeroed() }
}

/// (PCM device, ELD bytes) for every "ELD" control on a card.
fn read_elds(ctl: &Path) -> std::io::Result<Vec<(u32, Vec<u8>)>> {
    if cfg!(not(target_pointer_width = "64")) {
        return Ok(Vec::new());
    }
    let f = File::open(ctl)?;
    // How many controls, then all their ids.
    let mut list: ElemList = zeroed();
    ioctl_mut(&f, ELEM_LIST, &mut list)?;
    let mut ids: Vec<ElemId> = vec![zeroed(); list.count as usize];
    list.space = list.count;
    list.pids = ids.as_mut_ptr();
    ioctl_mut(&f, ELEM_LIST, &mut list)?;
    ids.truncate(list.used as usize);

    let mut out = Vec::new();
    for id in ids.iter().filter(|i| i.iface == IFACE_PCM && name_is(&i.name, b"ELD")) {
        let mut v: ElemValue = zeroed();
        v.id = *id;
        if ioctl_mut(&f, ELEM_READ, &mut v).is_ok() {
            out.push((id.device, v.value[..512].to_vec()));
        }
    }
    Ok(out)
}

fn name_is(name: &[u8; ID_NAME_LEN], want: &[u8]) -> bool {
    let end = name.iter().position(|&b| b == 0).unwrap_or(ID_NAME_LEN);
    &name[..end] == want
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid ELD naming `monitor` (version 2, baseline as on an LG TV).
    pub fn eld(monitor: &str) -> Vec<u8> {
        let mut e = vec![0u8; 84];
        e[0] = 2 << 3;
        e[2] = 18;
        e[4] = (1 << 5) | monitor.len() as u8;
        e[20..20 + monitor.len()].copy_from_slice(monitor.as_bytes());
        e
    }

    #[test]
    fn struct_layouts_match_the_kernel() {
        // x86_64 / aarch64 values of SNDRV_CTL_IOCTL_ELEM_LIST / _READ.
        assert_eq!(size_of::<ElemId>(), 64);
        assert_eq!(size_of::<ElemList>(), 80);
        assert_eq!(size_of::<ElemValue>(), 1224);
        assert_eq!(ELEM_LIST, 0xC050_5510);
        assert_eq!(ELEM_READ, 0xC4C8_5512);
    }

    #[test]
    fn eld_gives_the_screen_name_or_nothing() {
        assert_eq!(parse_eld(&eld("LG TV SSCR2")).as_deref(), Some("LG TV SSCR2"));
        assert_eq!(parse_eld(&eld("")).as_deref(), Some(""), "connected, no name");
        assert_eq!(parse_eld(&[0u8; 512]), None, "nothing connected");
        assert_eq!(parse_eld(&[]), None);
    }

    #[test]
    fn pcm_name_from_proc_info() {
        let info = "card: 0\ndevice: 3\nsubdevice: 0\nstream: PLAYBACK\nid: HDMI 0\nname: HDMI 0\n";
        assert_eq!(pcm_name(info).as_deref(), Some("HDMI 0"));
        assert_eq!(pcm_name("card: 0\n"), None);
    }

    #[test]
    fn no_sound_cards_is_empty() {
        let d = std::env::temp_dir().join(format!("jellyroll-asound-{}", std::process::id()));
        assert!(connected_in(&d, &d).is_empty());
    }

    #[test]
    fn a_card_without_a_control_device_is_skipped() {
        let d = std::env::temp_dir().join(format!("jellyroll-asound2-{}", std::process::id()));
        std::fs::create_dir_all(d.join("card0")).unwrap();
        std::fs::write(d.join("card0/id"), "PCH\n").unwrap();
        assert!(connected_in(&d, &d).is_empty());
        let _ = std::fs::remove_dir_all(d);
    }
}
