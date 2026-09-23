// SPDX-License-Identifier: GPL-3.0-or-later
//! Turning logind's scheduled poweroff into desktop notifications: the policy, without I/O.
//!
//! logind's `shutdown` broadcast reaches terminals only, and the tray shows a pending poweroff
//! only to someone looking at it. On a desktop that meant a poweroff could be a minute away with
//! nothing on screen saying so -- which is how the case button's countdown first went unseen on
//! the ONE V3. This watches the schedule itself and says so when it appears, changes or goes.
//!
//! - **A low-battery poweroff is not announced twice.** The UPS notices already cover it, from
//!   the status file, so one whose time matches that file's `shutdown_at` is left to them.
//! - **The button's countdown is named as such**, by its wall message, so the notice can say how
//!   to cancel it.
//! - **"Cannot tell" changes nothing.** A failed read of logind is neither a new poweroff nor a
//!   cancelled one.

use crate::status::{Notice, Urgency};
use std::time::{Duration, SystemTime};

/// Two poweroff times this close are the same poweroff: logind reports microseconds, the status
/// file whole seconds, and `shutdown` itself rounds.
const SAME: Duration = Duration::from_secs(90);

/// One look at logind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Nothing scheduled.
    Nothing,
    /// A poweroff at this time, placed with this message.
    Poweroff {
        /// When.
        at: SystemTime,
        /// logind's `WallMessage`, if any.
        message: Option<String>,
    },
    /// logind could not be read.
    CannotTell,
}

/// Watches logind's schedule for changes worth telling a person about.
#[derive(Debug, Default)]
pub struct ShutdownWatcher {
    /// The poweroff last announced (or deliberately left to the UPS notices).
    known: Option<SystemTime>,
}

impl ShutdownWatcher {
    /// A watcher that has announced nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self { known: None }
    }

    /// Looks at logind's schedule, with the UPS status file's pending poweroff alongside, and
    /// returns a notice if something changed. `hhmm` formats a time of day.
    pub fn observe(
        &mut self,
        seen: &Seen,
        ups_shutdown_at: Option<SystemTime>,
        hhmm: &dyn Fn(SystemTime) -> String,
        button_message: &str,
    ) -> Option<Notice> {
        match seen {
            Seen::CannotTell => None,
            Seen::Nothing => self.known.take().map(|_| Notice {
                urgency: Urgency::Normal,
                text: "The scheduled poweroff was cancelled.".to_owned(),
            }),
            Seen::Poweroff { at, message } => {
                if self.known.is_some_and(|k| close(k, *at)) {
                    return None;
                }
                self.known = Some(*at);
                // Already announced by the UPS notices: the low-battery poweroff.
                if ups_shutdown_at.is_some_and(|u| close(u, *at)) {
                    return None;
                }
                let text = if message.as_deref() == Some(button_message) {
                    format!(
                        "Powering off at {}: the case button was pressed. Press it again to \
                         cancel.",
                        hhmm(*at)
                    )
                } else {
                    format!(
                        "A poweroff is scheduled for {}. Cancel it with `shutdown -c`.",
                        hhmm(*at)
                    )
                };
                Some(Notice {
                    urgency: Urgency::Critical,
                    text,
                })
            }
        }
    }
}

fn close(a: SystemTime, b: SystemTime) -> bool {
    a.duration_since(b)
        .or_else(|_| b.duration_since(a))
        .is_ok_and(|d| d <= SAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const BUTTON: &str = "button message";

    fn at(n: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(n)
    }
    fn hhmm(_: SystemTime) -> String {
        "20:53".to_owned()
    }
    fn poweroff(n: u64, message: Option<&str>) -> Seen {
        Seen::Poweroff {
            at: at(n),
            message: message.map(str::to_owned),
        }
    }

    #[test]
    fn the_buttons_countdown_is_announced_with_how_to_cancel_it() {
        let mut w = ShutdownWatcher::new();
        let n = w
            .observe(&poweroff(1_060, Some(BUTTON)), None, &hhmm, BUTTON)
            .expect("a pending poweroff went unannounced");
        assert_eq!(n.urgency, Urgency::Critical);
        assert!(n.text.contains("case button"), "{}", n.text);
        assert!(n.text.contains("again to cancel"), "{}", n.text);
        assert!(n.text.contains("20:53"), "{}", n.text);
    }

    #[test]
    fn it_is_said_once_and_its_cancellation_is_said_too() {
        let mut w = ShutdownWatcher::new();
        assert!(
            w.observe(&poweroff(1_060, Some(BUTTON)), None, &hhmm, BUTTON)
                .is_some()
        );
        assert!(
            w.observe(&poweroff(1_060, Some(BUTTON)), None, &hhmm, BUTTON)
                .is_none(),
            "announced the same poweroff on every tick"
        );
        let n = w
            .observe(&Seen::Nothing, None, &hhmm, BUTTON)
            .expect("cancel unannounced");
        assert!(n.text.contains("cancelled"), "{}", n.text);
        assert!(w.observe(&Seen::Nothing, None, &hhmm, BUTTON).is_none());
    }

    #[test]
    fn a_low_battery_poweroff_is_left_to_the_ups_notices() {
        // The same poweroff, seen in logind and in the status file: say it once, not twice.
        let mut w = ShutdownWatcher::new();
        let from_battery = poweroff(1_120, Some("battery critical"));
        assert!(
            w.observe(&from_battery, Some(at(1_120)), &hhmm, BUTTON)
                .is_none(),
            "a low-battery poweroff announced twice"
        );
        // Nor its cancellation, which the UPS notices also cover.
        assert!(
            w.observe(&from_battery, Some(at(1_120)), &hhmm, BUTTON)
                .is_none()
        );
    }

    #[test]
    fn anyone_elses_poweroff_is_announced_with_the_general_way_to_cancel() {
        let mut w = ShutdownWatcher::new();
        let n = w
            .observe(&poweroff(3_000, None), None, &hhmm, BUTTON)
            .expect("a `shutdown +30` went unannounced");
        assert!(n.text.contains("shutdown -c"), "{}", n.text);
        assert!(
            !n.text.contains("case button"),
            "blamed the button: {}",
            n.text
        );
    }

    #[test]
    fn a_moved_poweroff_is_announced_again() {
        let mut w = ShutdownWatcher::new();
        w.observe(&poweroff(1_060, None), None, &hhmm, BUTTON);
        assert!(
            w.observe(&poweroff(2_000, None), None, &hhmm, BUTTON)
                .is_some()
        );
    }

    #[test]
    fn a_failed_read_is_neither_a_poweroff_nor_a_cancellation() {
        let mut w = ShutdownWatcher::new();
        w.observe(&poweroff(1_060, Some(BUTTON)), None, &hhmm, BUTTON);
        assert!(w.observe(&Seen::CannotTell, None, &hhmm, BUTTON).is_none());
        // And the pending one is still known: a real cancel afterwards is still announced.
        assert!(w.observe(&Seen::Nothing, None, &hhmm, BUTTON).is_some());
    }
}
