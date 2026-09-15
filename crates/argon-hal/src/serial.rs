// SPDX-License-Identifier: GPL-3.0-or-later
//! The UPS serial link.
//!
//! CDC-ACM has **no arbitration**. Two readers on the same port do not take turns — they
//! split the byte stream and both desynchronise mid-frame, silently. So every open is
//! preceded by an ownership check, and [`crate::foreign::port_owners`] exists for that
//! purpose.
//!
//! # Deadlines, not timeouts
//!
//! A per-read timeout is not a bound on an operation. A device that emits one byte just
//! before each timeout expires keeps a caller waiting forever while never once timing out.
//! [`SerialLink::request`] therefore takes an [`Instant`] deadline covering the whole
//! exchange, and the per-read timeout is only used to wake up and re-check it.

use crate::{Error, Result};
use argon_proto::ups::{Frame, FrameError, FrameReader};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// Line rate of the Argon PWR UPS (`ARGON-UPS-SERIAL-PARAMS`).
pub const BAUD: u32 = 115_200;

/// How long a single read may block before the deadline is re-checked.
const READ_SLICE: Duration = Duration::from_millis(100);

/// An open link to the UPS.
pub struct SerialLink {
    port: Box<dyn serialport::SerialPort>,
    reader: FrameReader,
}

impl SerialLink {
    /// Opens a serial port.
    ///
    /// Prefer a `/dev/serial/by-id/...` path. Device numbering is not stable: a single USB
    /// re-enumeration has been observed to move this device from `ttyACM0` to `ttyACM1`,
    /// which the vendor's software could not follow because it hardcodes the former.
    ///
    /// # Errors
    ///
    /// Fails if the port cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_string_lossy().into_owned();
        let port = serialport::new(&path, BAUD)
            .timeout(READ_SLICE)
            .open()
            .map_err(|e| Error::Io(std::io::Error::other(format!("{path}: {e}"))))?;
        Ok(Self::from_port(port))
    }

    /// Wraps an already-open port. Used by the simulator and by tests.
    #[must_use]
    pub fn from_port(port: Box<dyn serialport::SerialPort>) -> Self {
        Self {
            port,
            reader: FrameReader::new(),
        }
    }

    /// Sends a command and waits for a response frame carrying the same command byte.
    ///
    /// Frames for other commands are discarded: the device emits unsolicited frames, and a
    /// reply to something else is not a reply to this.
    ///
    /// # Errors
    ///
    /// Fails on I/O error, or [`Error::Timeout`] if the deadline passes first.
    pub fn request(&mut self, cmd: u8, payload: &[u8], deadline: Instant) -> Result<Frame> {
        let frame = Frame::new(cmd, payload)
            .map_err(|e: FrameError| Error::Io(std::io::Error::other(e.to_string())))?;
        let mut out = [0u8; 260];
        let n = frame
            .encode_into(&mut out)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;

        self.reader.reset();
        self.port.write_all(&out[..n])?;
        self.port.flush()?;

        self.read_matching(cmd, deadline)
    }

    /// Reads until a frame for `cmd` arrives, or the deadline passes.
    fn read_matching(&mut self, cmd: u8, deadline: Instant) -> Result<Frame> {
        let mut buf = [0u8; 256];
        loop {
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            let read = match self.port.read(&mut buf) {
                Ok(0) => continue,
                Ok(n) => n,
                // A read slice expiring is not a failure; it is how we re-check the
                // deadline. Only a genuine error propagates.
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::Io(e)),
            };
            for &b in &buf[..read] {
                // A frame for another command, or a rejected one, or no frame yet: all are
                // expected on a link that can desynchronise. Keep reading rather than
                // failing the request.
                if let Some(Ok(f)) = self.reader.push(b) {
                    if f.cmd() == cmd {
                        return Ok(f);
                    }
                }
            }
        }
    }
}

impl std::fmt::Debug for SerialLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialLink").finish_non_exhaustive()
    }
}
