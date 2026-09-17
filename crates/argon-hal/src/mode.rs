// SPDX-License-Identifier: GPL-3.0-or-later
//! Operating modes.
//!
//! The mode is chosen once, at startup, and decides which transport decorators wrap the real
//! device. It is deliberately not a flag consulted at each call site: a check that every
//! future code path must remember to perform is a check that some future code path will
//! forget.

use core::fmt;

/// How much this process is permitted to do to the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Nothing that changes device state. **The shipped default.**
    ///
    /// Side-effect-free queries are allowed: reading the UPS means sending it request frames,
    /// so "no byte written" would make monitoring impossible. The boundary is enforced by the
    /// transport, not by convention -- I2C writes are refused outright, and the UPS link only
    /// passes the query commands confirmed on hardware.
    ///
    /// A fresh install cannot change your fan or your UPS.
    #[default]
    ReadOnly,
    /// Fan and display writes, and button-driven reboot or shutdown.
    Managed,
    /// Additionally: arming the MCU power cut, UPS threshold and RTC writes, EEPROM changes.
    ///
    /// Every individual destructive operation is gated again on top of this — `Full` makes
    /// them reachable, not automatic.
    Full,
}

impl Mode {
    /// Whether ordinary device writes are permitted.
    #[must_use]
    pub const fn allows_writes(self) -> bool {
        matches!(self, Self::Managed | Self::Full)
    }

    /// Whether destructive operations are reachable.
    #[must_use]
    pub const fn allows_destructive(self) -> bool {
        matches!(self, Self::Full)
    }

    /// Parses a mode name from configuration.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "read-only" | "readonly" => Some(Self::ReadOnly),
            "managed" => Some(Self::Managed),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// The name used in configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Managed => "managed",
            Self::Full => "full",
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_safe_one() {
        assert_eq!(Mode::default(), Mode::ReadOnly);
        assert!(!Mode::default().allows_writes());
        assert!(!Mode::default().allows_destructive());
    }

    #[test]
    fn managed_does_not_imply_destructive() {
        assert!(Mode::Managed.allows_writes());
        assert!(
            !Mode::Managed.allows_destructive(),
            "managed must not reach destructive ops"
        );
    }

    #[test]
    fn names_round_trip() {
        for m in [Mode::ReadOnly, Mode::Managed, Mode::Full] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
    }

    #[test]
    fn an_unknown_mode_is_rejected_rather_than_defaulted() {
        // Silently falling back would turn a typo into a mode the operator did not choose.
        // Which direction that typo falls is exactly the question you do not want decided
        // by a default.
        assert_eq!(Mode::parse("manged"), None);
        assert_eq!(Mode::parse(""), None);
        assert_eq!(Mode::parse("yes"), None);
    }
}
