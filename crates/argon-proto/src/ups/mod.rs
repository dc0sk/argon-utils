// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! Argon PWR UPS serial protocol.
//!
//! Facts: `ARGON-UPS-FRAME`, `ARGON-UPS-READSHORT`, `ARGON-UPS-CMD*`.
//!
//! All framing facts are currently `inferred` in `docs/protocol/FACTS.md` — derived from
//! upstream behaviour rather than from a published specification or our own capture. The
//! codec is implemented because a pure function cannot harm a device, but per the
//! provenance rules in `CLEANROOM.md` the *write* paths that put these bytes on the wire
//! stay gated until the facts are promoted to `observed` by our own capture.

mod frame;
pub mod policy;
mod status;

pub use frame::{Frame, FrameError, FrameReader, MAX_PAYLOAD, START_BYTE, checksum, encode_read};
pub use status::{BatteryStatus, DecodeError, PowerSource, UpsTime};

/// Command identifiers understood by the UPS (`ARGON-UPS-CMD*`).
///
/// Deliberately not a `From<u8>` conversion over the whole byte range: the command space
/// above [`Command::ResetMeter`] is unmapped, command 9 is already destructive, and
/// sweeping unknown command IDs on a device that manages your power is how you discover a
/// factory reset. Unknown IDs stay unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Command {
    /// Battery status. Response is `[percent, charging]`; `charging == 0` means on mains.
    BatteryStatus = 0,
    /// Charge current, 16-bit big-endian. **Units are unknown** — never publish this as amps.
    ChargeCurrent = 2,
    /// Set the RTC from six BCD bytes `YY MM DD HH MM SS`, UTC.
    SetRtc = 3,
    /// Firmware version, one byte.
    FirmwareVersion = 4,
    /// Read the RTC as six BCD bytes.
    GetRtc = 5,
    /// Set an absolute wake schedule from five BCD bytes `YY MM DD HH MM`, UTC.
    SetWake = 6,
    /// Read the wake schedule as five BCD bytes.
    GetWake = 7,
    /// Device-initiated acknowledgement request; the host echoes it back.
    Acknowledge = 8,
    /// Reset the battery meter. **Destructive** — discards the meter's baseline.
    ResetMeter = 9,
}

impl Command {
    /// The wire byte for this command.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Whether this command mutates device state that survives the call.
    ///
    /// Used by the policy layer to decide which operations need the `full` mode gate and an
    /// explicit confirmation.
    #[must_use]
    pub const fn is_destructive(self) -> bool {
        matches!(self, Self::ResetMeter | Self::SetRtc | Self::SetWake)
    }

    /// Recognises a command byte, returning `None` for anything unmapped.
    #[must_use]
    pub const fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::BatteryStatus,
            2 => Self::ChargeCurrent,
            3 => Self::SetRtc,
            4 => Self::FirmwareVersion,
            5 => Self::GetRtc,
            6 => Self::SetWake,
            7 => Self::GetWake,
            8 => Self::Acknowledge,
            9 => Self::ResetMeter,
            _ => return None,
        })
    }
}
