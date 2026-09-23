// SPDX-License-Identifier: GPL-3.0-or-later
//! What a press of the case button does: the policy, without any I/O.
//!
//! The case MCU turns a press into one pulse on GPIO4 (`ONE-V1-BTN-PULSE`,
//! `ONE-V3-BTN-DOUBLE-TAP`): about 20 ms on both cases measured. It is one event with no gesture
//! in it -- on the ONE V1 any tap sends it, on the ONE V3 only a double-tap -- so there is one
//! action, not a menu of them.
//!
//! The action is an **announced** poweroff: scheduled through logind a minute out, visible in the
//! tray, and cancelled by pressing again. The rules are the low-battery path's, for the same
//! reasons:
//!
//! - **Only our own shutdown is cancelled.** "Ours" means logind's pending time is the one we
//!   placed. A press must never cancel a low-battery poweroff, or an operator's `shutdown +30`:
//!   a stray tap is not a decision to keep a machine running on a dying battery.
//! - **Nothing is scheduled over someone else's.** If another shutdown is already pending, a press
//!   leaves it alone rather than placing a second one or claiming it.
//! - **"Cannot tell" is not "nothing pending".** If logind cannot be read, a press does nothing:
//!   scheduling blind could place a second poweroff, and cancelling blind could remove one we do
//!   not own.
//! - **A bouncing contact is one press.** Events closer together than [`MIN_GAP`] are one.

use std::time::{Duration, SystemTime};

/// The narrowest pulse taken as a button press. Both cases measured send ~20 ms: 20.00 ms on the
/// ONE V3, 20.09 ms on the ONE V1. The band is generous on both sides without admitting a glitch.
pub const PULSE_MIN: Duration = Duration::from_millis(10);
/// The widest pulse taken as a button press.
pub const PULSE_MAX: Duration = Duration::from_millis(40);

/// Presses closer together than this are one: a contact bouncing, not a person pressing twice.
///
/// Well under the gap between two deliberate presses -- the operators in T3 left ~3 s -- and well
/// over any bounce. The V1 already merges a double-tap into one pulse, and the V3 sends one per
/// double-tap, so a person cannot produce two events this close on either case.
pub const MIN_GAP: Duration = Duration::from_secs(1);

/// How far out the announced poweroff is placed.
///
/// A minute because logind's `shutdown` works in whole minutes and this shares its path with the
/// low-battery shutdown -- and a minute is enough to see the notice and press again.
pub const DELAY: Duration = Duration::from_secs(60);

/// Whether a pulse this wide is a button press.
#[must_use]
pub fn is_press(width: Duration) -> bool {
    (PULSE_MIN..=PULSE_MAX).contains(&width)
}

/// What logind says is pending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pending {
    /// Nothing is scheduled.
    Nothing,
    /// A poweroff is scheduled for this time.
    At(SystemTime),
    /// logind could not be read.
    CannotTell,
}

/// What to do about a press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Place the announced poweroff.
    Schedule,
    /// Cancel the poweroff this button placed.
    Cancel,
    /// Do nothing, for this reason.
    Ignore(Why),
}

/// Why a press was ignored. Each is logged, so a press that "did nothing" can be explained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// `[button] action` is `none`.
    NotConfigured,
    /// Too soon after the last press: one press, bouncing.
    Bounce,
    /// A shutdown is pending that this button did not place.
    SomeoneElses,
    /// logind could not be read.
    CannotTell,
}

/// The button policy.
#[derive(Debug, Clone)]
pub struct Button {
    armed: bool,
    /// The poweroff this button placed, as logind reported it.
    ours: Option<SystemTime>,
    /// When the last press was accepted, on the caller's monotonic clock.
    last: Option<Duration>,
}

impl Button {
    /// A policy that acts on presses (`armed`), or only logs them.
    #[must_use]
    pub const fn new(armed: bool) -> Self {
        Self {
            armed,
            ours: None,
            last: None,
        }
    }

    /// A press arrived at `now` (monotonic), with `pending` read from logind just now.
    pub fn press(&mut self, now: Duration, pending: Pending) -> Action {
        if !self.armed {
            return Action::Ignore(Why::NotConfigured);
        }
        if self.last.is_some_and(|l| now.saturating_sub(l) < MIN_GAP) {
            return Action::Ignore(Why::Bounce);
        }
        self.last = Some(now);
        match pending {
            Pending::CannotTell => Action::Ignore(Why::CannotTell),
            Pending::At(at) if self.ours == Some(at) => Action::Cancel,
            Pending::At(_) => {
                // Not the one we placed. If we had placed one, it has been cancelled or replaced
                // since -- by an operator, or by the low-battery path -- so we no longer own it.
                self.ours = None;
                Action::Ignore(Why::SomeoneElses)
            }
            Pending::Nothing => {
                // Ours, if any, is gone: cancelled elsewhere, with `shutdown -c` or the tray.
                self.ours = None;
                Action::Schedule
            }
        }
    }

    /// The poweroff was placed, and logind reports it for `at`.
    pub const fn placed(&mut self, at: SystemTime) {
        self.ours = Some(at);
    }

    /// Our poweroff was cancelled.
    pub const fn cancelled(&mut self) {
        self.ours = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }
    fn at(n: u64) -> SystemTime {
        UNIX_EPOCH + s(n)
    }

    #[test]
    fn the_measured_pulses_are_presses_and_glitches_are_not() {
        assert!(is_press(Duration::from_micros(20_002)), "the V3's 20.00 ms");
        assert!(is_press(Duration::from_micros(20_094)), "the V1's 20.09 ms");
        assert!(!is_press(Duration::from_micros(500)), "a glitch");
        assert!(!is_press(s(2)), "a line held high");
    }

    #[test]
    fn unconfigured_it_only_ever_ignores() {
        let mut b = Button::new(false);
        assert_eq!(
            b.press(s(10), Pending::Nothing),
            Action::Ignore(Why::NotConfigured)
        );
    }

    #[test]
    fn a_press_schedules_and_a_second_press_cancels_it() {
        let mut b = Button::new(true);
        assert_eq!(b.press(s(10), Pending::Nothing), Action::Schedule);
        b.placed(at(1_060));
        assert_eq!(b.press(s(20), Pending::At(at(1_060))), Action::Cancel);
        b.cancelled();
        // And after that, a press schedules again.
        assert_eq!(b.press(s(30), Pending::Nothing), Action::Schedule);
    }

    #[test]
    fn a_press_never_cancels_a_low_battery_poweroff() {
        // The shutdown that must not be dismissed by a stray tap: pending, and not ours.
        let mut b = Button::new(true);
        assert_eq!(
            b.press(s(10), Pending::At(at(1_120))),
            Action::Ignore(Why::SomeoneElses)
        );
    }

    #[test]
    fn once_ours_is_replaced_by_someone_elses_we_no_longer_own_it() {
        // We placed one; an operator cancelled it and placed their own. A press must not cancel
        // theirs on the strength of having once placed ours.
        let mut b = Button::new(true);
        b.press(s(10), Pending::Nothing);
        b.placed(at(1_060));
        assert_eq!(
            b.press(s(20), Pending::At(at(1_900))),
            Action::Ignore(Why::SomeoneElses)
        );
        assert_eq!(
            b.press(s(30), Pending::At(at(1_900))),
            Action::Ignore(Why::SomeoneElses),
            "claimed the operator's shutdown later"
        );
    }

    #[test]
    fn if_ours_was_cancelled_elsewhere_the_next_press_schedules_again() {
        // `shutdown -c`, or the tray: logind reports nothing, so ours is gone.
        let mut b = Button::new(true);
        b.press(s(10), Pending::Nothing);
        b.placed(at(1_060));
        assert_eq!(b.press(s(20), Pending::Nothing), Action::Schedule);
    }

    #[test]
    fn when_logind_cannot_be_read_a_press_does_nothing() {
        // Scheduling blind could place a second poweroff; cancelling blind could remove one we
        // do not own. So neither.
        let mut b = Button::new(true);
        assert_eq!(
            b.press(s(10), Pending::CannotTell),
            Action::Ignore(Why::CannotTell)
        );
        b.placed(at(1_060));
        assert_eq!(
            b.press(s(20), Pending::CannotTell),
            Action::Ignore(Why::CannotTell)
        );
    }

    #[test]
    fn a_bouncing_contact_is_one_press() {
        let mut b = Button::new(true);
        assert_eq!(b.press(s(10), Pending::Nothing), Action::Schedule);
        b.placed(at(1_060));
        assert_eq!(
            b.press(Duration::from_millis(10_300), Pending::At(at(1_060))),
            Action::Ignore(Why::Bounce),
            "a bounce cancelled the shutdown it had just placed"
        );
        // A real second press, well after, still cancels.
        assert_eq!(b.press(s(15), Pending::At(at(1_060))), Action::Cancel);
    }
}
