//! The text console underneath (D70).
//!
//! On KMSDRM, SDL keeps the console's framebuffer and points the screen
//! back at it whenever it rebuilds its surfaces: on a mode change (the
//! refresh-rate switch before a video) the console shows until SDL presents
//! a frame in the new mode. So the console is cleared, and its cursor
//! hidden, before SDL takes the display: what shows for that moment is
//! black. On exit the cursor comes back (a shell started us, or the
//! service's `TTYReset=` resets the terminal anyway).
//!
//! Escape sequences only, no `KDSETMODE` graphics mode: a crash would leave
//! a graphics-mode console blank until something reset it.

use std::io::{IsTerminal, Write};

/// Clear screen and scrollback, cursor home, cursor hidden.
const BLANK: &[u8] = b"\x1b[2J\x1b[3J\x1b[H\x1b[?25l";
/// Cursor visible again.
const RESTORE: &[u8] = b"\x1b[?25h";

/// Restores the cursor when dropped.
pub struct Console {
    tty: Option<std::fs::File>,
}

impl Console {
    /// Blanks the controlling terminal, if there is one (the VT the service
    /// runs on, or the console it was started from). Nothing otherwise.
    pub fn blank() -> Self {
        let tty = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .ok()
            .filter(|f| f.is_terminal());
        Self::blank_on(tty)
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
        Self { tty }
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if let Some(f) = &mut self.tty {
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
    fn no_terminal_is_fine() {
        drop(Console::blank_on(None));
    }
}
