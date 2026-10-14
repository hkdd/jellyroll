//! The text console underneath (D70, D78).
//!
//! On KMSDRM, SDL keeps the console's framebuffer and points the screen
//! back at it whenever it rebuilds its surfaces: on every mode change (the
//! refresh-rate switch before and after a video) it first restores the
//! console's mode and framebuffer, then sets the new mode on the next swap.
//! Whatever the console last drew shows for that moment.
//!
//! So the console is cleared, its cursor hidden, and then put in graphics
//! mode (`KDSETMODE KD_GRAPHICS`, as X, Weston and Plymouth do): the kernel
//! stops drawing on it, so boot status lines, kernel messages and the cursor
//! can't reach the framebuffer SDL shows. What shows is black.
//!
//! On exit (and on a panic, which unwinds) it goes back to text mode with
//! the cursor visible. If the process is killed, the service's `TTYReset=`
//! puts the console back in text mode when it restarts or stops.

use std::ffi::{c_int, c_ulong};
use std::io::{IsTerminal, Write};
use std::os::fd::AsRawFd;

/// Clear screen and scrollback, cursor home, cursor hidden.
const BLANK: &[u8] = b"\x1b[2J\x1b[3J\x1b[H\x1b[?25l";
/// Cursor visible again.
const RESTORE: &[u8] = b"\x1b[?25h";

/// `<linux/kd.h>`.
const KDSETMODE: c_ulong = 0x4B3A;
const KD_TEXT: c_ulong = 0x00;
const KD_GRAPHICS: c_ulong = 0x01;

unsafe extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

/// `KDSETMODE` on `f`; fails (ENOTTY) unless it is a virtual console.
fn set_kd_mode(f: &std::fs::File, mode: c_ulong) -> std::io::Result<()> {
    // SAFETY: KDSETMODE takes its argument by value (an integer mode), not
    // a pointer; `f` is an open descriptor for the duration of the call.
    let rc = unsafe { ioctl(f.as_raw_fd(), KDSETMODE, mode) };
    if rc < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

/// Puts the console back (text mode, cursor) when dropped.
pub struct Console {
    tty: Option<std::fs::File>,
    graphics: bool,
}

impl Console {
    /// Blanks the controlling terminal, if there is one (the VT the service
    /// runs on, or the console it was started from). Nothing otherwise; a
    /// terminal that isn't a virtual console (ssh) is only cleared.
    pub fn blank() -> Self {
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .ok()
            .filter(|f| f.is_terminal());
        let mut c = Self::blank_on(tty);
        if let Some(f) = &c.tty {
            match set_kd_mode(f, KD_GRAPHICS) {
                Ok(()) => {
                    c.graphics = true;
                    tracing::info!("console in graphics mode under the display");
                }
                Err(e) => tracing::info!("console left in text mode: {e}"),
            }
        }
        c
    }

    fn blank_on(tty: Option<std::fs::File>) -> Self {
        let tty = tty.and_then(|mut f| {
            if let Err(e) = f.write_all(BLANK).and_then(|_| f.flush()) {
                tracing::debug!("console: {e}");
                return None;
            }
            Some(f)
        });
        if tty.is_some() {
            tracing::debug!("console cleared under the display");
        }
        Self { tty, graphics: false }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if let Some(f) = &mut self.tty {
            if self.graphics
                && let Err(e) = set_kd_mode(f, KD_TEXT)
            {
                tracing::warn!("console back to text mode: {e}");
            }
            let _ = f.write_all(RESTORE).and_then(|_| f.flush());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanks_then_restores_the_cursor() {
        let p = std::env::temp_dir().join(format!("jellyroll-console-{}", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        drop(Console::blank_on(Some(f)));
        let mut want = BLANK.to_vec();
        want.extend_from_slice(RESTORE);
        assert_eq!(std::fs::read(&p).unwrap(), want);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn graphics_mode_needs_a_virtual_console() {
        let p = std::env::temp_dir().join(format!("jellyroll-console-kd-{}", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        assert!(set_kd_mode(&f, KD_GRAPHICS).is_err(), "a plain file isn't a console");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn no_terminal_is_fine() {
        drop(Console::blank_on(None));
    }
}
