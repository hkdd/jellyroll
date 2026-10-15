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
const OBJECT_CRTC: u32 = 0xcccc_cccc;
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

/// `struct drm_mode_modeinfo`: one display mode as the kernel describes it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrmMode {
    pub clock: u32,
    pub hdisplay: u16,
    pub hsync_start: u16,
    pub hsync_end: u16,
    pub htotal: u16,
    pub hskew: u16,
    pub vdisplay: u16,
    pub vsync_start: u16,
    pub vsync_end: u16,
    pub vtotal: u16,
    pub vscan: u16,
    pub vrefresh: u32,
    pub flags: u32,
    pub type_: u32,
    pub name: [u8; PROP_NAME_LEN],
}

const MODE_FLAG_INTERLACE: u32 = 1 << 4;
const MODE_FLAG_DBLSCAN: u32 = 1 << 5;

impl DrmMode {
    /// Exact refresh rate in Hz (23.976 and 24 differ here, not in
    /// `vrefresh`).
    pub fn hz(&self) -> f64 {
        let total = self.htotal as f64 * self.vtotal as f64;
        if total == 0.0 {
            return 0.0;
        }
        let mut hz = self.clock as f64 * 1000.0 / total;
        if self.flags & MODE_FLAG_INTERLACE != 0 {
            hz *= 2.0;
        }
        if self.flags & MODE_FLAG_DBLSCAN != 0 {
            hz /= 2.0;
        }
        hz
    }

    pub fn interlaced(&self) -> bool {
        self.flags & MODE_FLAG_INTERLACE != 0
    }

    fn zeroed() -> Self {
        Self {
            clock: 0,
            hdisplay: 0,
            hsync_start: 0,
            hsync_end: 0,
            htotal: 0,
            hskew: 0,
            vdisplay: 0,
            vsync_start: 0,
            vsync_end: 0,
            vtotal: 0,
            vscan: 0,
            vrefresh: 0,
            flags: 0,
            type_: 0,
            name: [0; PROP_NAME_LEN],
        }
    }

    fn as_bytes(&self) -> &[u8] {
        // SAFETY: plain-data repr(C) struct without padding (size checked
        // in tests: 68 bytes, all fields packed naturally).
        unsafe { std::slice::from_raw_parts(self as *const Self as *const u8, size_of::<Self>()) }
    }
}

/// The progressive mode of `w`×`h` closest to `hz`, within 0.01 Hz.
pub fn pick_mode(modes: &[DrmMode], w: u32, h: u32, hz: f64) -> Option<&DrmMode> {
    modes
        .iter()
        .filter(|m| m.hdisplay as u32 == w && m.vdisplay as u32 == h && !m.interlaced())
        .map(|m| (m, (m.hz() - hz).abs()))
        .filter(|(_, d)| *d < 0.01)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(m, _)| m)
}

#[repr(C)]
#[derive(Default)]
struct GetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct Atomic {
    flags: u32,
    count_objs: u32,
    objs_ptr: u64,
    count_props_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    reserved: u64,
    user_data: u64,
}

pub const ATOMIC_TEST_ONLY: u32 = 0x0100;
pub const ATOMIC_ALLOW_MODESET: u32 = 0x0400;

const fn iowr(nr: c_ulong, size: usize) -> c_ulong {
    (3 << 30) | ((size as c_ulong) << 16) | ((b'd' as c_ulong) << 8) | nr
}
const GETPROPERTY: c_ulong = iowr(0xAA, size_of::<GetProperty>());
const OBJ_GETPROPERTIES: c_ulong = iowr(0xB9, size_of::<ObjGetProperties>());
const OBJ_SETPROPERTY: c_ulong = iowr(0xBA, size_of::<ObjSetProperty>());
const CREATEPROPBLOB: c_ulong = iowr(0xBD, size_of::<CreateBlob>());
const DESTROYPROPBLOB: c_ulong = iowr(0xBE, size_of::<DestroyBlob>());
const GETCONNECTOR: c_ulong = iowr(0xA7, size_of::<GetConnector>());
const ATOMIC: c_ulong = iowr(0xBC, size_of::<Atomic>());

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
    object_props(fd, id, OBJECT_CONNECTOR)
}

/// The modes connector `id` offers (no re-probe of the TV).
pub fn connector_modes(fd: RawFd, id: u32) -> std::io::Result<Vec<DrmMode>> {
    // A non-zero count_modes keeps the kernel from re-probing the
    // connector (an EDID read); it copies only when the buffer is big
    // enough, and always reports the real count.
    let mut buf = vec![DrmMode::zeroed(); 1];
    loop {
        let mut q = GetConnector {
            connector_id: id,
            count_modes: buf.len() as u32,
            modes_ptr: buf.as_mut_ptr() as u64,
            ..Default::default()
        };
        ioctl_mut(fd, GETCONNECTOR, &mut q)?;
        let n = q.count_modes as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        buf = vec![DrmMode::zeroed(); n];
    }
}

/// One atomic commit of (object, property, value) triples. Needs the DRM
/// master fd with the atomic client cap (SDL sets it when it uses atomic).
pub fn atomic_commit(fd: RawFd, props: &[(u32, u32, u64)], flags: u32) -> std::io::Result<()> {
    // The ioctl wants objects listed once, each with its properties.
    let mut objs: Vec<u32> = Vec::new();
    let mut counts: Vec<u32> = Vec::new();
    let mut ids: Vec<u32> = Vec::new();
    let mut values: Vec<u64> = Vec::new();
    for obj in props.iter().map(|p| p.0) {
        if objs.contains(&obj) {
            continue;
        }
        objs.push(obj);
        let mine: Vec<_> = props.iter().filter(|p| p.0 == obj).collect();
        counts.push(mine.len() as u32);
        ids.extend(mine.iter().map(|p| p.1));
        values.extend(mine.iter().map(|p| p.2));
    }
    let mut a = Atomic {
        flags,
        count_objs: objs.len() as u32,
        objs_ptr: objs.as_ptr() as u64,
        count_props_ptr: counts.as_ptr() as u64,
        props_ptr: ids.as_ptr() as u64,
        prop_values_ptr: values.as_ptr() as u64,
        ..Default::default()
    };
    ioctl_mut(fd, ATOMIC, &mut a)
}

fn object_props(fd: RawFd, id: u32, obj_type: u32) -> std::io::Result<Vec<Prop>> {
    let mut q = ObjGetProperties { obj_id: id, obj_type, ..Default::default() };
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

/// Switches one connector between SDR and HDR10 through the DRM master fd
/// (SDL's). Dropping it puts the connector back to SDR.
pub struct HdrSwitch {
    fd: RawFd,
    pub connector: Connector,
    hdr_prop: u32,
    /// `Colorspace` property id and its `BT2020_RGB` / `Default` values.
    colorspace: Option<(u32, u64, u64)>,
    /// The metadata blob in use while HDR is on.
    blob: Option<u32>,
    /// For one-commit mode + HDR switches: the connector's `CRTC_ID`
    /// property and the CRTC it drives with its `MODE_ID` and `ACTIVE`.
    kms: Option<Kms>,
    /// The mode blob of our last mode switch.
    mode_blob: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct Kms {
    conn_crtc_prop: u32,
    crtc: u32,
    mode_id_prop: u32,
    active_prop: u32,
}

/// EBUSY: a page flip is in flight; retry.
const EBUSY: i32 = 16;
const BUSY_RETRIES: u32 = 25;

fn with_retry(mut f: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let mut n = 0;
    loop {
        match f() {
            Err(e) if e.raw_os_error() == Some(EBUSY) && n < BUSY_RETRIES => {
                n += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            r => return r,
        }
    }
}

impl HdrSwitch {
    /// Finds the connected output on `fd`'s device that has
    /// `HDR_OUTPUT_METADATA`. `fd` must stay open while this lives.
    pub fn new(fd: RawFd) -> Result<Self, String> {
        let mut why = String::from("no connected output");
        for c in connected() {
            let props = match connector_props(fd, c.id) {
                Ok(p) => p,
                Err(e) => {
                    why = format!("{}: {e}", c.name);
                    continue;
                }
            };
            let Some(hdr) = props.iter().find(|p| p.name == "HDR_OUTPUT_METADATA") else {
                why = format!("{} has no HDR_OUTPUT_METADATA property", c.name);
                continue;
            };
            let colorspace = props
                .iter()
                .find(|p| p.name == "Colorspace")
                .and_then(|p| Some((p.id, p.enum_value("BT2020_RGB")?, p.enum_value("Default")?)));
            let kms = (|| {
                let crtc_prop = props.iter().find(|p| p.name == "CRTC_ID")?;
                let crtc = crtc_prop.value as u32;
                let cprops = object_props(fd, crtc, OBJECT_CRTC).ok()?;
                let find = |n: &str| cprops.iter().find(|p| p.name == n).map(|p| p.id);
                Some(Kms {
                    conn_crtc_prop: crtc_prop.id,
                    crtc: (crtc != 0).then_some(crtc)?,
                    mode_id_prop: find("MODE_ID")?,
                    active_prop: find("ACTIVE")?,
                })
            })();
            return Ok(Self {
                fd,
                connector: c,
                hdr_prop: hdr.id,
                colorspace,
                blob: None,
                kms,
                mode_blob: None,
            });
        }
        Err(why)
    }

    pub fn is_on(&self) -> bool {
        self.blob.is_some()
    }

    /// The modes the connector offers.
    pub fn modes(&self) -> Result<Vec<DrmMode>, String> {
        connector_modes(self.fd, self.connector.id).map_err(|e| format!("modes: {e}"))
    }

    /// Sets the output mode and HDR (`Some`) or SDR (`None`) in **one**
    /// atomic commit, so the TV re-syncs once. Tested first (TEST_ONLY);
    /// on any error nothing has changed and the caller can fall back to
    /// separate switches.
    pub fn commit_mode_hdr(
        &mut self,
        mode: &DrmMode,
        meta: Option<&HdrMetadata>,
    ) -> Result<(), String> {
        let k = self.kms.ok_or("no CRTC properties for an atomic mode switch")?;
        let fd = self.fd;
        let mode_blob = create_blob(fd, mode.as_bytes()).map_err(|e| format!("mode blob: {e}"))?;
        let hdr_blob = match meta {
            Some(m) => match create_blob(fd, &m.to_blob()) {
                Ok(b) => Some(b),
                Err(e) => {
                    let _ = destroy_blob(fd, mode_blob);
                    return Err(format!("HDR blob: {e}"));
                }
            },
            None => None,
        };
        let conn = self.connector.id;
        let mut props = vec![
            (k.crtc, k.mode_id_prop, mode_blob as u64),
            (k.crtc, k.active_prop, 1),
            (conn, k.conn_crtc_prop, k.crtc as u64),
            (conn, self.hdr_prop, hdr_blob.unwrap_or(0) as u64),
        ];
        if let Some((cs, bt2020, default)) = self.colorspace {
            props.push((conn, cs, if meta.is_some() { bt2020 } else { default }));
        }
        let result =
            with_retry(|| atomic_commit(fd, &props, ATOMIC_ALLOW_MODESET | ATOMIC_TEST_ONLY))
                .map_err(|e| format!("atomic test: {e}"))
                .and_then(|()| {
                    with_retry(|| atomic_commit(fd, &props, ATOMIC_ALLOW_MODESET))
                        .map_err(|e| format!("atomic commit: {e}"))
                });
        if let Err(e) = result {
            let _ = destroy_blob(fd, mode_blob);
            if let Some(b) = hdr_blob {
                let _ = destroy_blob(fd, b);
            }
            return Err(e);
        }
        for old in [self.mode_blob.replace(mode_blob), std::mem::replace(&mut self.blob, hdr_blob)]
            .into_iter()
            .flatten()
        {
            let _ = destroy_blob(fd, old);
        }
        Ok(())
    }

    /// Sends `meta` to the TV and switches the colour space to BT.2020.
    /// The TV re-syncs. A failed colour-space change is logged, not fatal
    /// (TVs switch to HDR on the InfoFrame alone).
    pub fn enable(&mut self, meta: &HdrMetadata) -> Result<(), String> {
        let blob = create_blob(self.fd, &meta.to_blob()).map_err(|e| format!("blob: {e}"))?;
        let (fd, conn, prop) = (self.fd, self.connector.id, self.hdr_prop);
        if let Err(e) = with_retry(|| set_connector_prop(fd, conn, prop, blob as u64)) {
            let _ = destroy_blob(fd, blob);
            return Err(format!("HDR_OUTPUT_METADATA: {e}"));
        }
        if let Some(old) = self.blob.replace(blob) {
            let _ = destroy_blob(fd, old);
        }
        if let Some((cs, bt2020, _)) = self.colorspace
            && let Err(e) = with_retry(|| set_connector_prop(fd, conn, cs, bt2020))
        {
            tracing::warn!("Colorspace BT2020_RGB: {e} (sending HDR metadata only)");
        }
        Ok(())
    }

    /// Back to SDR: no metadata, default colour space. The TV re-syncs.
    pub fn disable(&mut self) -> Result<(), String> {
        let Some(blob) = self.blob else { return Ok(()) };
        let (fd, conn) = (self.fd, self.connector.id);
        if let Some((cs, _, default)) = self.colorspace
            && let Err(e) = with_retry(|| set_connector_prop(fd, conn, cs, default))
        {
            tracing::warn!("Colorspace Default: {e}");
        }
        let prop = self.hdr_prop;
        with_retry(|| set_connector_prop(fd, conn, prop, 0))
            .map_err(|e| format!("HDR_OUTPUT_METADATA off: {e}"))?;
        self.blob = None;
        let _ = destroy_blob(fd, blob);
        Ok(())
    }
}

impl Drop for HdrSwitch {
    fn drop(&mut self) {
        if let Err(e) = self.disable() {
            tracing::warn!("leaving HDR on exit: {e}");
        }
    }
}

/// The HDR support of the TV on the first connected output that reports it.
pub fn tv_caps() -> Option<HdrCaps> {
    connected().iter().find_map(|c| parse_edid_hdr(&c.edid))
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
    fn mode_rates_and_picking() {
        let mk = |w: u16, h: u16, clock: u32, htotal: u16, vtotal: u16| DrmMode {
            clock,
            hdisplay: w,
            htotal,
            vdisplay: h,
            vtotal,
            ..DrmMode::zeroed()
        };
        // CTA-861 4K timings: 24, 23.976 (clock /1.001) and 60.
        let m24 = mk(3840, 2160, 297_000, 5500, 2250);
        let m23 = mk(3840, 2160, 296_703, 5500, 2250);
        let m60 = mk(3840, 2160, 594_000, 4400, 2250);
        assert!((m24.hz() - 24.0).abs() < 1e-6);
        assert!((m23.hz() - 23.976).abs() < 0.001);
        let modes = [m60, m24, m23];
        assert_eq!(pick_mode(&modes, 3840, 2160, 23.976), Some(&m23));
        assert_eq!(pick_mode(&modes, 3840, 2160, 24.0), Some(&m24));
        assert_eq!(pick_mode(&modes, 3840, 2160, 50.0), None);
        assert_eq!(pick_mode(&modes, 1920, 1080, 60.0), None);
        assert_eq!(m24.as_bytes().len(), 68);
    }

    #[test]
    fn struct_layouts_match_the_kernel() {
        assert_eq!(size_of::<DrmMode>(), 68);
        assert_eq!(size_of::<GetConnector>(), 80);
        assert_eq!(size_of::<Atomic>(), 56);
        assert_eq!(GETCONNECTOR, 0xC050_64A7);
        assert_eq!(ATOMIC, 0xC038_64BC);
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
