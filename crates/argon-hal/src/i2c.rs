// SPDX-License-Identifier: GPL-3.0-or-later
//! I2C transport, and the decorators that make the safety policy structural.
//!
//! The central idea: a write that must not happen is prevented by the *type of the
//! transport*, not by an `if` somewhere up the call stack. A driver holding a
//! [`ReadOnly`]-wrapped bus cannot write, whatever it tries, and no future contributor can
//! reintroduce the possibility by forgetting a flag they never saw.

use crate::{Error, Result};
use std::time::{Duration, Instant};

/// A byte-level I2C bus.
pub trait I2cBus {
    /// Writes bytes to a device.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or [`Error::WriteBlocked`] when the transport forbids writes.
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()>;

    /// Checks whether a device acknowledges its address, transferring **no data byte**.
    ///
    /// This is the only probe permitted against an unidentified device. An `SMBus` register
    /// read is not a safe alternative: it places the register number on the bus as a write
    /// before the repeated start, and firmware that implements only the documented
    /// single-byte protocol reads `0x80` as a fan duty of 128. See ADR-0002.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    fn probe(&mut self, addr: u8) -> Result<bool>;

    /// A short description for logs and dry-run output.
    fn describe(&self) -> String;
}

/// A real Linux I2C bus.
pub struct LinuxI2c {
    dev: i2cdev::linux::LinuxI2CDevice,
    path: String,
    addr: u16,
}

impl LinuxI2c {
    /// Opens a bus for a fixed device address.
    ///
    /// # Errors
    ///
    /// Fails if the device node cannot be opened.
    pub fn open(path: &str, addr: u16) -> Result<Self> {
        let dev = i2cdev::linux::LinuxI2CDevice::new(path, addr)
            .map_err(|e| Error::Io(std::io::Error::other(format!("{path}: {e}"))))?;
        Ok(Self {
            dev,
            path: path.to_owned(),
            addr,
        })
    }
}

impl I2cBus for LinuxI2c {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        use i2cdev::core::I2CDevice;
        if u16::from(addr) != self.addr {
            return Err(Error::Io(std::io::Error::other(format!(
                "bus is bound to 0x{:02x}, refusing a write to 0x{addr:02x}",
                self.addr
            ))));
        }
        self.dev
            .write(data)
            .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
    }

    fn probe(&mut self, addr: u8) -> Result<bool> {
        use i2cdev::core::I2CDevice;
        if u16::from(addr) != self.addr {
            return Ok(false);
        }
        // `SMBus` quick-write: address plus the write bit, zero data bytes, stop. The same
        // transaction i2cdetect uses, and the only one with no data byte for legacy firmware
        // to misinterpret.
        Ok(self.dev.smbus_write_quick(false).is_ok())
    }

    fn describe(&self) -> String {
        format!("{} @ 0x{:02x}", self.path, self.addr)
    }
}

/// Reads registers from one device, and can do nothing else.
///
/// A register read writes the register number before a repeated start: `S addr+W reg Sr
/// addr+R data.. P`. That byte is harmless only on a device whose datasheet says it just moves
/// a pointer -- which is **not** true of the ONE-family MCU (ADR-0002). So this is a separate
/// trait from [`I2cBus`], used only for devices identified and documented as safe to read,
/// and it has no write method at all: a driver holding one cannot write, whatever it tries.
pub trait RegisterRead {
    /// Reads `buf.len()` bytes starting at `register`, in one transaction.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    fn read_registers(&mut self, register: u8, buf: &mut [u8]) -> Result<()>;

    /// A short description for logs.
    fn describe(&self) -> String;
}

impl RegisterRead for LinuxI2c {
    fn read_registers(&mut self, register: u8, buf: &mut [u8]) -> Result<()> {
        use i2cdev::core::{I2CMessage, I2CTransfer};
        use i2cdev::linux::LinuxI2CMessage;
        let reg = [register];
        // One transfer, so nothing else on the bus -- the vendor's daemon, say -- can move the
        // pointer between the write and the read, and a two-byte value cannot tear.
        let mut msgs = [LinuxI2CMessage::write(&reg), LinuxI2CMessage::read(buf)];
        self.dev
            .transfer(&mut msgs)
            .map(|_| ())
            .map_err(|e| Error::Io(std::io::Error::other(format!("{}: {e}", self.path))))
    }

    fn describe(&self) -> String {
        format!("{} @ 0x{:02x}", self.path, self.addr)
    }
}

/// Forbids every write. Wraps the bus used in [`crate::mode::Mode::ReadOnly`].
pub struct ReadOnly<T>(pub T);

impl<T: I2cBus> I2cBus for ReadOnly<T> {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        Err(Error::WriteBlocked {
            what: format!("i2c 0x{addr:02x} <- {}", hex(data)),
            reason: "transport is read-only",
        })
    }

    fn probe(&mut self, addr: u8) -> Result<bool> {
        self.0.probe(addr)
    }

    fn describe(&self) -> String {
        format!("{} (read-only)", self.0.describe())
    }
}

/// Records writes instead of performing them.
///
/// A decorator rather than a branch: because it sits at the bottom of the stack, nothing
/// above it can bypass it, and `--dry-run` cannot be partially implemented.
pub struct DryRun<T> {
    inner: T,
    writes: Vec<(u8, Vec<u8>)>,
}

impl<T: I2cBus> DryRun<T> {
    /// Wraps a bus.
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            writes: Vec::new(),
        }
    }

    /// The writes that would have been performed, in order.
    #[must_use]
    pub fn writes(&self) -> &[(u8, Vec<u8>)] {
        &self.writes
    }
}

impl<T: I2cBus> I2cBus for DryRun<T> {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        self.writes.push((addr, data.to_vec()));
        Ok(())
    }

    fn probe(&mut self, addr: u8) -> Result<bool> {
        self.inner.probe(addr)
    }

    fn describe(&self) -> String {
        format!("{} (dry run)", self.inner.describe())
    }
}

/// Enforces a minimum interval between writes.
///
/// A temperature oscillating around a curve boundary would otherwise produce a burst of bus
/// traffic, and the MCU is not the only thing on that bus.
pub struct RateLimited<T> {
    inner: T,
    min_interval: Duration,
    last_write: Option<Instant>,
    suppressed: usize,
}

impl<T: I2cBus> RateLimited<T> {
    /// Wraps a bus with a minimum write interval.
    pub const fn new(inner: T, min_interval: Duration) -> Self {
        Self {
            inner,
            min_interval,
            last_write: None,
            suppressed: 0,
        }
    }

    /// How many writes have been suppressed.
    ///
    /// Exposed so a daemon can surface it as a metric: a rate limiter that is constantly
    /// firing is telling you something about the curve, and a silent one tells you nothing.
    #[must_use]
    pub const fn suppressed(&self) -> usize {
        self.suppressed
    }
}

impl<T: I2cBus> I2cBus for RateLimited<T> {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        if let Some(last) = self.last_write {
            let elapsed = last.elapsed();
            if elapsed < self.min_interval {
                self.suppressed += 1;
                // Reported, not swallowed. Returning Ok here would tell the caller the device
                // now holds this value; a caller that writes only on change would then never
                // send it again. That is exactly how a fan sat at 55% for twelve minutes
                // while the daemon logged that it was off.
                return Err(Error::RateLimited {
                    retry_after: self.min_interval.saturating_sub(elapsed),
                });
            }
        }
        self.inner.write(addr, data)?;
        self.last_write = Some(Instant::now());
        Ok(())
    }

    fn probe(&mut self, addr: u8) -> Result<bool> {
        self.inner.probe(addr)
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

/// Formats bytes for a log line.
fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
