// SPDX-License-Identifier: GPL-3.0-or-later
//! Radio switches: Wi-Fi and Bluetooth, through the kernel's rfkill interface.
//!
//! State is read from `/sys/class/rfkill`, which anyone may read. Changes are written to
//! `/dev/rfkill` as the kernel's documented event (`include/uapi/linux/rfkill.h`): on Raspberry
//! Pi OS the logged-in user may write it (an ACL from logind), so a session agent needs no
//! extra privilege. Only the soft block is ever changed; a hard block is a switch or firmware
//! decision and is left alone.

use crate::{Error, Result};
use std::io::Write;
use std::path::Path;

const SYSFS: &str = "/sys/class/rfkill";
const DEVICE: &str = "/dev/rfkill";

/// `RFKILL_TYPE_ALL`: with `RFKILL_OP_CHANGE`, match the index whatever its type.
const TYPE_ALL: u8 = 0;
/// `RFKILL_OP_CHANGE`: change one switch, by index.
const OP_CHANGE: u8 = 2;

/// What a radio is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Wi-Fi.
    Wlan,
    /// Bluetooth.
    Bluetooth,
    /// Anything else rfkill knows about (WWAN, NFC, ...), never touched here.
    Other,
}

/// One radio switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Radio {
    /// The kernel's index for it, as used in events.
    pub index: u32,
    /// What it is.
    pub kind: Kind,
    /// Its name, e.g. `phy0` or `hci0`.
    pub name: String,
    /// Turned off in software.
    pub soft_blocked: bool,
    /// Turned off by a switch or firmware.
    pub hard_blocked: bool,
}

/// The radios the kernel knows about. Empty if there is no rfkill support.
#[must_use]
pub fn radios() -> Vec<Radio> {
    radios_in(Path::new(SYSFS))
}

fn radios_in(root: &Path) -> Vec<Radio> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let read = |dir: &Path, f: &str| {
        std::fs::read_to_string(dir.join(f))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut out: Vec<Radio> = entries
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let index = read(&dir, "index").parse().ok()?;
            Some(Radio {
                index,
                kind: match read(&dir, "type").as_str() {
                    "wlan" => Kind::Wlan,
                    "bluetooth" => Kind::Bluetooth,
                    _ => Kind::Other,
                },
                name: read(&dir, "name"),
                soft_blocked: read(&dir, "soft") == "1",
                hard_blocked: read(&dir, "hard") == "1",
            })
        })
        .collect();
    out.sort_by_key(|r| r.index);
    out
}

/// The event that sets one radio's soft block.
#[must_use]
pub fn change_event(index: u32, block: bool) -> [u8; 8] {
    let i = index.to_ne_bytes();
    [
        i[0],
        i[1],
        i[2],
        i[3],
        TYPE_ALL,
        OP_CHANGE,
        u8::from(block),
        0,
    ]
}

/// Turns one radio's soft block on or off.
///
/// # Errors
///
/// Fails if `/dev/rfkill` cannot be opened for writing or the write fails.
pub fn set_soft_block(index: u32, block: bool) -> Result<()> {
    let mut dev = std::fs::OpenOptions::new()
        .write(true)
        .open(DEVICE)
        .map_err(|e| Error::Io(std::io::Error::other(format!("{DEVICE}: {e}"))))?;
    dev.write_all(&change_event(index, block))
        .map_err(|e| Error::Io(std::io::Error::other(format!("{DEVICE}: {e}"))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_change_event_has_the_kernel_layout() {
        // struct rfkill_event { __u32 idx; __u8 type; __u8 op; __u8 soft; __u8 hard; }
        let e = change_event(1, true);
        assert_eq!(u32::from_ne_bytes([e[0], e[1], e[2], e[3]]), 1);
        assert_eq!(&e[4..], &[0, 2, 1, 0]);
        assert_eq!(change_event(0, false)[6], 0);
    }

    #[test]
    fn reads_the_radios_as_the_one_up_lists_them() {
        let root = std::env::temp_dir().join(format!("argon-rfkill-{}", std::process::id()));
        for (n, kind, name, soft) in [(0, "bluetooth", "hci0", "0"), (1, "wlan", "phy0", "1")] {
            let d = root.join(format!("rfkill{n}"));
            std::fs::create_dir_all(&d).unwrap();
            for (f, v) in [
                ("index", n.to_string()),
                ("type", kind.into()),
                ("name", name.into()),
                ("soft", soft.into()),
                ("hard", "0".into()),
            ] {
                std::fs::write(d.join(f), format!("{v}\n")).unwrap();
            }
        }
        let got = radios_in(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].kind, got[0].soft_blocked), (Kind::Bluetooth, false));
        assert_eq!(
            (got[1].kind, got[1].name.as_str(), got[1].soft_blocked),
            (Kind::Wlan, "phy0", true)
        );
    }
}
