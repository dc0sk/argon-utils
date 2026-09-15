// SPDX-License-Identifier: GPL-3.0-or-later
//! Binary-coded decimal, as used by the UPS RTC commands and the PCF8563.
//!
//! Facts: `ARGON-UPS-CMD3`, `ARGON-UPS-CMD5`, `ARGON-UPS-CMD6`, `ARGON-UPS-CMD7`
//! (`inferred`); `ARGON-RTC-EON-REGS` (`documented`).

use core::fmt;

/// A byte that is not valid BCD, i.e. either nibble is in `0xA..=0xF`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBcd(pub u8);

impl fmt::Display for InvalidBcd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:02x} is not valid BCD", self.0)
    }
}

impl core::error::Error for InvalidBcd {}

/// Decodes a BCD byte to its decimal value.
///
/// Rejects bytes whose nibbles are not both in `0..=9`. Upstream's equivalent silently
/// computes `(v & 0xF) + 10 * (v >> 4)`, so a corrupt `0xFF` on the wire becomes the
/// decimal value 165 and flows onward as a plausible-looking number. A desynchronised
/// serial link produces exactly that kind of garbage, so this rejects instead.
pub const fn decode(byte: u8) -> Result<u8, InvalidBcd> {
    let lo = byte & 0x0F;
    let hi = byte >> 4;
    if lo > 9 || hi > 9 {
        return Err(InvalidBcd(byte));
    }
    Ok(hi * 10 + lo)
}

/// Encodes a decimal value in `0..=99` as BCD.
pub const fn encode(value: u8) -> Result<u8, OutOfRange> {
    if value > 99 {
        return Err(OutOfRange(value));
    }
    Ok(((value / 10) << 4) | (value % 10))
}

/// A decimal value too large to represent in two BCD nibbles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutOfRange(pub u8);

impl fmt::Display for OutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} cannot be encoded as two BCD digits (expected 0..=99)",
            self.0
        )
    }
}

impl core::error::Error for OutOfRange {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_representable_value() {
        for v in 0..=99u8 {
            assert_eq!(decode(encode(v).unwrap()), Ok(v), "failed at {v}");
        }
    }

    #[test]
    fn rejects_every_byte_with_a_hex_nibble() {
        let mut rejected = 0;
        for b in 0..=255u8 {
            let valid = (b & 0x0F) <= 9 && (b >> 4) <= 9;
            match decode(b) {
                Ok(_) => assert!(valid, "0x{b:02x} accepted but has a hex nibble"),
                Err(InvalidBcd(x)) => {
                    assert_eq!(x, b);
                    assert!(!valid, "0x{b:02x} rejected but is valid BCD");
                    rejected += 1;
                }
            }
        }
        // 256 total, 100 valid.
        assert_eq!(rejected, 156);
    }

    #[test]
    fn encode_rejects_out_of_range() {
        assert_eq!(encode(100), Err(OutOfRange(100)));
        assert_eq!(encode(255), Err(OutOfRange(255)));
    }

    #[test]
    fn known_values() {
        assert_eq!(decode(0x26), Ok(26));
        assert_eq!(encode(26), Ok(0x26));
        assert_eq!(decode(0x00), Ok(0));
        assert_eq!(decode(0x99), Ok(99));
    }
}
