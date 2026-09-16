// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux transports and read-only hardware discovery for Argon40 devices.
//!
//! Everything in this crate that touches a device is either strictly read-only or is a
//! transaction documented as safe on every firmware generation. Discovery in particular
//! reads sysfs and never opens a serial port: the vendor's daemon may hold `/dev/ttyACM0`,
//! and CDC-ACM has no arbitration at all — two readers silently split the byte stream and
//! desynchronise. Identity therefore comes from USB descriptors in sysfs, not from talking
//! to the device.

pub mod discovery;
pub mod foreign;
pub mod gpio;
pub mod hidraw;
pub mod i2c;
pub mod mode;
pub mod platform;
pub mod serial;
pub mod thermal;

use std::io;

/// Anything that can go wrong while inspecting the system.
#[derive(Debug)]
pub enum Error {
    /// A filesystem read failed.
    Io(io::Error),
    /// An operation did not complete before its deadline.
    Timeout,
    /// A write was refused by the transport's policy.
    WriteBlocked {
        /// What would have been written.
        what: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// A sysfs value was present but not in the expected form.
    Parse {
        /// What was being read.
        what: &'static str,
        /// The value that could not be parsed.
        got: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("timed out"),
            Self::WriteBlocked { what, reason } => write!(f, "refused {what}: {reason}"),
            Self::Parse { what, got } => write!(f, "could not parse {what} from {got:?}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Timeout | Self::WriteBlocked { .. } | Self::Parse { .. } => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Shorthand for this crate's results.
pub type Result<T> = std::result::Result<T, Error>;

/// Reads a sysfs file, trimming whitespace and trailing NULs.
///
/// Device-tree properties are NUL-terminated, which is the usual reason a naive read of
/// `/proc/device-tree/model` compares unequal to the string it visibly contains.
pub(crate) fn read_trimmed(path: impl AsRef<std::path::Path>) -> io::Result<String> {
    let s = std::fs::read_to_string(path)?;
    Ok(s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
        .to_owned())
}
