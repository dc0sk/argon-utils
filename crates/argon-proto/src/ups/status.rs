// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! Decoding the UPS's responses.
//!
//! Facts: `ARGON-UPS-CMD0`, `ARGON-UPS-CMD2`, `ARGON-UPS-CMD4`, `ARGON-UPS-CMD5`,
//! `ARGON-UPS-CMD7` — all `inferred`. These decoders exist so the shape can be tested and
//! fuzzed; promoting the facts to `observed` needs a capture from real hardware, which is
//! task T4 in `docs/testing/HUMAN-TASKS.md`.

use crate::bcd;
use core::fmt;

/// Whether the UPS is running from mains or from its battery.
///
/// The wire encoding is a single byte where **zero means on mains**. That reads backwards,
/// so it is given a name here rather than left as a bare integer comparison at each use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerSource {
    /// Mains present; the battery is charging or charged.
    Mains,
    /// Running from the battery.
    Battery,
}

/// The response to [`Command::BatteryStatus`](super::Command::BatteryStatus).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatteryStatus {
    /// Charge percentage, clamped to `0..=100`.
    pub percent: u8,
    /// Where power is coming from.
    pub source: PowerSource,
}

impl BatteryStatus {
    /// Decodes a two-byte battery-status payload.
    pub fn decode(payload: &[u8]) -> Result<Self, DecodeError> {
        let [raw_percent, charging] = *payload else {
            return Err(DecodeError::Length {
                want: 2,
                got: payload.len(),
            });
        };
        Ok(Self {
            // The device has been seen to report values above 100. Clamp rather than reject:
            // an implausible percentage is still a usable signal, and refusing the whole
            // frame would lose the power source with it.
            percent: raw_percent.min(100),
            source: if charging == 0 {
                PowerSource::Mains
            } else {
                PowerSource::Battery
            },
        })
    }
}

impl fmt::Display for BatteryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let src = match self.source {
            PowerSource::Mains => "on mains",
            PowerSource::Battery => "on battery",
        };
        write!(f, "{}%, {src}", self.percent)
    }
}

/// A wall-clock time as the UPS represents it: BCD, UTC, with a year offset from 2000.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpsTime {
    /// Full year, e.g. 2026.
    pub year: u16,
    /// Month, 1..=12.
    pub month: u8,
    /// Day of month, 1..=31.
    pub day: u8,
    /// Hour, 0..=23.
    pub hour: u8,
    /// Minute, 0..=59.
    pub minute: u8,
    /// Second, 0..=59. Absent from a wake schedule, which has minute resolution.
    pub second: Option<u8>,
}

impl UpsTime {
    /// Decodes a six-byte RTC payload (`YY MM DD HH MM SS`).
    pub fn decode_clock(payload: &[u8]) -> Result<Self, DecodeError> {
        let [y, mo, d, h, mi, s] = *payload else {
            return Err(DecodeError::Length {
                want: 6,
                got: payload.len(),
            });
        };
        Ok(Self {
            year: 2000 + u16::from(bcd::decode(y)?),
            month: bcd::decode(mo)?,
            day: bcd::decode(d)?,
            hour: bcd::decode(h)?,
            minute: bcd::decode(mi)?,
            second: Some(bcd::decode(s)?),
        })
    }

    /// Decodes a five-byte wake-schedule payload (`YY MM DD HH MM`).
    pub fn decode_schedule(payload: &[u8]) -> Result<Self, DecodeError> {
        let [y, mo, d, h, mi] = *payload else {
            return Err(DecodeError::Length {
                want: 5,
                got: payload.len(),
            });
        };
        Ok(Self {
            year: 2000 + u16::from(bcd::decode(y)?),
            month: bcd::decode(mo)?,
            day: bcd::decode(d)?,
            hour: bcd::decode(h)?,
            minute: bcd::decode(mi)?,
            second: None,
        })
    }

    /// Decodes a wake-schedule response, where no schedule set is a valid answer.
    ///
    /// `ARGON-UPS-CMD7-EMPTY`, `observed`: with nothing scheduled the device replies
    /// `FE 00 07 05` — a well-formed frame carrying no payload at all. An earlier
    /// implementation guessed five zero bytes, which would have been reported to the user as
    /// a decode failure on a perfectly healthy device.
    pub fn decode_optional_schedule(payload: &[u8]) -> Result<Option<Self>, DecodeError> {
        if payload.is_empty() {
            return Ok(None);
        }
        Self::decode_schedule(payload).map(Some)
    }

    /// Encodes as a six-byte RTC payload.
    pub fn encode_clock(&self) -> Result<[u8; 6], DecodeError> {
        Ok([
            bcd::encode(self.year_byte()?)?,
            bcd::encode(self.month)?,
            bcd::encode(self.day)?,
            bcd::encode(self.hour)?,
            bcd::encode(self.minute)?,
            bcd::encode(self.second.unwrap_or(0))?,
        ])
    }

    /// Encodes as a five-byte wake-schedule payload.
    pub fn encode_schedule(&self) -> Result<[u8; 5], DecodeError> {
        Ok([
            bcd::encode(self.year_byte()?)?,
            bcd::encode(self.month)?,
            bcd::encode(self.day)?,
            bcd::encode(self.hour)?,
            bcd::encode(self.minute)?,
        ])
    }

    /// Converts seconds since the unix epoch (UTC) to a clock time.
    ///
    /// `None` outside the years the device can represent (2000-2099: a two-digit BCD year
    /// offset from 2000). Civil-from-days after Howard Hinnant's algorithm, which is exact for
    /// the proleptic Gregorian calendar; no date library, because this crate is `no_std` and
    /// needs one conversion.
    #[must_use]
    pub const fn from_unix_seconds(secs: u64) -> Option<Self> {
        let days = secs / 86_400;
        let rem = secs % 86_400;
        // Shift the epoch to 0000-03-01 so leap days fall at the end of a year.
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
        if year < 2000 || year > 2099 {
            return None;
        }
        // Every value below is range-checked by construction, so the casts cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        Some(Self {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: (rem / 3_600) as u8,
            minute: (rem % 3_600 / 60) as u8,
            second: Some((rem % 60) as u8),
        })
    }

    /// Converts to seconds since the unix epoch (UTC), the inverse of
    /// [`from_unix_seconds`](Self::from_unix_seconds).
    ///
    /// `None` if the time is not [plausible](Self::is_plausible). A missing seconds field (a
    /// wake schedule) counts as zero.
    #[must_use]
    pub const fn to_unix_seconds(&self) -> Option<u64> {
        if !self.is_plausible() {
            return None;
        }
        let y = self.year as u64 - if self.month <= 2 { 1 } else { 0 };
        let era = y / 400;
        let yoe = y - era * 400;
        let m = self.month as u64;
        let mp = if m > 2 { m - 3 } else { m + 9 };
        let doy = (153 * mp + 2) / 5 + self.day as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let second = match self.second {
            Some(s) => s as u64,
            None => 0,
        };
        Some(days * 86_400 + self.hour as u64 * 3_600 + self.minute as u64 * 60 + second)
    }

    /// Whether every field is within its calendar range.
    ///
    /// Valid BCD is not the same as a valid date: `0x99` decodes cleanly to 99 and is not a
    /// month. Callers acting on a schedule should check this.
    #[must_use]
    pub const fn is_plausible(&self) -> bool {
        self.year >= 2000
            && self.year <= 2099
            && self.month >= 1
            && self.month <= 12
            && self.day >= 1
            && self.day <= 31
            && self.hour <= 23
            && self.minute <= 59
            && match self.second {
                Some(s) => s <= 59,
                None => true,
            }
    }

    fn year_byte(self) -> Result<u8, DecodeError> {
        // The wire carries the year as two BCD digits offset from 2000, so the expressible
        // range is exactly 2000..=2099. Bound it here rather than letting 2100 fall through
        // to the BCD encoder, which would report "100 cannot be encoded as two BCD digits"
        // and leave the caller to work out that it meant the year.
        if self.year < 2000 || self.year > 2099 {
            return Err(DecodeError::YearOutOfRange(self.year));
        }
        u8::try_from(self.year - 2000).map_err(|_| DecodeError::YearOutOfRange(self.year))
    }
}

impl fmt::Display for UpsTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute
        )?;
        if let Some(s) = self.second {
            write!(f, ":{s:02}")?;
        }
        f.write_str(" UTC")
    }
}

/// Why a response payload could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The payload was not the expected length.
    Length {
        /// Bytes expected.
        want: usize,
        /// Bytes received.
        got: usize,
    },
    /// A field that should have been BCD was not.
    Bcd(bcd::InvalidBcd),
    /// A value could not be encoded as two BCD digits.
    Range(bcd::OutOfRange),
    /// A year outside the 2000..=2099 the wire format can express.
    YearOutOfRange(u16),
}

impl From<bcd::InvalidBcd> for DecodeError {
    fn from(e: bcd::InvalidBcd) -> Self {
        Self::Bcd(e)
    }
}

impl From<bcd::OutOfRange> for DecodeError {
    fn from(e: bcd::OutOfRange) -> Self {
        Self::Range(e)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { want, got } => {
                write!(f, "expected a {want}-byte payload, got {got}")
            }
            Self::Bcd(e) => write!(f, "{e}"),
            Self::Range(e) => write!(f, "{e}"),
            Self::YearOutOfRange(y) => write!(f, "year {y} is outside 2000..=2099"),
        }
    }
}

impl core::error::Error for DecodeError {}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn zero_means_mains_which_reads_backwards() {
        assert_eq!(
            BatteryStatus::decode(&[93, 0]).unwrap().source,
            PowerSource::Mains
        );
        assert_eq!(
            BatteryStatus::decode(&[93, 1]).unwrap().source,
            PowerSource::Battery
        );
    }

    #[test]
    fn implausible_percentages_clamp_rather_than_lose_the_frame() {
        assert_eq!(BatteryStatus::decode(&[255, 0]).unwrap().percent, 100);
        assert_eq!(BatteryStatus::decode(&[0, 1]).unwrap().percent, 0);
    }

    #[test]
    fn wrong_length_is_rejected() {
        assert!(matches!(
            BatteryStatus::decode(&[93]),
            Err(DecodeError::Length { want: 2, got: 1 })
        ));
        assert!(matches!(
            UpsTime::decode_clock(&[0x26, 0x09, 0x15, 0x14, 0x30]),
            Err(DecodeError::Length { want: 6, got: 5 })
        ));
    }

    #[test]
    fn clock_round_trips() {
        let t = UpsTime {
            year: 2026,
            month: 9,
            day: 15,
            hour: 14,
            minute: 30,
            second: Some(45),
        };
        assert_eq!(UpsTime::decode_clock(&t.encode_clock().unwrap()), Ok(t));
    }

    #[test]
    fn schedule_round_trips_without_seconds() {
        let t = UpsTime {
            year: 2027,
            month: 1,
            day: 2,
            hour: 3,
            minute: 4,
            second: None,
        };
        assert_eq!(
            UpsTime::decode_schedule(&t.encode_schedule().unwrap()),
            Ok(t)
        );
    }

    #[test]
    fn corrupt_bcd_is_rejected_not_reinterpreted() {
        // 0xFF is not BCD. Upstream's arithmetic would turn it into the decimal 165 and
        // carry on; a desynchronised serial link produces exactly this kind of byte.
        assert!(matches!(
            UpsTime::decode_clock(&[0xFF, 0x09, 0x15, 0x14, 0x30, 0x00]),
            Err(DecodeError::Bcd(_))
        ));
    }

    #[test]
    fn valid_bcd_is_not_the_same_as_a_valid_date() {
        // Every byte here is legal BCD, and none of it is a real date.
        let t = UpsTime::decode_clock(&[0x99, 0x99, 0x99, 0x99, 0x99, 0x99]).unwrap();
        assert!(
            !t.is_plausible(),
            "99-99-99 should not pass a plausibility check"
        );

        let good = UpsTime::decode_clock(&[0x26, 0x09, 0x15, 0x14, 0x30, 0x00]).unwrap();
        assert!(good.is_plausible());
        assert_eq!(good.year, 2026);
    }

    #[test]
    fn years_outside_the_wire_format_are_refused() {
        let t = UpsTime {
            year: 2100,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: None,
        };
        assert!(matches!(
            t.encode_schedule(),
            Err(DecodeError::YearOutOfRange(2100))
        ));
        let t = UpsTime {
            year: 1999,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: None,
        };
        assert!(matches!(
            t.encode_schedule(),
            Err(DecodeError::YearOutOfRange(1999))
        ));
    }
}
