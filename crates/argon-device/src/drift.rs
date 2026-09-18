// SPDX-License-Identifier: GPL-3.0-or-later
//! A record of the UPS clock's drift that survives reboots.
//!
//! argond logs each clock check to the journal, but Raspberry Pi OS keeps the journal only until
//! reboot, so on its own that record ends every time the machine restarts. This keeps one line per
//! check in argond's state directory instead, and derives a drift rate from it.
//!
//! # Format
//!
//! One check per line: `UNIX OFFSET ACTION [AFTER]`, where `OFFSET` is UPS minus system in seconds
//! and `AFTER` is the offset read back after a correction. Lines starting with `#` are comments.
//! Anything unparseable is skipped, so a damaged line costs that line, not the record.

use std::fmt;
use std::io::Write;
use std::path::Path;

/// When the record exceeds this many lines, it is cut back to [`KEEP_LINES`].
pub const MAX_LINES: usize = 1_000;

/// How many of the newest lines survive a cut. At four checks a day, months of history.
pub const KEEP_LINES: usize = 500;

/// The shortest uncorrected stretch a rate is reported for.
///
/// The clock is read in whole seconds, so over a few hours a one-second step is most of the
/// signal. A day keeps the resolution error to about a second per day, which is the scale the
/// question is asked at.
pub const MIN_SPAN_S: u64 = 86_400;

/// What a check did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Within tolerance; nothing done.
    InSync,
    /// Set from the system clock; the offset read back afterwards.
    Corrected {
        /// Offset after the correction.
        after_s: i64,
    },
    /// Off by more than tolerance but not corrected, because the system clock was unsynced or
    /// clock writes are off.
    LeftAlone,
}

/// One check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// When, seconds since the unix epoch.
    pub unix: u64,
    /// UPS minus system, seconds, before any correction.
    pub offset_s: i64,
    /// What was done.
    pub action: Action,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:+}", self.unix, self.offset_s)?;
        match self.action {
            Action::InSync => write!(f, " in-sync"),
            Action::Corrected { after_s } => write!(f, " corrected {after_s:+}"),
            Action::LeftAlone => write!(f, " left-alone"),
        }
    }
}

impl Entry {
    /// Parses one line; `None` for comments and anything malformed.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return None;
        }
        let mut parts = line.split_whitespace();
        let unix = parts.next()?.parse().ok()?;
        let offset_s = parts.next()?.parse().ok()?;
        let action = match parts.next()? {
            "in-sync" => Action::InSync,
            "left-alone" => Action::LeftAlone,
            "corrected" => Action::Corrected {
                after_s: parts.next()?.parse().ok()?,
            },
            _ => return None,
        };
        Some(Self {
            unix,
            offset_s,
            action,
        })
    }
}

/// Reads every parseable entry, oldest first.
#[must_use]
pub fn read(path: &Path) -> Vec<Entry> {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter_map(Entry::parse).collect())
        .unwrap_or_default()
}

/// Appends one entry, cutting the record back to its newest [`KEEP_LINES`] once it passes
/// [`MAX_LINES`].
///
/// # Errors
///
/// Fails on I/O error.
pub fn append(path: &Path, entry: Entry) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "{entry}")?;
    drop(f);

    let text = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() > MAX_LINES {
        let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, kept)?;
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

/// A drift rate, from one uncorrected stretch of the record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    /// Seconds gained (positive) or lost (negative) per day.
    pub s_per_day: f64,
    /// How long the stretch it was measured over is, in seconds.
    pub span_s: u64,
}

impl Rate {
    /// The stretch's length in days.
    #[must_use]
    pub fn span_days(&self) -> f64 {
        // Exact for any span under 2^52 seconds -- over a hundred million years.
        #[allow(clippy::cast_precision_loss)]
        let s = self.span_s as f64;
        s / 86_400.0
    }
}

/// The drift rate over the most recent stretch with no correction in it, if that stretch spans
/// at least [`MIN_SPAN_S`].
///
/// A stretch starts at a correction -- from the offset read back after it -- or at the first
/// entry, and ends at the entry before the next correction. Within it, the UPS clock is running
/// free, so the change in offset over the time elapsed is its drift.
#[must_use]
pub fn rate(entries: &[Entry]) -> Option<Rate> {
    // (unix, offset) points of the most recent free-running stretch.
    let mut stretch: Vec<(u64, i64)> = Vec::new();
    for e in entries {
        match e.action {
            Action::Corrected { after_s } => {
                stretch.clear();
                stretch.push((e.unix, after_s));
            }
            Action::InSync | Action::LeftAlone => stretch.push((e.unix, e.offset_s)),
        }
    }
    let (&(t0, o0), &(t1, o1)) = (stretch.first()?, stretch.last()?);
    let span_s = t1.checked_sub(t0)?;
    if span_s < MIN_SPAN_S {
        return None;
    }
    // Both conversions are exact for any realistic value: offsets are seconds, spans are at most
    // decades of seconds.
    #[allow(clippy::cast_precision_loss)]
    let s_per_day = (o1 - o0) as f64 * 86_400.0 / span_s as f64;
    Some(Rate { s_per_day, span_s })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn e(unix: u64, offset_s: i64, action: Action) -> Entry {
        Entry {
            unix,
            offset_s,
            action,
        }
    }

    #[test]
    fn entries_round_trip_through_their_line_form() {
        for entry in [
            e(1_789_740_000, -21, Action::Corrected { after_s: 0 }),
            e(1_789_761_600, 1, Action::InSync),
            e(1_789_783_200, -5, Action::LeftAlone),
        ] {
            assert_eq!(Entry::parse(&entry.to_string()), Some(entry));
        }
        assert_eq!(e(1, 1, Action::InSync).to_string(), "1 +1 in-sync");
    }

    #[test]
    fn comments_and_damage_are_skipped_not_fatal() {
        for bad in [
            "# header",
            "",
            "garbage",
            "1 x in-sync",
            "1 +1 teleported",
            "1 +1 corrected",
        ] {
            assert_eq!(Entry::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_rate_is_the_offset_change_over_an_uncorrected_stretch() {
        // Corrected to 0, then 3 s slow two days later: -1.5 s/day.
        let r = rate(&[
            e(0, -21, Action::Corrected { after_s: 0 }),
            e(DAY, -1, Action::InSync),
            e(2 * DAY, -3, Action::LeftAlone),
        ])
        .unwrap();
        assert!((r.s_per_day - -1.5).abs() < 1e-9, "{r:?}");
        assert_eq!(r.span_s, 2 * DAY);
    }

    #[test]
    fn a_correction_starts_a_new_stretch() {
        // The first stretch drifts fast; after the correction the clock barely moves. Only the
        // most recent stretch counts, measured from the offset read back after correcting.
        let r = rate(&[
            e(0, 0, Action::InSync),
            e(2 * DAY, -10, Action::Corrected { after_s: 1 }),
            e(4 * DAY, 1, Action::InSync),
        ])
        .unwrap();
        assert!(r.s_per_day.abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn under_a_day_is_not_a_rate() {
        // The T15 and T17 data points: +0 and +1 s a couple of hours apart. At whole-second
        // resolution that is noise, not a rate.
        assert_eq!(
            rate(&[
                e(0, 0, Action::Corrected { after_s: 0 }),
                e(9_000, 1, Action::InSync),
            ]),
            None
        );
        assert_eq!(rate(&[]), None);
    }

    #[test]
    fn the_record_is_cut_back_once_it_grows_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("argon-drift-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("clock.log");
        let _ = std::fs::remove_file(&path);
        for i in 0..=MAX_LINES as u64 {
            append(&path, e(i, 0, Action::InSync)).unwrap();
        }
        let kept = read(&path);
        assert_eq!(kept.len(), KEEP_LINES);
        assert_eq!(
            kept.last().unwrap().unix,
            MAX_LINES as u64,
            "lost the newest entry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
