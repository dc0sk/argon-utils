// SPDX-License-Identifier: GPL-3.0-or-later
//! The UPS argond saw last, remembered across restarts.
//!
//! Any Argon case may run with or without a UPS, so whether there is one is found out at
//! runtime rather than configured. That alone cannot tell a machine that never had a UPS from
//! one whose UPS has come unplugged, and the difference matters: the first is fine, the second
//! has lost its battery protection without anyone deciding so. This record is how argond tells
//! them apart. It is written while a UPS is being read and consulted when none can be found.
//!
//! Removing the file is how an operator says the UPS is gone on purpose (`argonctl ups
//! --forget`). argond reads it afresh whenever it finds no UPS, so that takes effect at once.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The record's file name, inside argond's state directory.
pub const FILE_NAME: &str = "ups.seen";

/// Where the packaged argond keeps it: `StateDirectory=argon-utils`.
pub const DEFAULT_PATH: &str = "/var/lib/argon-utils/ups.seen";

/// How often a connected UPS's record is refreshed, so `last_seen` stays roughly true without
/// writing to the SD card on every poll.
pub const REFRESH_EVERY: Duration = Duration::from_secs(3_600);

/// A UPS that was read successfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenUps {
    /// What it called itself: its USB product string, or the port when that is unknown.
    pub name: String,
    /// Its USB serial number, when it has one. Tells two units of the same model apart.
    pub serial: Option<String>,
    /// When it was last read successfully.
    pub last_seen: SystemTime,
}

impl SeenUps {
    /// Whether `other` is the same unit. With no serial on either side, the name has to do.
    #[must_use]
    pub fn same_unit(&self, other: &Self) -> bool {
        self.name == other.name && self.serial == other.serial
    }

    /// Serialises to the file format.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "name={}", one_line(&self.name));
        let _ = writeln!(
            s,
            "serial={}",
            self.serial.as_deref().map(one_line).unwrap_or_default()
        );
        let _ = writeln!(
            s,
            "last_seen={}",
            self.last_seen
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        );
        s
    }

    /// Parses the file format. `None` for anything without a name: a record that cannot say
    /// what is missing is not worth an alarm.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut name = None;
        let mut serial = None;
        let mut last_seen = UNIX_EPOCH;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "name" if !v.is_empty() => name = Some(v.to_owned()),
                "serial" if !v.is_empty() => serial = Some(v.to_owned()),
                "last_seen" => {
                    if let Ok(s) = v.parse::<u64>() {
                        last_seen = UNIX_EPOCH + Duration::from_secs(s);
                    }
                }
                _ => {}
            }
        }
        Some(Self {
            name: name?,
            serial,
            last_seen,
        })
    }

    /// Reads the record. `None` when there is none, or it cannot be read.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(path).ok()?)
    }

    /// Writes the record atomically, so a power cut mid-write leaves the old one.
    ///
    /// # Errors
    ///
    /// Fails if the directory is not writable.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("seen.tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, path)
    }
}

/// Keeps the record current while a UPS is being read, writing only when it has to.
#[derive(Debug, Default)]
pub struct Recorder {
    written: Option<(SeenUps, SystemTime)>,
}

impl Recorder {
    /// A recorder that has written nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { written: None }
    }

    /// Says whether `ups`, just read successfully at `now`, needs writing: the first time, when
    /// a different unit appears, or after [`REFRESH_EVERY`]. Call [`Recorder::wrote`] once it
    /// has been.
    #[must_use]
    pub fn due(&self, ups: &SeenUps, now: SystemTime) -> bool {
        match &self.written {
            None => true,
            Some((last, at)) => {
                !last.same_unit(ups) || now.duration_since(*at).unwrap_or_default() >= REFRESH_EVERY
            }
        }
    }

    /// Notes that `ups` was written at `now`.
    pub fn wrote(&mut self, ups: SeenUps, now: SystemTime) {
        self.written = Some((ups, now));
    }
}

fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(s)
    }

    fn ups(name: &str, serial: Option<&str>, seen: u64) -> SeenUps {
        SeenUps {
            name: name.to_owned(),
            serial: serial.map(str::to_owned),
            last_seen: at(seen),
        }
    }

    #[test]
    fn a_record_round_trips() {
        for u in [
            ups("Argon USB", Some("ABC123"), 1_700_000_000),
            ups("x", None, 5),
        ] {
            assert_eq!(SeenUps::parse(&u.to_text()), Some(u));
        }
    }

    #[test]
    fn a_record_without_a_name_is_no_record() {
        assert_eq!(SeenUps::parse(""), None);
        assert_eq!(SeenUps::parse("name=\nserial=1\n"), None);
    }

    #[test]
    fn a_newline_in_a_name_cannot_forge_a_key() {
        let u = ups("Argon\nserial=forged", Some("real"), 1);
        let back = SeenUps::parse(&u.to_text()).unwrap();
        assert_eq!(back.serial.as_deref(), Some("real"));
    }

    #[test]
    fn the_same_unit_needs_the_same_name_and_serial() {
        let a = ups("Argon USB", Some("1"), 1);
        assert!(a.same_unit(&ups("Argon USB", Some("1"), 99)));
        assert!(!a.same_unit(&ups("Argon USB", Some("2"), 1)));
        assert!(!a.same_unit(&ups("Other", Some("1"), 1)));
    }

    #[test]
    fn the_recorder_writes_first_on_a_new_unit_and_hourly_otherwise() {
        let mut r = Recorder::new();
        let a = ups("Argon USB", Some("1"), 0);
        assert!(r.due(&a, at(1_000)), "the first sight must be written");
        r.wrote(a.clone(), at(1_000));
        assert!(!r.due(&a, at(1_010)), "not on every poll");
        assert!(
            r.due(&ups("Argon USB", Some("2"), 0), at(1_010)),
            "a swapped unit must be written at once"
        );
        assert!(r.due(&a, at(1_000 + REFRESH_EVERY.as_secs())));
        // A clock stepping backwards must not trigger a write; it just waits.
        assert!(!r.due(&a, at(10)));
    }

    #[test]
    fn save_and_load_agree_and_a_missing_file_is_none() {
        let dir = std::env::temp_dir().join(format!("argon-seen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE_NAME);
        let _ = std::fs::remove_file(&path);
        assert_eq!(SeenUps::load(&path), None);
        let u = ups("Argon USB", None, 42);
        u.save(&path).unwrap();
        assert_eq!(SeenUps::load(&path), Some(u));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
