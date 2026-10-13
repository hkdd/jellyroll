//! Can we read the kernel input devices? On the KMSDRM path SDL reads
//! keyboards, remotes and gamepads directly from /dev/input/event*. Those
//! are root:input 0660, and logind's uaccess rules only open up joysticks,
//! so a user outside the `input` group gets a picture but no keyboard.

use std::path::Path;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct InputAccess {
    pub devices: usize,
    pub readable: usize,
}

impl InputAccess {
    /// True when input devices exist but none can be opened.
    pub fn blocked(&self) -> bool {
        self.devices > 0 && self.readable == 0
    }
}

/// Opens each `event*` node read-only and closes it again. Opening does not
/// grab the device or consume events.
pub fn scan(dir: &Path) -> InputAccess {
    let mut a = InputAccess::default();
    let Ok(entries) = std::fs::read_dir(dir) else { return a };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with("event") {
            continue;
        }
        a.devices += 1;
        if std::fs::File::open(e.path()).is_ok() {
            a.readable += 1;
        }
    }
    a
}

pub const REMEDY: &str = "add this user to the 'input' group \
    (sudo usermod -aG input \"$USER\"), then log out and back in";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_event_nodes_only() {
        let dir = std::env::temp_dir().join(format!("jellyroll-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("event0"), b"").unwrap();
        std::fs::write(dir.join("event1"), b"").unwrap();
        std::fs::write(dir.join("mice"), b"").unwrap();
        let a = scan(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(a, InputAccess { devices: 2, readable: 2 });
        assert!(!a.blocked());
    }

    #[test]
    fn blocked_only_when_devices_exist() {
        assert!(!scan(Path::new("/nonexistent")).blocked());
        assert!(InputAccess { devices: 3, readable: 0 }.blocked());
        assert!(!InputAccess { devices: 3, readable: 1 }.blocked());
    }
}
