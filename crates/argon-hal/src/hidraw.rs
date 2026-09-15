// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading a HID device through `hidraw`.
//!
//! Deliberately `hidraw` and not libusb. A libusb interface claim detaches the kernel driver
//! and does not restore it, which on the Argon UPS takes `/dev/ttyACM0` down with it and
//! breaks whatever was using the serial port. `hidraw` rides the existing `usbhid` binding
//! on interface 2 and touches nothing else. See `ARGON-UPS-HID-ACCESS` in
//! `docs/protocol/FACTS.md`, and the incident in
//! `docs/protocol/captures/OBS-2026-09-15-nut-usbhid-ups.md`.
//!
//! Only Input reports are read, via `read()`. Feature reports would need `HIDIOCGFEATURE`,
//! an ioctl, and are not required for telemetry.

use crate::{Result, read_trimmed};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The largest HID report we will accept in one read.
const MAX_REPORT: usize = 4096;

/// An open `hidraw` device.
#[derive(Debug)]
pub struct HidRaw {
    file: File,
    path: PathBuf,
}

/// One report as delivered by the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Report ID, stripped from the front of the payload.
    pub id: u8,
    /// Payload, excluding the report-ID byte.
    pub payload: Vec<u8>,
}

impl HidRaw {
    /// Opens a hidraw node read-only.
    ///
    /// # Errors
    ///
    /// Fails if the node cannot be opened. `/dev/hidraw*` is `root:root 0600` by default, so
    /// without the shipped udev rule this needs privileges.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let file = File::open(&path)?;
        Ok(Self { file, path })
    }

    /// The device node this was opened from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the report descriptor from sysfs.
    ///
    /// Taken from sysfs rather than via `HIDIOCGRDESC` so that no ioctl, and therefore no
    /// `unsafe`, is needed anywhere in this crate.
    ///
    /// # Errors
    ///
    /// Fails if the sysfs attribute cannot be read.
    pub fn descriptor(&self) -> Result<Vec<u8>> {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let attr = PathBuf::from(format!("/sys/class/hidraw/{name}/device/report_descriptor"));
        let raw = std::fs::read(attr)?;
        Ok(trim_descriptor(&raw))
    }

    /// The device's HID name from sysfs, e.g. `Argon Argon_USB`.
    #[must_use]
    pub fn hid_name(&self) -> Option<String> {
        let name = self.path.file_name()?.to_string_lossy().into_owned();
        let uevent = read_trimmed(format!("/sys/class/hidraw/{name}/device/uevent")).ok()?;
        uevent
            .lines()
            .find_map(|l| l.strip_prefix("HID_NAME="))
            .map(str::to_owned)
    }

    /// Reads one report, waiting up to `timeout`.
    ///
    /// Returns `Ok(None)` on timeout.
    ///
    /// # Errors
    ///
    /// Fails if the read fails for any reason other than timing out.
    pub fn read_report(&mut self, timeout: Duration) -> Result<Option<Report>> {
        if !self.wait_readable(timeout)? {
            return Ok(None);
        }
        let mut buf = [0u8; MAX_REPORT];
        let n = self.file.read(&mut buf)?;
        if n == 0 {
            return Ok(None);
        }
        Ok(Some(Report {
            id: buf[0],
            payload: buf[1..n].to_vec(),
        }))
    }

    /// Collects reports until `deadline`, keeping the most recent of each report ID.
    ///
    /// A device that emits reports only on change can be quiet for a long time, so callers
    /// get whatever arrived rather than blocking for a complete set.
    ///
    /// Note this is a real deadline across the whole operation, not a per-read timeout: a
    /// device dribbling one report per timeout period cannot keep us here indefinitely.
    ///
    /// # Errors
    ///
    /// Fails if a read fails.
    pub fn collect_until(&mut self, deadline: Instant) -> Result<Vec<Report>> {
        let mut seen: Vec<Report> = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Some(r) = self.read_report(remaining)? else {
                break;
            };
            if let Some(slot) = seen.iter_mut().find(|s| s.id == r.id) {
                *slot = r;
            } else {
                seen.push(r);
            }
        }
        seen.sort_by_key(|r| r.id);
        Ok(seen)
    }

    /// Waits for the device to become readable.
    ///
    /// A real readiness check via `poll(2)`, not a sleep loop: hidraw delivers Input reports
    /// only when the device sends them, and a device that has nothing to say can be quiet
    /// for a long time. Returns `false` on timeout.
    fn wait_readable(&self, timeout: Duration) -> Result<bool> {
        use rustix::event::{PollFd, PollFlags, poll};

        let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            let millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
            match poll(
                &mut fds,
                Some(&rustix::event::Timespec {
                    tv_sec: i64::from(millis / 1000),
                    tv_nsec: i64::from(millis % 1000) * 1_000_000,
                }),
            ) {
                Ok(0) => return Ok(false),
                Ok(_) => return Ok(!fds[0].revents().is_empty()),
                // A signal interrupted the wait. Fall through to the next iteration, which
                // recomputes the remaining time -- so the caller's deadline is honoured
                // rather than restarted.
                Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(crate::Error::Io(std::io::Error::from(e))),
            }
        }
    }
}

/// Strips the trailing zero padding a sysfs read adds to a report descriptor.
///
/// The attribute is served from a page-sized buffer, so the tail is padding rather than
/// descriptor content. Walking the items is the only reliable way to find the real end:
/// a zero byte is a legal item prefix, so scanning backwards for the last non-zero byte
/// would truncate a descriptor that legitimately ends in one.
#[must_use]
pub fn trim_descriptor(raw: &[u8]) -> Vec<u8> {
    let mut i = 0usize;
    let mut end = 0usize;
    let mut depth = 0i32;
    while i < raw.len() {
        let prefix = raw[i];
        let raw_size = (prefix & 0x03) as usize;
        let size = if raw_size == 3 { 4 } else { raw_size };
        if i + 1 + size > raw.len() {
            break;
        }
        let tag = prefix & 0xFC;
        if tag == 0xA0 {
            depth += 1;
        } else if tag == 0xC0 {
            depth -= 1;
        }
        i += 1 + size;
        // The descriptor ends when every collection has been closed.
        if depth == 0 && tag == 0xC0 {
            end = i;
        }
    }
    raw[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_sysfs_padding_to_the_last_balanced_collection() {
        let real: &[u8] =
            include_bytes!("../../../docs/protocol/captures/ups-hid-report-descriptor.bin");
        let mut padded = real.to_vec();
        padded.resize(4096, 0);
        assert_eq!(trim_descriptor(&padded), real);
    }

    #[test]
    fn trimming_is_idempotent() {
        let real: &[u8] =
            include_bytes!("../../../docs/protocol/captures/ups-hid-report-descriptor.bin");
        assert_eq!(trim_descriptor(real), real);
        assert_eq!(trim_descriptor(&trim_descriptor(real)), real);
    }

    #[test]
    fn junk_does_not_panic() {
        for seed in 0u16..256 {
            let junk: Vec<u8> = (0..32u16)
                .map(|i| (i.wrapping_mul(seed) & 0xFF) as u8)
                .collect();
            let _ = trim_descriptor(&junk);
        }
    }
}
