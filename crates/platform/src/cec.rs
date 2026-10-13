//! HDMI-CEC through the Linux kernel CEC API (`/dev/cecN`), no libcec.
//!
//! Works with any adapter that has a kernel driver: GPUs with built-in CEC,
//! and USB adapters such as Pulse-Eight (`pulse8-cec`, attached with
//! `inputattach --pulse8-cec`). The app claims a Playback logical address
//! named "Jellyroll", optionally wakes the TV and becomes active source
//! (spec 5.4), and turns the TV remote's key presses into
//! [`PlatformEvent::CecDown`]/[`PlatformEvent::CecUp`].
//!
//! A reader thread does all device I/O and only forwards events through a
//! channel plus the platform waker (spec 4: the render thread never blocks).

use std::ffi::{CStr, c_char, c_int, c_short, c_ulong, c_void};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use crate::{CecKey, PlatformEvent};

// ---- kernel ABI (linux/cec.h; sizes asserted in tests) ------------------

const CEC_ADAP_G_CAPS: c_ulong = 0xc04c_6100;
const CEC_ADAP_G_PHYS_ADDR: c_ulong = 0x8002_6101;
const CEC_ADAP_G_LOG_ADDRS: c_ulong = 0x805c_6103;
const CEC_ADAP_S_LOG_ADDRS: c_ulong = 0xc05c_6104;
const CEC_TRANSMIT: c_ulong = 0xc038_6105;
const CEC_RECEIVE: c_ulong = 0xc038_6106;
const CEC_DQEVENT: c_ulong = 0xc050_6107;
const CEC_S_MODE: c_ulong = 0x4004_6109;

const CEC_MODE_INITIATOR: u32 = 0x1;
const CEC_MODE_FOLLOWER: u32 = 0x10;
const CEC_CAP_LOG_ADDRS: u32 = 0x2;
const CEC_LOG_ADDR_TYPE_PLAYBACK: u8 = 3;
const CEC_OP_PRIM_DEVTYPE_PLAYBACK: u8 = 4;
const CEC_OP_ALL_DEVTYPE_PLAYBACK: u8 = 0x10;
const CEC_OP_CEC_VERSION_1_4: u8 = 5;
const CEC_VENDOR_ID_NONE: u32 = 0xffff_ffff;
const CEC_EVENT_STATE_CHANGE: u32 = 1;
const CEC_PHYS_ADDR_INVALID: u16 = 0xffff;
const CEC_LOG_ADDR_INVALID: u8 = 0xff;
const LOG_ADDR_TV: u8 = 0;
const LOG_ADDR_BROADCAST: u8 = 0xf;

const MSG_FEATURE_ABORT: u8 = 0x00;
const MSG_IMAGE_VIEW_ON: u8 = 0x04;
const MSG_STANDBY: u8 = 0x36;
const MSG_USER_CONTROL_PRESSED: u8 = 0x44;
const MSG_USER_CONTROL_RELEASED: u8 = 0x45;
const MSG_ACTIVE_SOURCE: u8 = 0x82;
const MSG_REQUEST_ACTIVE_SOURCE: u8 = 0x85;
const MSG_SET_STREAM_PATH: u8 = 0x86;
const MSG_MENU_REQUEST: u8 = 0x8d;
const MSG_MENU_STATUS: u8 = 0x8e;
const MSG_GIVE_DEVICE_POWER_STATUS: u8 = 0x8f;
const MSG_REPORT_POWER_STATUS: u8 = 0x90;
const POWER_STATUS_ON: u8 = 0;
const MENU_STATE_ACTIVATED: u8 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct CecCaps {
    driver: [c_char; 32],
    name: [c_char; 32],
    available_log_addrs: u32,
    capabilities: u32,
    version: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CecLogAddrs {
    log_addr: [u8; 4],
    log_addr_mask: u16,
    cec_version: u8,
    num_log_addrs: u8,
    vendor_id: u32,
    flags: u32,
    osd_name: [c_char; 15],
    primary_device_type: [u8; 4],
    log_addr_type: [u8; 4],
    all_device_types: [u8; 4],
    features: [[u8; 12]; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CecMsg {
    tx_ts: u64,
    rx_ts: u64,
    len: u32,
    timeout: u32,
    sequence: u32,
    flags: u32,
    msg: [u8; 16],
    reply: u8,
    rx_status: u8,
    tx_status: u8,
    tx_arb_lost_cnt: u8,
    tx_nack_cnt: u8,
    tx_low_drive_cnt: u8,
    tx_error_cnt: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CecEvent {
    ts: u64,
    event: u32,
    flags: u32,
    data: [u8; 64],
}

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}
const POLLIN: c_short = 0x1;
const POLLPRI: c_short = 0x2;

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
}

/// Zero-initialized ABI struct.
fn zeroed<T: Copy>() -> T {
    // SAFETY: only used for the plain-data `#[repr(C)]` structs above, for
    // which all-zero bytes are a valid value.
    unsafe { std::mem::zeroed() }
}

fn ioctl_ptr<T>(f: &File, request: c_ulong, arg: &mut T) -> std::io::Result<()> {
    // SAFETY: `request` is the ioctl whose argument type is `T` (the
    // _IOR/_IOW/_IOWR size encodes it, checked in tests); `arg` is a valid,
    // exclusively borrowed `T` for the duration of the call.
    let rc = unsafe { ioctl(f.as_raw_fd(), request, arg as *mut T as *mut c_void) };
    if rc < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

fn c_str(bytes: &[c_char]) -> String {
    // SAFETY: the kernel NUL-terminates these fixed-size fields; reading
    // stops at the NUL and the buffer is in bounds.
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(bytes.as_ptr() as *const u8, bytes.len()) };
    CStr::from_bytes_until_nul(bytes).map(|c| c.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---- public API -----------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CecInfo {
    pub device: String,
    pub driver: String,
    pub name: String,
    pub physical_address: Option<u16>,
    pub logical_address: Option<u8>,
}

/// Formats a CEC physical address as `a.b.c.d`.
pub fn fmt_physical_address(pa: u16) -> String {
    format!("{}.{}.{}.{}", pa >> 12, (pa >> 8) & 0xf, (pa >> 4) & 0xf, pa & 0xf)
}

#[derive(Clone, Debug)]
pub struct CecOptions {
    /// Specific device (e.g. /dev/cec0); otherwise the first that opens.
    pub device: Option<PathBuf>,
    /// Name shown on the TV's source list (max 14 bytes).
    pub osd_name: String,
    /// Send "Image View On" at startup (turns the TV on and switches input).
    pub wake_tv: bool,
}

impl Default for CecOptions {
    fn default() -> Self {
        Self { device: None, osd_name: "Jellyroll".into(), wake_tv: true }
    }
}

fn open_device(device: Option<&Path>) -> Result<(PathBuf, File), String> {
    let candidates: Vec<PathBuf> = match device {
        Some(d) => vec![d.to_path_buf()],
        None => {
            let mut v: Vec<PathBuf> = std::fs::read_dir("/dev")
                .map_err(|e| format!("/dev: {e}"))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("cec")))
                .collect();
            v.sort();
            v
        }
    };
    if candidates.is_empty() {
        return Err(
            "no CEC device (/dev/cec*); the GPU has no CEC support or no adapter is attached"
                .into(),
        );
    }
    let mut last = String::new();
    for path in candidates {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                last = format!(
                    "{}: permission denied (add the user to the group that owns it, usually `video`)",
                    path.display()
                );
            }
            Err(e) => last = format!("{}: {e}", path.display()),
        }
    }
    Err(last)
}

fn read_info(path: &Path, f: &File) -> Result<(CecInfo, u32), String> {
    let mut caps: CecCaps = zeroed();
    ioctl_ptr(f, CEC_ADAP_G_CAPS, &mut caps).map_err(|e| format!("CEC_ADAP_G_CAPS: {e}"))?;
    let mut pa: u16 = CEC_PHYS_ADDR_INVALID;
    let _ = ioctl_ptr(f, CEC_ADAP_G_PHYS_ADDR, &mut pa);
    let mut la: CecLogAddrs = zeroed();
    let _ = ioctl_ptr(f, CEC_ADAP_G_LOG_ADDRS, &mut la);
    let logical =
        (la.num_log_addrs > 0 && la.log_addr[0] != CEC_LOG_ADDR_INVALID).then_some(la.log_addr[0]);
    Ok((
        CecInfo {
            device: path.display().to_string(),
            driver: c_str(&caps.driver),
            name: c_str(&caps.name),
            physical_address: (pa != CEC_PHYS_ADDR_INVALID).then_some(pa),
            logical_address: logical,
        },
        caps.capabilities,
    ))
}

/// Adapter information for `--print-diagnostics`.
pub fn probe(device: Option<&Path>) -> Result<CecInfo, String> {
    let (path, f) = open_device(device)?;
    read_info(&path, &f).map(|(i, _)| i)
}

/// Running CEC session; dropping it stops the reader thread.
pub struct Cec {
    stop: Arc<AtomicBool>,
    pub info: CecInfo,
}

impl Drop for Cec {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens the adapter and starts the reader thread. Fails fast (no device,
/// no permission); claiming the logical address happens on the thread
/// because it can wait for the TV.
pub fn start(
    opts: CecOptions,
    tx: Sender<PlatformEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<Cec, String> {
    let (path, file) = open_device(opts.device.as_deref())?;
    let (info, caps) = read_info(&path, &file)?;
    let mut mode = CEC_MODE_INITIATOR | CEC_MODE_FOLLOWER;
    ioctl_ptr(&file, CEC_S_MODE, &mut mode).map_err(|e| format!("CEC_S_MODE: {e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    std::thread::Builder::new()
        .name("cec".into())
        .spawn(move || run(file, caps, opts, tx, wake, thread_stop))
        .map_err(|e| format!("cec thread: {e}"))?;
    Ok(Cec { stop, info })
}

// ---- protocol handling (pure, unit-tested) --------------------------------

/// A held key with no "pressed" repeat or "released" for this long is
/// released (CEC initiators repeat presses at least every 450 ms).
const HOLD_TIMEOUT: Duration = Duration::from_millis(550);

#[derive(Default)]
pub struct Handler {
    pub log_addr: u8,
    pub phys_addr: u16,
    pub active: bool,
    held: Option<(CecKey, Instant)>,
}

#[derive(Debug, PartialEq)]
pub enum Out {
    Event(PlatformEvent),
    Transmit(Vec<u8>),
}

impl Handler {
    fn header(&self, dest: u8) -> u8 {
        (self.log_addr << 4) | (dest & 0xf)
    }

    pub fn active_source(&self) -> Vec<u8> {
        vec![
            self.header(LOG_ADDR_BROADCAST),
            MSG_ACTIVE_SOURCE,
            (self.phys_addr >> 8) as u8,
            self.phys_addr as u8,
        ]
    }

    pub fn image_view_on(&self) -> Vec<u8> {
        vec![self.header(LOG_ADDR_TV), MSG_IMAGE_VIEW_ON]
    }

    fn release(&mut self, out: &mut Vec<Out>) {
        if let Some((key, _)) = self.held.take() {
            out.push(Out::Event(PlatformEvent::CecUp { key }));
        }
    }

    /// Handles one received message (`msg[0]` is the header).
    pub fn on_message(&mut self, msg: &[u8], now: Instant, out: &mut Vec<Out>) {
        if msg.len() < 2 {
            return; // poll message
        }
        let initiator = msg[0] >> 4;
        let opcode = msg[1];
        let pa_arg = || (msg.len() >= 4).then(|| u16::from(msg[2]) << 8 | u16::from(msg[3]));
        match opcode {
            MSG_USER_CONTROL_PRESSED if msg.len() >= 3 => {
                let key = CecKey::from_ui_command(msg[2]);
                match self.held {
                    // Repeat of the held key: keep holding.
                    Some((k, ref mut t)) if k == key => *t = now,
                    _ => {
                        self.release(out);
                        self.held = Some((key, now));
                        out.push(Out::Event(PlatformEvent::CecDown { key }));
                    }
                }
            }
            MSG_USER_CONTROL_RELEASED => self.release(out),
            MSG_STANDBY => {
                self.release(out);
                self.active = false;
                out.push(Out::Event(PlatformEvent::CecStandby));
            }
            MSG_GIVE_DEVICE_POWER_STATUS => out.push(Out::Transmit(vec![
                self.header(initiator),
                MSG_REPORT_POWER_STATUS,
                POWER_STATUS_ON,
            ])),
            // Some TVs only forward remote keys to devices whose menu is
            // reported active.
            MSG_MENU_REQUEST => out.push(Out::Transmit(vec![
                self.header(initiator),
                MSG_MENU_STATUS,
                MENU_STATE_ACTIVATED,
            ])),
            MSG_REQUEST_ACTIVE_SOURCE if self.active => {
                out.push(Out::Transmit(self.active_source()))
            }
            MSG_SET_STREAM_PATH => {
                if pa_arg() == Some(self.phys_addr) {
                    self.active = true;
                    out.push(Out::Transmit(self.active_source()));
                }
            }
            MSG_ACTIVE_SOURCE => {
                if pa_arg().is_some_and(|pa| pa != self.phys_addr) {
                    self.active = false;
                }
            }
            MSG_FEATURE_ABORT => {}
            _ => {}
        }
    }

    /// Releases a held key whose release message never came.
    pub fn tick(&mut self, now: Instant, out: &mut Vec<Out>) {
        if self.held.is_some_and(|(_, t)| now.duration_since(t) >= HOLD_TIMEOUT) {
            self.release(out);
        }
    }
}

// ---- reader thread ----------------------------------------------------------

fn transmit(f: &File, bytes: &[u8]) {
    let mut m: CecMsg = zeroed();
    let n = bytes.len().min(16);
    m.msg[..n].copy_from_slice(&bytes[..n]);
    m.len = n as u32;
    if let Err(e) = ioctl_ptr(f, CEC_TRANSMIT, &mut m) {
        tracing::debug!(opcode = bytes.get(1), "CEC transmit failed: {e}");
    }
}

fn claim(f: &File, caps: u32, osd_name: &str) {
    let mut la: CecLogAddrs = zeroed();
    if ioctl_ptr(f, CEC_ADAP_G_LOG_ADDRS, &mut la).is_ok() && la.num_log_addrs > 0 {
        tracing::info!("CEC logical address already configured; using it");
        return;
    }
    if caps & CEC_CAP_LOG_ADDRS == 0 {
        tracing::warn!("CEC adapter does not let applications claim a logical address");
        return;
    }
    let mut la: CecLogAddrs = zeroed();
    la.cec_version = CEC_OP_CEC_VERSION_1_4;
    la.num_log_addrs = 1;
    la.vendor_id = CEC_VENDOR_ID_NONE;
    for (d, s) in la.osd_name.iter_mut().zip(osd_name.bytes().take(14)) {
        *d = s as c_char;
    }
    la.primary_device_type[0] = CEC_OP_PRIM_DEVTYPE_PLAYBACK;
    la.log_addr_type[0] = CEC_LOG_ADDR_TYPE_PLAYBACK;
    la.all_device_types[0] = CEC_OP_ALL_DEVTYPE_PLAYBACK;
    // Blocks until claimed (or until the TV provides a physical address).
    if let Err(e) = ioctl_ptr(f, CEC_ADAP_S_LOG_ADDRS, &mut la) {
        tracing::warn!("CEC: claiming a playback logical address failed: {e}");
    }
}

fn addresses(f: &File) -> (Option<u8>, Option<u16>) {
    let mut la: CecLogAddrs = zeroed();
    let mut pa: u16 = CEC_PHYS_ADDR_INVALID;
    let la_ok = ioctl_ptr(f, CEC_ADAP_G_LOG_ADDRS, &mut la).is_ok()
        && la.num_log_addrs > 0
        && la.log_addr[0] != CEC_LOG_ADDR_INVALID;
    let _ = ioctl_ptr(f, CEC_ADAP_G_PHYS_ADDR, &mut pa);
    (la_ok.then_some(la.log_addr[0]), (pa != CEC_PHYS_ADDR_INVALID).then_some(pa))
}

/// How long to wait for a logical address before warning that the CEC
/// line seems dead.
const SILENT_WARN_AFTER: Duration = Duration::from_secs(10);

fn run(
    file: File,
    caps: u32,
    opts: CecOptions,
    tx: Sender<PlatformEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stop: Arc<AtomicBool>,
) {
    claim(&file, caps, &opts.osd_name);
    let mut h = Handler::default();
    let mut announced = false;
    let mut out = Vec::new();
    let mut silent_warn_at = Some(Instant::now() + SILENT_WARN_AFTER);
    while !stop.load(Ordering::Relaxed) {
        // Announce once both addresses are known (the TV may supply the
        // physical address only after it is switched on).
        if !announced && let (Some(la), Some(pa)) = addresses(&file) {
            h.log_addr = la;
            h.phys_addr = pa;
            tracing::info!(
                logical = la,
                physical = %fmt_physical_address(pa),
                "CEC ready as playback device"
            );
            if opts.wake_tv {
                transmit(&file, &h.image_view_on());
            }
            h.active = true;
            transmit(&file, &h.active_source());
            announced = true;
            silent_warn_at = None;
        }
        // Connected (physical address known) but no logical address: the
        // kernel's address polls get no valid response, i.e. the CEC wire
        // is not connected end to end. Typical with DisplayPort-to-HDMI
        // adapters that report CEC tunneling but don't wire the CEC pin.
        if silent_warn_at.is_some_and(|t| Instant::now() >= t) {
            silent_warn_at = None;
            if let (None, Some(pa)) = addresses(&file) {
                tracing::warn!(
                    physical = %fmt_physical_address(pa),
                    "HDMI-CEC: could not claim an address; nothing answers on the CEC \
                     line. Check that CEC is on in the TV's settings; a DisplayPort-to-HDMI \
                     adapter may not carry CEC (see README)"
                );
            }
        }

        let mut pfd = PollFd { fd: file.as_raw_fd(), events: POLLIN | POLLPRI, revents: 0 };
        // SAFETY: one valid pollfd; 200 ms timeout so stop and hold
        // timeouts are noticed.
        let n = unsafe { poll(&mut pfd, 1, 200) };
        let now = Instant::now();
        if n > 0 && pfd.revents & POLLPRI != 0 {
            let mut ev: CecEvent = zeroed();
            if ioctl_ptr(&file, CEC_DQEVENT, &mut ev).is_ok() && ev.event == CEC_EVENT_STATE_CHANGE
            {
                // Addresses changed (TV power cycle, cable): re-announce.
                announced = false;
            }
        }
        if n > 0 && pfd.revents & POLLIN != 0 {
            let mut m: CecMsg = zeroed();
            if ioctl_ptr(&file, CEC_RECEIVE, &mut m).is_ok() {
                let len = (m.len as usize).min(16);
                h.on_message(&m.msg[..len], now, &mut out);
            }
        }
        h.tick(now, &mut out);
        let mut woke = false;
        for o in out.drain(..) {
            match o {
                Out::Transmit(bytes) => transmit(&file, &bytes),
                Out::Event(e) => {
                    if tx.send(e).is_err() {
                        return; // platform gone
                    }
                    woke = true;
                }
            }
        }
        if woke {
            wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_sizes_match_linux_cec_h() {
        // Values printed from linux/cec.h with gcc (docs/DECISIONS.md D26).
        assert_eq!(size_of::<CecMsg>(), 0x38);
        assert_eq!(size_of::<CecLogAddrs>(), 0x5c);
        assert_eq!(size_of::<CecCaps>(), 0x4c);
        assert_eq!(size_of::<CecEvent>(), 0x50);
        assert_eq!(std::mem::offset_of!(CecMsg, msg), 0x20);
        assert_eq!(std::mem::offset_of!(CecMsg, reply), 0x30);
        assert_eq!(std::mem::offset_of!(CecLogAddrs, vendor_id), 0x8);
        assert_eq!(std::mem::offset_of!(CecLogAddrs, osd_name), 0x10);
        assert_eq!(std::mem::offset_of!(CecLogAddrs, primary_device_type), 0x1f);
        // The ioctl numbers encode the argument size.
        let size = |req: c_ulong| ((req >> 16) & 0x3fff) as usize;
        assert_eq!(size(CEC_ADAP_G_CAPS), size_of::<CecCaps>());
        assert_eq!(size(CEC_ADAP_S_LOG_ADDRS), size_of::<CecLogAddrs>());
        assert_eq!(size(CEC_RECEIVE), size_of::<CecMsg>());
        assert_eq!(size(CEC_DQEVENT), size_of::<CecEvent>());
    }

    fn handler() -> Handler {
        Handler { log_addr: 4, phys_addr: 0x1000, active: true, held: None }
    }

    fn events(out: &[Out]) -> Vec<PlatformEvent> {
        out.iter().filter_map(|o| if let Out::Event(e) = o { Some(*e) } else { None }).collect()
    }

    #[test]
    fn press_repeat_release() {
        let mut h = handler();
        let t0 = Instant::now();
        let mut out = Vec::new();
        h.on_message(&[0x04, 0x44, 0x01], t0, &mut out); // TV -> us: Up pressed
        h.on_message(&[0x04, 0x44, 0x01], t0 + Duration::from_millis(400), &mut out); // repeat
        h.on_message(&[0x04, 0x45], t0 + Duration::from_millis(600), &mut out);
        assert_eq!(
            events(&out),
            vec![
                PlatformEvent::CecDown { key: CecKey::Up },
                PlatformEvent::CecUp { key: CecKey::Up }
            ]
        );
    }

    #[test]
    fn new_key_releases_previous_and_timeout_releases() {
        let mut h = handler();
        let t0 = Instant::now();
        let mut out = Vec::new();
        h.on_message(&[0x04, 0x44, 0x01], t0, &mut out);
        h.on_message(&[0x04, 0x44, 0x00], t0, &mut out); // Select without a release
        h.tick(t0 + HOLD_TIMEOUT, &mut out);
        assert_eq!(
            events(&out),
            vec![
                PlatformEvent::CecDown { key: CecKey::Up },
                PlatformEvent::CecUp { key: CecKey::Up },
                PlatformEvent::CecDown { key: CecKey::Select },
                PlatformEvent::CecUp { key: CecKey::Select },
            ]
        );
    }

    #[test]
    fn answers_power_and_menu_queries() {
        let mut h = handler();
        let mut out = Vec::new();
        h.on_message(&[0x04, MSG_GIVE_DEVICE_POWER_STATUS], Instant::now(), &mut out);
        h.on_message(&[0x04, MSG_MENU_REQUEST, 0x02], Instant::now(), &mut out);
        assert_eq!(
            out,
            vec![
                Out::Transmit(vec![0x40, MSG_REPORT_POWER_STATUS, POWER_STATUS_ON]),
                Out::Transmit(vec![0x40, MSG_MENU_STATUS, MENU_STATE_ACTIVATED]),
            ]
        );
    }

    #[test]
    fn active_source_tracking() {
        let mut h = handler();
        let now = Instant::now();
        let mut out = Vec::new();
        h.on_message(&[0x0f, MSG_ACTIVE_SOURCE, 0x20, 0x00], now, &mut out); // someone else
        assert!(!h.active);
        h.on_message(&[0x0f, MSG_REQUEST_ACTIVE_SOURCE], now, &mut out);
        assert!(out.is_empty(), "inactive devices stay quiet");
        h.on_message(&[0x0f, MSG_SET_STREAM_PATH, 0x10, 0x00], now, &mut out); // our path
        assert!(h.active);
        assert_eq!(out, vec![Out::Transmit(vec![0x4f, MSG_ACTIVE_SOURCE, 0x10, 0x00])]);
    }

    #[test]
    fn standby_releases_and_notifies() {
        let mut h = handler();
        let now = Instant::now();
        let mut out = Vec::new();
        h.on_message(&[0x04, 0x44, 0x02], now, &mut out);
        h.on_message(&[0x0f, MSG_STANDBY], now, &mut out);
        assert_eq!(
            events(&out),
            vec![
                PlatformEvent::CecDown { key: CecKey::Down },
                PlatformEvent::CecUp { key: CecKey::Down },
                PlatformEvent::CecStandby,
            ]
        );
    }

    #[test]
    fn ui_command_codes() {
        assert_eq!(CecKey::from_ui_command(0x0d), CecKey::Back);
        assert_eq!(CecKey::from_ui_command(0x25), CecKey::Number(5));
        assert_eq!(CecKey::from_ui_command(0x61), CecKey::PlayPause);
        assert_eq!(CecKey::from_ui_command(0xee), CecKey::Other(0xee));
    }

    #[test]
    fn physical_address_format() {
        assert_eq!(fmt_physical_address(0x1200), "1.2.0.0");
    }

    #[test]
    fn no_device_is_an_error() {
        if !std::path::Path::new("/dev/cec0").exists() {
            assert!(probe(None).is_err());
        }
    }
}
