// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeping the UPS clock right.
//!
//! The UPS clock runs off the UPS battery, so a deep discharge can reset it, and it drifts in
//! between: it was found 21-22 s slow on 2026-09-18, the day after the vendor daemon that had
//! presumably been keeping it set was retired. argond therefore compares it with the system
//! clock periodically and sets it when it has wandered.
//!
//! The decision is a pure function so the rules can be tested; the reading, the waiting for a
//! second boundary and the writing are the daemon's.

use argon_proto::ups::UpsTime;
use std::time::Duration;

/// How often the clock is compared.
///
/// Drift is seconds per day at worst, so six-hourly keeps it well inside the threshold while
/// adding four queries a day to a link that is polled every few seconds anyway.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3_600);

/// How soon to look again when the system clock is not synchronised yet.
///
/// The first check runs seconds into the boot, before NTP has had its say, and an unsynchronised
/// system clock must not be copied into the UPS. Waiting a whole [`CHECK_INTERVAL`] after that
/// leaves the UPS uncorrected for hours on a machine that was only briefly unsure of the time
/// (seen on the boot after T18's wake).
pub const RETRY_UNSYNCED: Duration = Duration::from_secs(10 * 60);

/// How long until the next clock check, given whether the system clock is synchronised.
#[must_use]
pub const fn next_check_after(system_synced: bool) -> Duration {
    if system_synced {
        CHECK_INTERVAL
    } else {
        RETRY_UNSYNCED
    }
}

/// How far off the UPS clock may be before it is set, in seconds.
///
/// Above the resolution of the method: the device counts whole seconds, and T15 read back
/// within +/-1 s of a time it had just been given. Correcting anything smaller would be
/// chasing that noise.
pub const THRESHOLD_S: i64 = 2;

/// What a comparison concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Within the threshold. Nothing to do.
    InSync {
        /// UPS minus system, seconds.
        offset_s: i64,
    },
    /// Off by more than the threshold, and the system clock can be trusted to correct it.
    Correct {
        /// UPS minus system, seconds.
        offset_s: i64,
    },
    /// Off by more than the threshold, but the system clock is not NTP-synchronised, so
    /// copying it could make the UPS clock worse rather than better.
    Untrusted {
        /// UPS minus system, seconds.
        offset_s: i64,
    },
    /// The UPS returned a date that is not a date. Never "corrected" blindly: an impossible
    /// reading may be a link problem rather than a clock problem.
    Implausible,
}

/// Compares the UPS clock with the system clock.
#[must_use]
pub fn judge(ups: UpsTime, system_unix_s: u64, system_synced: bool) -> Verdict {
    let Some(ups_s) = ups.to_unix_seconds() else {
        return Verdict::Implausible;
    };
    let offset_s = to_i64(ups_s) - to_i64(system_unix_s);
    if offset_s.abs() <= THRESHOLD_S {
        Verdict::InSync { offset_s }
    } else if system_synced {
        Verdict::Correct { offset_s }
    } else {
        Verdict::Untrusted { offset_s }
    }
}

fn to_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

/// Whether the system clock is NTP-synchronised, according to `timedatectl`.
///
/// Anything but a clear "yes" -- the command missing, failing, or saying no -- counts as not
/// synchronised. The consequence of a wrong "no" is a clock left slightly off; the consequence
/// of a wrong "yes" is the UPS clock set to a wrong time.
#[must_use]
pub fn system_clock_synced() -> bool {
    std::process::Command::new("timedatectl")
        .args(["show", "-p", "NTPSynchronized", "--value"])
        .output()
        .ok()
        .is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "yes")
}

/// Sleeps until just after the next whole second of the system clock.
///
/// So a time copied from the system clock is right to a fraction of a second rather than to
/// within one: the device counts whole seconds, and a write landing mid-second is otherwise up
/// to a second out before it starts.
pub fn sleep_to_next_second() {
    let into = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(Duration::ZERO, |d| {
            Duration::from_nanos(u64::from(d.subsec_nanos()))
        });
    std::thread::sleep(Duration::from_secs(1).saturating_sub(into) + Duration::from_millis(5));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsynchronised_system_clock_is_looked_at_again_soon() {
        // The first check runs seconds into a boot, before NTP: waiting six hours to look again
        // leaves the UPS uncorrected all that time (seen after T18's wake boot).
        assert_eq!(next_check_after(false), RETRY_UNSYNCED);
        assert!(RETRY_UNSYNCED < CHECK_INTERVAL);
        assert_eq!(next_check_after(true), CHECK_INTERVAL);
    }

    const NOW: u64 = 1_789_728_000;

    fn ups_at(unix: u64) -> UpsTime {
        UpsTime::from_unix_seconds(unix).unwrap()
    }

    #[test]
    fn within_the_threshold_is_left_alone() {
        assert_eq!(
            judge(ups_at(NOW - 2), NOW, true),
            Verdict::InSync { offset_s: -2 }
        );
        assert_eq!(
            judge(ups_at(NOW + 1), NOW, true),
            Verdict::InSync { offset_s: 1 }
        );
    }

    #[test]
    fn beyond_it_is_corrected_in_either_direction() {
        // The T15 finding: 21 s slow.
        assert_eq!(
            judge(ups_at(NOW - 21), NOW, true),
            Verdict::Correct { offset_s: -21 }
        );
        assert_eq!(
            judge(ups_at(NOW + 3), NOW, true),
            Verdict::Correct { offset_s: 3 }
        );
    }

    #[test]
    fn an_unsynchronised_system_clock_is_never_copied() {
        assert_eq!(
            judge(ups_at(NOW - 21), NOW, false),
            Verdict::Untrusted { offset_s: -21 }
        );
    }

    #[test]
    fn a_reset_clock_is_corrected_not_mistaken_for_garbage() {
        // What a full depletion may leave behind: a plausible date, years in the past.
        let reset = UpsTime {
            year: 2000,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: Some(0),
        };
        assert!(matches!(judge(reset, NOW, true), Verdict::Correct { offset_s } if offset_s < 0));
    }

    #[test]
    fn an_impossible_date_is_not_acted_on() {
        let nonsense = UpsTime {
            year: 2026,
            month: 13,
            day: 40,
            hour: 25,
            minute: 0,
            second: Some(0),
        };
        assert_eq!(judge(nonsense, NOW, true), Verdict::Implausible);
    }
}
