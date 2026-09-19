// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! Fan duty cycle as a type that cannot hold an invalid value.
//!
//! Facts: `ARGON-MCU-L-FAN`, `ARGON-MCU-L-FANMIN` (both `documented`).

use core::fmt;

#[cfg(feature = "alloc")]
mod config;
#[cfg(feature = "alloc")]
mod curve;

#[cfg(feature = "alloc")]
pub use config::{ParseError, ParseErrorKind, parse};
#[cfg(feature = "alloc")]
pub use curve::{CurveError, CurvePoint, FanController, FanCurve};

/// The duty cycle of the case fan.
///
/// The MCU accepts `0x00` to stop the fan and `0x01`–`0x64` as a literal percentage
/// (`ARGON-MCU-L-FAN`). Stopping the fan is represented as a distinct variant rather than
/// as `Percent(0)`, so that "turn the fan off" is always a deliberate, greppable choice and
/// never the result of arithmetic landing on zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FanDuty {
    /// Fan stopped.
    Off,
    /// Fan running at the given percentage, guaranteed to be in `1..=100`.
    Percent(u8),
}

/// The lowest duty at which the fan physically begins to turn (`ARGON-MCU-L-FANMIN`).
///
/// Values below this are documented as not spinning the fan at all. A caller asking for
/// 1–9% is therefore asking for something the hardware cannot do; [`FanDuty::clamped`]
/// raises such a request to this floor rather than silently producing a stopped fan.
pub const MIN_SPINNING_DUTY: u8 = 10;

impl FanDuty {
    /// Builds a duty from a raw percentage, rejecting values above 100.
    ///
    /// `0` yields [`FanDuty::Off`].
    pub const fn new(percent: u8) -> Result<Self, InvalidDuty> {
        match percent {
            0 => Ok(Self::Off),
            1..=100 => Ok(Self::Percent(percent)),
            _ => Err(InvalidDuty(percent)),
        }
    }

    /// Builds a duty from a raw percentage, clamping into the range the hardware can act on.
    ///
    /// Values above 100 clamp to 100. Values in `1..MIN_SPINNING_DUTY` are raised to
    /// [`MIN_SPINNING_DUTY`], because the documented hardware behaviour is that they do not
    /// spin the fan — returning [`FanDuty::Off`] for them would turn a request for *some*
    /// cooling into *no* cooling.
    ///
    /// This is deliberately not upstream's behaviour: the official daemon silently rewrites
    /// any configured value below 25 to 25, so a documented `55=10` curve point actually
    /// produces 25%. We either honour the request or raise it to the physical floor, and the
    /// configuration layer rejects out-of-range values loudly rather than rewriting them.
    #[must_use]
    pub const fn clamped(percent: u8) -> Self {
        match percent {
            0 => Self::Off,
            1..MIN_SPINNING_DUTY => Self::Percent(MIN_SPINNING_DUTY),
            MIN_SPINNING_DUTY..=100 => Self::Percent(percent),
            _ => Self::Percent(100),
        }
    }

    /// The byte to write to the MCU for this duty (`ARGON-MCU-L-FAN`).
    #[must_use]
    pub const fn as_mcu_byte(self) -> u8 {
        match self {
            Self::Off => 0x00,
            Self::Percent(p) => p,
        }
    }

    /// The duty as a percentage, with [`FanDuty::Off`] reported as `0`.
    #[must_use]
    pub const fn percent(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Percent(p) => p,
        }
    }

    /// Whether this duty actually spins the fan (`ARGON-MCU-L-FANMIN`).
    #[must_use]
    pub const fn is_spinning(self) -> bool {
        match self {
            Self::Off => false,
            Self::Percent(p) => p >= MIN_SPINNING_DUTY,
        }
    }
}

impl fmt::Display for FanDuty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("off"),
            Self::Percent(p) => write!(f, "{p}%"),
        }
    }
}

/// A duty cycle outside the representable range `0..=100`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDuty(pub u8);

impl fmt::Display for InvalidDuty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fan duty {} is out of range (expected 0..=100)", self.0)
    }
}

impl core::error::Error for InvalidDuty {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_off_not_percent_zero() {
        assert_eq!(FanDuty::new(0), Ok(FanDuty::Off));
        assert_eq!(FanDuty::clamped(0), FanDuty::Off);
    }

    #[test]
    fn above_one_hundred_is_rejected_but_clamps() {
        assert_eq!(FanDuty::new(101), Err(InvalidDuty(101)));
        assert_eq!(FanDuty::new(255), Err(InvalidDuty(255)));
        assert_eq!(FanDuty::clamped(255), FanDuty::Percent(100));
    }

    #[test]
    fn sub_spinning_requests_are_raised_not_dropped_to_off() {
        // The failure we are guarding against: a request for a little cooling silently
        // becoming no cooling at all.
        for p in 1..MIN_SPINNING_DUTY {
            let d = FanDuty::clamped(p);
            assert_eq!(
                d,
                FanDuty::Percent(MIN_SPINNING_DUTY),
                "duty {p} collapsed to {d}"
            );
            assert!(d.is_spinning());
        }
    }

    #[test]
    fn no_constructor_yields_percent_zero_or_over_hundred() {
        for p in 0..=255u8 {
            if let Ok(FanDuty::Percent(v)) = FanDuty::new(p) {
                assert!((1..=100).contains(&v), "new({p}) produced Percent({v})");
            }
            if let FanDuty::Percent(v) = FanDuty::clamped(p) {
                assert!((1..=100).contains(&v), "clamped({p}) produced Percent({v})");
            }
        }
    }

    #[test]
    fn mcu_byte_round_trips_within_documented_range() {
        for p in 1..=100u8 {
            assert_eq!(FanDuty::new(p).unwrap().as_mcu_byte(), p);
        }
        assert_eq!(FanDuty::Off.as_mcu_byte(), 0x00);
    }
}
