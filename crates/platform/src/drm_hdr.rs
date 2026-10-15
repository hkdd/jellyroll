//! HDR10 passthrough on the connector (M9, docs/M9-HDR-PLAN.md).
//!
//! What the TV supports comes from its EDID (sysfs). Switching it into HDR
//! sets two connector properties with the DRM ioctls libdrm wraps:
//! `HDR_OUTPUT_METADATA` (a blob holding the HDMI static-metadata InfoFrame)
//! and `Colorspace` (`BT2020_RGB`). Setting needs the DRM master fd, i.e.
//! SDL's; reading works on any open card node.

use std::ffi::{c_int, c_ulong, c_void};
use std::fs::File;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};

/// A connected output.
#[derive(Clone, Debug, PartialEq)]
pub struct Connector {
    /// e.g. "HDMI-A-1".
    pub name: String,
    /// e.g. /dev/dri/card1.
    pub card: PathBuf,
    /// DRM object id of the connector.
    pub id: u32,
    pub edid: Vec<u8>,
}

/// Every connected output, by sysfs name.
pub fn connected() -> Vec<Connector> {
    connected_in(Path::new("/sys/class/drm"), Path::new("/dev/dri"))
}

fn connected_in(sys: &Path, dev: &Path) -> Vec<Connector> {
    let Ok(rd) = std::fs::read_dir(sys) else { return Vec::new() };
    let mut out: Vec<Connector> = rd
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            // "card1-HDMI-A-1": a connector of card1.
            let (card, name) = file.split_once('-')?;
            if !card.starts_with("card") {
                return None;
            }
            let dir = e.path();
            let status = std::fs::read_to_string(dir.join("status")).ok()?;
            if status.trim() != "connected" {
                return None;
            }
            let id = std::fs::read_to_string(dir.join("connector_id")).ok()?.trim().parse().ok()?;
            Some(Connector {
                name: name.to_string(),
                card: dev.join(card),
                id,
                edid: std::fs::read(dir.join("edid")).unwrap_or_default(),
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.card, &a.name).cmp(&(&b.card, &b.name)));
    out
}

/// What the TV says it can display (EDID CTA-861 HDR static metadata block).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HdrCaps {
    /// SMPTE ST 2084 (HDR10's transfer function).
    pub pq: bool,
    pub hlg: bool,
    /// Desired content max luminance, max frame-average, min (cd/m²), if
    /// the TV gives them.
    pub max_lum: Option<f32>,
    pub max_fall: Option<f32>,
    pub min_lum: Option<f32>,
}

/// The HDR static metadata block of an EDID, if it has one.
pub fn parse_edid_hdr(edid: &[u8]) -> Option<HdrCaps> {
    if edid.len() < 256 {
        return None;
    }
    for ext in edid[128..].as_chunks::<128>().0 {
        // CTA-861 extension: tag 2; byte 2 is where the detailed timings
        // start, data blocks run from byte 4 up to there.
        if ext[0] != 0x02 {
            continue;
        }
        let end = (ext[2] as usize).clamp(4, 127);
        let mut i = 4;
        while i < end {
            let (tag, len) = (ext[i] >> 5, (ext[i] & 0x1f) as usize);
            let payload = ext.get(i + 1..(i + 1 + len).min(end)).unwrap_or(&[]);
            // Extended tag (7) "HDR static metadata" (6).
            if tag == 7 && payload.first() == Some(&0x06) && payload.len() >= 3 {
                let eotf = payload[1];
                let lum = |cv: u8| 50.0 * 2f32.powf(cv as f32 / 32.0);
                let max_lum = payload.get(3).filter(|v| **v != 0).map(|v| lum(*v));
                let max_fall = payload.get(4).filter(|v| **v != 0).map(|v| lum(*v));
                let min_lum = match (max_lum, payload.get(5)) {
                    (Some(max), Some(cv)) => Some(max * (*cv as f32 / 255.0).powi(2) / 100.0),
                    _ => None,
                };
                return Some(HdrCaps {
                    pq: eotf & 0b100 != 0,
                    hlg: eotf & 0b1000 != 0,
                    max_lum,
                    max_fall,
                    min_lum,
                });
            }
            i += 1 + len;
        }
    }
    None
}

/// HDR10 static metadata for the HDMI InfoFrame, in natural units.
#[derive(Clone, Debug, PartialEq)]
pub struct HdrMetadata {
    /// Mastering display primaries, CIE 1931 xy: red, green, blue.
    pub primaries: [(f32, f32); 3],
    pub white: (f32, f32),
    /// Mastering display luminance, cd/m².
    pub max_lum: f32,
    pub min_lum: f32,
    /// Content light levels, cd/m² (0: unknown).
    pub max_cll: f32,
    pub max_fall: f32,
}

impl Default for HdrMetadata {
    /// BT.2020 primaries, D65, a 1000 cd/m² mastering display: what most
    /// HDR10 releases are mastered for, used when a file says nothing.
    fn default() -> Self {
        Self {
            primaries: [(0.708, 0.292), (0.170, 0.797), (0.131, 0.046)],
            white: (0.3127, 0.3290),
            max_lum: 1000.0,
            min_lum: 0.005,
            max_cll: 0.0,
            max_fall: 0.0,
        }
    }
}

impl HdrMetadata {
    /// `struct hdr_output_metadata` (static metadata type 1, PQ), as the
    /// kernel reads it from a property blob.
    pub fn to_blob(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        // u32 metadata_type = 0 (HDMI_STATIC_METADATA_TYPE1) at 0..4.
        b[4] = 2; // eotf: SMPTE ST 2084
        b[5] = 0; // metadata_type: static metadata type 1
        let xy = |v: f32| ((v / 0.00002).round().clamp(0.0, 50000.0)) as u16;
        let nits = |v: f32| v.round().clamp(0.0, 65535.0) as u16;
        let mut words = Vec::with_capacity(12);
        for (x, y) in self.primaries {
            words.extend([xy(x), xy(y)]);
        }
        words.extend([xy(self.white.0), xy(self.white.1)]);
        words.push(nits(self.max_lum));
        words.push((self.min_lum / 0.0001).round().clamp(0.0, 65535.0) as u16);
        words.push(nits(self.max_cll));
        words.push(nits(self.max_fall));
        for (i, w) in words.iter().enumerate() {
            b[6 + 2 * i..8 + 2 * i].copy_from_slice(&w.to_ne_bytes());
        }
        b
    }
}

// ---- DRM property ioctls (<drm/drm_mode.h>, 64-bit layouts) ----------------

const OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
const PROP_ENUM: u32 = 1 << 3;
const PROP_BITMASK: u32 = 1 << 5;
const PROP_NAME_LEN: usize = 32;

#[repr(C)]
#[derive(Default)]
struct ObjGetProperties {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
    _pad: u32,
}

#[repr(C)]
struct GetProperty {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; PROP_NAME_LEN],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PropertyEnum {
    value: u64,
    name: [u8; PROP_NAME_LEN],
}

#[repr(C)]
#[derive(Default)]
struct ObjSetProperty {
    value: u64,
    prop_id: u32,
    obj_id: u32,
    obj_type: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct CreateBlob {
    data: u64,
    length: u32,
    blob_id: u32,
}

#[repr(C)]
#[derive(Default)]
struct DestroyBlob {
    blob_id: u32,
}

const fn iowr(nr: c_ulong, size: usize) -> c_ulong {
    (3 << 30) | ((size as c_ulong) << 16) | ((b'd' as c_ulong) << 8) | nr
}
const GETPROPERTY: c_ulong = iowr(0xAA, size_of::<GetProperty>());
const OBJ_GETPROPERTIES: c_ulong = iowr(0xB9, size_of::<ObjGetProperties>());
const OBJ_SETPROPERTY: c_ulong = iowr(0xBA, size_of::<ObjSetProperty>());
const CREATEPROPBLOB: c_ulong = iowr(0xBD, size_of::<CreateBlob>());
const DESTROYPROPBLOB: c_ulong = iowr(0xBE, size_of::<DestroyBlob>());

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

fn ioctl_mut<T>(fd: RawFd, request: c_ulong, arg: &mut T) -> std::io::Result<()> {
    loop {
        // SAFETY: `request` is one of the ioctls above whose argument is a
        // `T` (sizes encoded in the request and checked in tests); `arg` is
        // valid and exclusively borrowed for the call, and any pointers in
        // it point to buffers of the advertised sizes.
        let rc = unsafe { ioctl(fd, request, arg as *mut T as *mut c_void) };
        if rc >= 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(4 | 11)) {
            // Retry EINTR/EAGAIN like drmIoctl; everything else is final.
            return Err(e);
        }
    }
}

fn c_name(n: &[u8; PROP_NAME_LEN]) -> String {
    let end = n.iter().position(|&b| b == 0).unwrap_or(PROP_NAME_LEN);
    String::from_utf8_lossy(&n[..end]).into_owned()
}

/// A connector property: id, name, current value, and for enums the
/// (value, name) choices.
#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub id: u32,
    pub name: String,
    pub value: u64,
    pub enums: Vec<(u64, String)>,
}

impl Prop {
    pub fn enum_value(&self, name: &str) -> Option<u64> {
        self.enums.iter().find(|(_, n)| n == name).map(|(v, _)| *v)
    }
    pub fn enum_name(&self) -> Option<&str> {
        self.enums.iter().find(|(v, _)| *v == self.value).map(|(_, n)| n.as_str())
    }
}

/// All properties of connector `id`. Works on any open card fd.
pub fn connector_props(fd: RawFd, id: u32) -> std::io::Result<Vec<Prop>> {
    let mut q = ObjGetProperties { obj_id: id, obj_type: OBJECT_CONNECTOR, ..Default::default() };
    ioctl_mut(fd, OBJ_GETPROPERTIES, &mut q)?;
    let n = q.count_props as usize;
    let mut ids = vec![0u32; n];
    let mut values = vec![0u64; n];
    q.props_ptr = ids.as_mut_ptr() as u64;
    q.prop_values_ptr = values.as_mut_ptr() as u64;
    ioctl_mut(fd, OBJ_GETPROPERTIES, &mut q)?;
    let n = (q.count_props as usize).min(n);

    let mut out = Vec::with_capacity(n);
    for (&pid, &value) in ids[..n].iter().zip(&values[..n]) {
        let mut p = GetProperty {
            values_ptr: 0,
            enum_blob_ptr: 0,
            prop_id: pid,
            flags: 0,
            name: [0; PROP_NAME_LEN],
            count_values: 0,
            count_enum_blobs: 0,
        };
        ioctl_mut(fd, GETPROPERTY, &mut p)?;
        let mut enums = Vec::new();
        let count = p.count_enum_blobs as usize;
        if p.flags & (PROP_ENUM | PROP_BITMASK) != 0 && count > 0 {
            let mut buf = vec![PropertyEnum { value: 0, name: [0; PROP_NAME_LEN] }; count];
            p.count_values = 0;
            p.enum_blob_ptr = buf.as_mut_ptr() as u64;
            ioctl_mut(fd, GETPROPERTY, &mut p)?;
            let got = (p.count_enum_blobs as usize).min(count);
            enums = buf[..got].iter().map(|e| (e.value, c_name(&e.name))).collect();
        }
        out.push(Prop { id: pid, name: c_name(&p.name), value, enums });
    }
    Ok(out)
}

/// Sets a connector property. Needs the DRM master fd. On an atomic driver
/// this is a commit that may modeset (the TV re-syncs).
pub fn set_connector_prop(fd: RawFd, conn: u32, prop: u32, value: u64) -> std::io::Result<()> {
    let mut s = ObjSetProperty {
        value,
        prop_id: prop,
        obj_id: conn,
        obj_type: OBJECT_CONNECTOR,
        ..Default::default()
    };
    ioctl_mut(fd, OBJ_SETPROPERTY, &mut s)
}

pub fn create_blob(fd: RawFd, data: &[u8]) -> std::io::Result<u32> {
    let mut b = CreateBlob { data: data.as_ptr() as u64, length: data.len() as u32, blob_id: 0 };
    ioctl_mut(fd, CREATEPROPBLOB, &mut b)?;
    Ok(b.blob_id)
}

pub fn destroy_blob(fd: RawFd, id: u32) -> std::io::Result<()> {
    ioctl_mut(fd, DESTROYPROPBLOB, &mut DestroyBlob { blob_id: id })
}

/// Human-readable HDR facts for every connected output, for
/// `--print-diagnostics` and debug reports: the TV's HDR support and the
/// connector properties passthrough needs.
pub fn report() -> Vec<String> {
    let conns = connected();
    if conns.is_empty() {
        return vec!["hdr: no connected outputs found in /sys/class/drm".into()];
    }
    let mut out = Vec::new();
    for c in conns {
        let key = format!("hdr.{}", c.name);
        let tv = match parse_edid_hdr(&c.edid) {
            Some(h) => {
                let nits = |v: Option<f32>| v.map_or("?".to_string(), |v| format!("{v:.0}"));
                format!(
                    "PQ {}, HLG {}, max {} cd/m², frame-average {} cd/m², min {}",
                    yes(h.pq),
                    yes(h.hlg),
                    nits(h.max_lum),
                    nits(h.max_fall),
                    h.min_lum.map_or("?".into(), |v| format!("{v:.4}"))
                )
            }
            None if c.edid.is_empty() => "no EDID".into(),
            None => "no HDR metadata block (SDR only)".into(),
        };
        out.push(format!("{key}.tv: {tv}"));
        out.push(format!("{key}.connector: {} id {}", c.card.display(), c.id));
        let props = File::open(&c.card).and_then(|f| connector_props(f.as_raw_fd(), c.id));
        match props {
            Ok(props) => {
                let find = |n: &str| props.iter().find(|p| p.name == n);
                out.push(format!(
                    "{key}.HDR_OUTPUT_METADATA: {}",
                    match find("HDR_OUTPUT_METADATA") {
                        Some(p) if p.value == 0 => "present (off)".to_string(),
                        Some(p) => format!("present (blob {})", p.value),
                        None => "missing: this output can't send HDR".into(),
                    }
                ));
                out.push(format!(
                    "{key}.Colorspace: {}",
                    match find("Colorspace") {
                        Some(p) => format!(
                            "{} (choices: {})",
                            p.enum_name().unwrap_or("?"),
                            p.enums.iter().map(|(_, n)| n.as_str()).collect::<Vec<_>>().join(", ")
                        ),
                        None => "missing".into(),
                    }
                ));
                if let Some(p) = find("max bpc") {
                    out.push(format!("{key}.max_bpc: {}", p.value));
                }
            }
            Err(e) => out.push(format!("{key}.properties: {}: {e}", c.card.display())),
        }
    }
    out
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An EDID with one CTA extension holding an HDR static metadata block.
    pub fn edid_with_hdr(eotf: u8, lums: &[u8]) -> Vec<u8> {
        let mut e = vec![0u8; 256];
        e[126] = 1;
        let x = &mut e[128..];
        x[0] = 0x02;
        x[1] = 3;
        // A video data block first (tag 2, 2 bytes), then the HDR block.
        x[4] = (2 << 5) | 2;
        x[5] = 16;
        x[6] = 4;
        let len = 3 + lums.len();
        x[7] = (7 << 5) | len as u8;
        x[8] = 0x06;
        x[9] = eotf;
        x[10] = 0x01;
        x[11..11 + lums.len()].copy_from_slice(lums);
        x[2] = (11 + lums.len()) as u8;
        e
    }

    #[test]
    fn struct_layouts_match_the_kernel() {
        assert_eq!(size_of::<ObjGetProperties>(), 32);
        assert_eq!(size_of::<GetProperty>(), 64);
        assert_eq!(size_of::<PropertyEnum>(), 40);
        assert_eq!(size_of::<ObjSetProperty>(), 24);
        assert_eq!(size_of::<CreateBlob>(), 16);
        assert_eq!(size_of::<DestroyBlob>(), 4);
        // libdrm's values on x86_64.
        assert_eq!(GETPROPERTY, 0xC040_64AA);
        assert_eq!(OBJ_GETPROPERTIES, 0xC020_64B9);
        assert_eq!(OBJ_SETPROPERTY, 0xC018_64BA);
        assert_eq!(CREATEPROPBLOB, 0xC010_64BD);
        assert_eq!(DESTROYPROPBLOB, 0xC004_64BE);
    }

    #[test]
    fn edid_hdr_block() {
        // SDR + PQ + HLG; max CV 96 → 50·2³ = 400; min CV 0.
        let h = parse_edid_hdr(&edid_with_hdr(0b1101, &[96, 64, 0])).unwrap();
        assert!(h.pq && h.hlg);
        assert_eq!(h.max_lum, Some(400.0));
        assert_eq!(h.max_fall, Some(200.0));
        assert_eq!(h.min_lum, Some(0.0));
        let h = parse_edid_hdr(&edid_with_hdr(0b0001, &[])).unwrap();
        assert!(!h.pq && !h.hlg, "SDR only");
        assert_eq!(h.max_lum, None);
        assert_eq!(parse_edid_hdr(&[0u8; 128]), None, "no extension");
        let mut no_block = edid_with_hdr(0b100, &[]);
        no_block[128 + 7] = 0;
        no_block[128 + 2] = 7;
        assert_eq!(parse_edid_hdr(&no_block), None);
    }

    #[test]
    fn metadata_blob_layout() {
        let m = HdrMetadata { max_cll: 1000.0, max_fall: 400.0, ..Default::default() };
        let b = m.to_blob();
        assert_eq!(&b[0..4], &[0, 0, 0, 0], "static metadata type 1");
        assert_eq!(b[4], 2, "PQ");
        let w = |i: usize| u16::from_ne_bytes([b[6 + 2 * i], b[7 + 2 * i]]);
        assert_eq!(w(0), 35400, "red x 0.708");
        assert_eq!(w(1), 14600, "red y 0.292");
        assert_eq!(w(6), 15635, "white x 0.3127");
        assert_eq!(w(7), 16450, "white y 0.3290");
        assert_eq!(w(8), 1000, "max mastering cd/m²");
        assert_eq!(w(9), 50, "min 0.005 cd/m² in 0.0001 units");
        assert_eq!(w(10), 1000, "MaxCLL");
        assert_eq!(w(11), 400, "MaxFALL");
        assert_eq!(&b[30..32], &[0, 0], "padding");
    }

    #[test]
    fn connectors_from_sysfs() {
        let d = std::env::temp_dir().join(format!("jellyroll-drm-{}", std::process::id()));
        let mk = |name: &str, status: &str, id: &str| {
            let c = d.join(name);
            std::fs::create_dir_all(&c).unwrap();
            std::fs::write(c.join("status"), status).unwrap();
            std::fs::write(c.join("connector_id"), id).unwrap();
        };
        mk("card1-HDMI-A-1", "connected\n", "236\n");
        mk("card1-HDMI-A-2", "disconnected\n", "246\n");
        mk("card1", "", "");
        let c = connected_in(&d, Path::new("/dev/dri"));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "HDMI-A-1");
        assert_eq!(c[0].card, Path::new("/dev/dri/card1"));
        assert_eq!(c[0].id, 236);
        let _ = std::fs::remove_dir_all(d);
    }
}
