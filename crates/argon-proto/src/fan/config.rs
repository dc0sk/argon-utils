// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! Parsing the vendor's fan curve file format, for migration.
//!
//! The format is one `<temperature>=<duty>` pair per line, with `#` comments. It is read
//! here only so an existing configuration can be imported; `argon-utils` does not write it.
//!
//! Naming the vendor's file format is interoperability, not derivation — the same reason a
//! program may read a competitor's file. No vendor code was consulted for this parser; the
//! format is inferred from the file on disk, which is the user's own configuration data.

use super::{CurvePoint, FanCurve, FanDuty};
use alloc::vec::Vec;
use core::fmt;

/// A problem with a line of a vendor fan config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    /// What was wrong.
    pub kind: ParseErrorKind,
}

/// What was wrong with a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// The line had no `=`, or more than one.
    NotAPair,
    /// The temperature was not a number.
    BadTemperature,
    /// The duty was not a number, or was above 100.
    BadDuty,
    /// The resulting curve was not valid.
    Curve(super::CurveError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: ", self.line)?;
        match &self.kind {
            ParseErrorKind::NotAPair => f.write_str("expected `<temperature>=<duty>`"),
            ParseErrorKind::BadTemperature => f.write_str("temperature is not a number"),
            ParseErrorKind::BadDuty => f.write_str("duty is not a number in 0..=100"),
            ParseErrorKind::Curve(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for ParseError {}

/// Parses a vendor fan config into a curve.
///
/// Blank lines and `#` comments are skipped. A malformed line is an **error**, not a line to
/// skip: upstream silently drops any line that fails validation, so a typo in a threshold
/// quietly removes a step from the curve and the fan runs slower than the file says.
///
/// # Errors
///
/// Returns the first malformed line, or the reason the resulting curve was rejected.
pub fn parse(text: &str) -> Result<FanCurve, ParseError> {
    let mut points = Vec::new();

    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        let content = raw.split('#').next().unwrap_or("").trim();
        if content.is_empty() {
            continue;
        }

        let mut parts = content.splitn(2, '=');
        let (Some(lhs), Some(rhs)) = (parts.next(), parts.next()) else {
            return Err(ParseError {
                line,
                kind: ParseErrorKind::NotAPair,
            });
        };
        let (lhs, rhs) = (lhs.trim(), rhs.trim());
        if lhs.is_empty() || rhs.is_empty() || rhs.contains('=') {
            return Err(ParseError {
                line,
                kind: ParseErrorKind::NotAPair,
            });
        }

        let decicelsius = parse_decicelsius(lhs).ok_or(ParseError {
            line,
            kind: ParseErrorKind::BadTemperature,
        })?;
        let duty = rhs
            .parse::<u8>()
            .ok()
            .and_then(|d| FanDuty::new(d).ok())
            .ok_or(ParseError {
                line,
                kind: ParseErrorKind::BadDuty,
            })?;

        points.push(CurvePoint { decicelsius, duty });
    }

    FanCurve::new(points).map_err(|e| ParseError {
        line: 0,
        kind: ParseErrorKind::Curve(e),
    })
}

/// Parses a temperature into tenths of a degree, accepting one optional decimal place.
fn parse_decicelsius(s: &str) -> Option<i32> {
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };

    let (whole, frac) = match digits.split_once('.') {
        Some((w, f)) => (w, f),
        None => (digits, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }

    let whole: i32 = whole.parse().ok()?;
    // Only the first decimal place is representable; more would round silently, so refuse.
    let tenths: i32 = match frac.len() {
        0 => 0,
        1 => i32::from(frac.as_bytes()[0] - b'0'),
        _ => return None,
    };

    let value = whole.checked_mul(10)?.checked_add(tenths)?;
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;

    #[test]
    fn parses_the_stock_config() {
        // Exactly the file on the development machine.
        let curve =
            parse("#\n# Argon Fan Speed Configuration (CPU)\n#\n55=30\n60=55\n65=100\n").unwrap();
        assert_eq!(curve.points().len(), 3);
        assert_eq!(curve.duty_for(600), FanDuty::Percent(55));
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let curve = parse("\n  \n# comment\n55=30   # trailing comment\n").unwrap();
        assert_eq!(curve.points().len(), 1);
        assert_eq!(curve.duty_for(600), FanDuty::Percent(30));
    }

    #[test]
    fn a_malformed_line_is_an_error_not_a_skipped_line() {
        // Upstream drops any line that fails validation, so a typo quietly removes a step
        // from the curve and the fan then runs slower than the file says.
        let err = parse("55=30\n6O=55\n65=100\n").unwrap_err();
        assert_eq!(err.line, 2);
        assert_eq!(err.kind, ParseErrorKind::BadTemperature);
    }

    #[test]
    fn out_of_range_duty_is_rejected() {
        assert_eq!(parse("55=101\n").unwrap_err().kind, ParseErrorKind::BadDuty);
        assert_eq!(parse("55=300\n").unwrap_err().kind, ParseErrorKind::BadDuty);
        assert_eq!(parse("55=-5\n").unwrap_err().kind, ParseErrorKind::BadDuty);
    }

    #[test]
    fn lines_without_a_pair_are_rejected() {
        assert_eq!(parse("55\n").unwrap_err().kind, ParseErrorKind::NotAPair);
        assert_eq!(parse("=30\n").unwrap_err().kind, ParseErrorKind::NotAPair);
        assert_eq!(parse("55=\n").unwrap_err().kind, ParseErrorKind::NotAPair);
        assert_eq!(
            parse("55=30=40\n").unwrap_err().kind,
            ParseErrorKind::NotAPair
        );
    }

    #[test]
    fn a_curve_that_cools_less_as_it_heats_is_rejected() {
        let err = parse("55=80\n65=40\n").unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::Curve(_)));
    }

    #[test]
    fn decimal_temperatures_are_supported_to_one_place() {
        let curve = parse("55.5=30\n").unwrap();
        assert_eq!(curve.points()[0].decicelsius, 555);
        assert_eq!(curve.duty_for(554), FanDuty::Off);
        assert_eq!(curve.duty_for(555), FanDuty::Percent(30));
        // More precision than the format can hold is refused rather than rounded silently.
        assert_eq!(
            parse("55.55=30\n").unwrap_err().kind,
            ParseErrorKind::BadTemperature
        );
    }

    #[test]
    fn an_empty_config_is_rejected() {
        assert!(matches!(
            parse("").unwrap_err().kind,
            ParseErrorKind::Curve(_)
        ));
        assert!(matches!(
            parse("# only comments\n").unwrap_err().kind,
            ParseErrorKind::Curve(_)
        ));
    }

    #[test]
    fn parsing_arbitrary_text_never_panics() {
        for seed in 0u32..400 {
            let mut s = std::string::String::new();
            for i in 0..24u32 {
                let b = ((i.wrapping_mul(seed).wrapping_add(7)) % 128) as u8;
                s.push(b as char);
            }
            let _ = parse(&s);
        }
    }

    #[test]
    fn ordering_in_the_file_does_not_matter() {
        let a = parse("65=100\n55=30\n60=55\n").unwrap();
        let b = parse("55=30\n60=55\n65=100\n").unwrap();
        assert_eq!(a, b);
        let _ = vec![a, b];
    }
}
