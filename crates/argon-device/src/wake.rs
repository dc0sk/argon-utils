// SPDX-License-Identifier: GPL-3.0-or-later
//! Scheduled wake: the rules, as pure functions.
//!
//! Setting a wake schedule is observed (`ARGON-UPS-CMD6`, T17). **How** the UPS wakes the Pi is
//! not (`ARGON-UPS-WAKE-MECHANISM`, unknown) -- but a Pi 5 set to power off on halt restarts when
//! its power returns, so the UPS very likely wakes it by cutting and restoring its output. That
//! is assumed until observed, because the consequence of being wrong the other way is an abrupt
//! power cut on a running machine. Every rule here follows from one principle:
//!
//! **A wake schedule must never come due while the machine is running.**
//!
//! Hence: a wake is only ever set together with a poweroff that comes well before it, and a
//! schedule found close to due -- or already past -- on a running machine is parked.

use argon_proto::ups::UpsTime;
use std::time::Duration;

/// Where a schedule is parked: decades away, where it is inert.
///
/// No command to clear a schedule is known (`ARGON-UPS-WAKE-CLEAR`), and guessing one risks the
/// UPS taking leftover bytes as a time. This is the time T17 left the schedule at, already
/// observed to be stored and read back correctly.
pub const PARK_AT: UpsTime = UpsTime {
    year: 2097,
    month: 3,
    day: 21,
    hour: 17,
    minute: 42,
    second: None,
};

/// How close to due a schedule may get on a running machine before it is parked.
pub const PARK_WINDOW: Duration = Duration::from_secs(10 * 60);

/// How often a running daemon looks at the schedule. Well inside [`PARK_WINDOW`], so a schedule
/// cannot slip from "not yet close" to "due" between two looks.
pub const CHECK_EVERY: Duration = Duration::from_secs(5 * 60);

/// The shortest lead a requested wake may have over the moment it is requested.
///
/// Longer than [`PARK_WINDOW`] plus [`POWEROFF_DELAY`], and that is the point: a freshly set wake
/// must stay outside the park window until the machine is off, or the safety net would park the
/// wake it was asked to set, in the minute before the poweroff, and the machine would never come
/// back. If the poweroff is cancelled instead, the wake later drifts into the window on a running
/// machine and is parked, which is exactly right. (An earlier draft had five minutes here, inside
/// the ten-minute window.)
pub const MIN_LEAD: Duration = Duration::from_secs(15 * 60);

/// How long after the request the poweroff that accompanies a wake happens.
///
/// Not immediate: a minute is announced to logged-in users and can be cancelled with
/// `shutdown -c`. If it is cancelled, the wake it came with falls inside [`PARK_WINDOW`] while
/// the machine is still running, and is parked.
pub const POWEROFF_DELAY: Duration = Duration::from_secs(60);

const _: () = assert!(
    CHECK_EVERY.as_secs() < PARK_WINDOW.as_secs(),
    "a schedule could slip from 'not close' to 'due' between two checks"
);
const _: () = assert!(
    MIN_LEAD.as_secs() > PARK_WINDOW.as_secs() + POWEROFF_DELAY.as_secs() + 60,
    "a requested wake would fall inside the park window before the poweroff, and be parked"
);

/// Whether two schedule times name the same minute. A schedule has no seconds field.
#[must_use]
pub const fn same_minute(a: UpsTime, b: UpsTime) -> bool {
    a.year == b.year
        && a.month == b.month
        && a.day == b.day
        && a.hour == b.hour
        && a.minute == b.minute
}

/// The wall message for a poweroff that comes with a wake.
///
/// Deliberately does **not** contain [`crate::power::SHUTDOWN_MESSAGE`]: the battery coordinator
/// adopts, and on mains recovery cancels, any pending poweroff carrying that message. This one is
/// the operator's, not the battery's, and must not be cancelled because mains is present.
#[must_use]
pub fn poweroff_message(wake: UpsTime) -> String {
    format!(
        "Powering off. The UPS will power this machine on again at {:04}-{:02}-{:02} {:02}:{:02} UTC.",
        wake.year, wake.month, wake.day, wake.hour, wake.minute
    )
}

/// What to do about a schedule found on a running machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assessment {
    /// No schedule.
    Nothing,
    /// A schedule far enough away to leave alone, including a parked one.
    Leave {
        /// When it is set for.
        at: UpsTime,
    },
    /// A schedule that must be moved to [`PARK_AT`] now.
    Park {
        /// What was found.
        found: UpsTime,
        /// Why.
        why: ParkReason,
    },
}

/// Why a schedule is parked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkReason {
    /// It would come due within [`PARK_WINDOW`].
    DueSoon,
    /// It is already past. Whether a past schedule can still fire -- or whether this is a wake
    /// that just happened and did not clear itself -- is not known, so it is not left in place.
    Past,
    /// It does not decode to a real date, so when it would fire cannot be known.
    Unreadable,
}

/// Looks at a schedule found on a **running** machine.
#[must_use]
pub fn assess(schedule: Option<UpsTime>, now_unix_s: u64) -> Assessment {
    let Some(found) = schedule else {
        return Assessment::Nothing;
    };
    let Some(at) = found.to_unix_seconds() else {
        return Assessment::Park {
            found,
            why: ParkReason::Unreadable,
        };
    };
    if at <= now_unix_s {
        Assessment::Park {
            found,
            why: ParkReason::Past,
        }
    } else if at - now_unix_s <= PARK_WINDOW.as_secs() {
        Assessment::Park {
            found,
            why: ParkReason::DueSoon,
        }
    } else {
        Assessment::Leave { at: found }
    }
}

/// Why a requested wake time was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    /// Closer than [`MIN_LEAD`], once rounded down to the minute.
    TooSoon {
        /// Seconds of lead it would have had.
        lead_s: u64,
    },
    /// Outside the years the device can hold (2000-2099).
    OutOfRange,
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooSoon { lead_s } => write!(
                f,
                "the wake would be only {lead_s} s away; it must be at least {} min out, so the \
                 machine is certainly off first",
                MIN_LEAD.as_secs() / 60
            ),
            Self::OutOfRange => f.write_str("the UPS can only hold years 2000 to 2099"),
        }
    }
}

/// Turns a requested wake instant into the schedule the UPS will hold.
///
/// Rounded **down** to the minute, because the schedule has minute resolution -- and the lead
/// is checked after rounding, since rounding down is what could bring it under the minimum.
///
/// # Errors
///
/// Too soon, or out of the device's range.
pub fn schedule_for(at_unix_s: u64, now_unix_s: u64) -> Result<UpsTime, RequestError> {
    let floored = at_unix_s - at_unix_s % 60;
    let lead_s = floored.saturating_sub(now_unix_s);
    if floored <= now_unix_s || lead_s < MIN_LEAD.as_secs() {
        return Err(RequestError::TooSoon { lead_s });
    }
    let mut t = UpsTime::from_unix_seconds(floored).ok_or(RequestError::OutOfRange)?;
    t.second = None;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_789_728_000; // a whole minute

    fn at(unix: u64) -> UpsTime {
        let mut t = UpsTime::from_unix_seconds(unix).unwrap();
        t.second = None;
        t
    }

    #[test]
    fn the_parked_time_is_left_alone() {
        assert_eq!(
            assess(Some(PARK_AT), NOW),
            Assessment::Leave { at: PARK_AT }
        );
    }

    #[test]
    fn a_schedule_close_to_due_is_parked() {
        let soon = at(NOW + 9 * 60);
        assert_eq!(
            assess(Some(soon), NOW),
            Assessment::Park {
                found: soon,
                why: ParkReason::DueSoon
            }
        );
        // Exactly at the window's edge counts as close.
        let edge = at(NOW + PARK_WINDOW.as_secs());
        assert!(matches!(assess(Some(edge), NOW), Assessment::Park { .. }));
    }

    #[test]
    fn a_schedule_further_out_is_left() {
        let later = at(NOW + 11 * 60);
        assert_eq!(assess(Some(later), NOW), Assessment::Leave { at: later });
    }

    #[test]
    fn a_past_schedule_is_parked() {
        // What a wake that just happened, and did not clear itself, would look like at boot.
        let past = at(NOW - 3 * 60);
        assert_eq!(
            assess(Some(past), NOW),
            Assessment::Park {
                found: past,
                why: ParkReason::Past
            }
        );
    }

    #[test]
    fn an_impossible_schedule_is_parked_not_trusted() {
        let unreadable = UpsTime {
            year: 2026,
            month: 13,
            day: 1,
            hour: 7,
            minute: 0,
            second: None,
        };
        assert_eq!(
            assess(Some(unreadable), NOW),
            Assessment::Park {
                found: unreadable,
                why: ParkReason::Unreadable
            }
        );
    }

    #[test]
    fn nothing_scheduled_is_nothing_to_do() {
        assert_eq!(assess(None, NOW), Assessment::Nothing);
    }

    #[test]
    fn a_request_is_rounded_down_and_needs_its_lead() {
        // 07:00:59 is held as 07:00.
        let t = schedule_for(NOW + 3_600 + 59, NOW).unwrap();
        assert_eq!(t, at(NOW + 3_600));
        assert_eq!(t.second, None);

        // Exactly the minimum lead is accepted...
        assert!(schedule_for(NOW + MIN_LEAD.as_secs(), NOW).is_ok());
        // ...but not when rounding down takes it under.
        assert!(matches!(
            schedule_for(NOW + MIN_LEAD.as_secs() + 30, NOW + 31),
            Err(RequestError::TooSoon { .. })
        ));
        assert!(matches!(
            schedule_for(NOW - 60, NOW),
            Err(RequestError::TooSoon { .. })
        ));
    }

    #[test]
    fn the_wake_poweroff_is_not_mistaken_for_a_battery_poweroff() {
        // If it carried the battery message, the coordinator would adopt it after a restart and
        // cancel it as soon as it saw mains -- which is always, for a wake requested on mains.
        let msg = poweroff_message(PARK_AT);
        assert!(!msg.contains(crate::power::SHUTDOWN_MESSAGE));
        assert!(msg.contains("2097-03-21 17:42 UTC"), "{msg}");
    }

    #[test]
    fn years_the_device_cannot_hold_are_refused() {
        assert_eq!(
            schedule_for(4_102_444_800, NOW),
            Err(RequestError::OutOfRange)
        );
    }
}
