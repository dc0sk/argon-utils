// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's case-button thread: watches GPIO4 and acts on a press.
//!
//! The policy -- what a press means, and which shutdowns it may touch -- is
//! `argon_device::button`. This thread only turns edges into presses and presses into logind
//! calls, and says what it did every time, so a press that "did nothing" is explained in the log.

use argon_device::button::{Action, Button, DELAY, Pending, Why, is_press};
use argon_device::config::Config;
use argon_device::power::{Logind, PowerControl};
use argon_hal::{discovery, gpio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// The line the case MCU pulses (`ARGON-GPIO-BTN`, and observed on the ONE V1 and V3).
const LINE: u32 = 4;

/// What logind broadcasts to the terminals, and what the tray shows.
const MESSAGE: &str = "The Argon case button was pressed: powering off in one minute. \
                       Press it again to cancel.";

/// Starts watching the button, if `[button] action` asks for it.
///
/// `None` -- without touching GPIO4 at all -- when it does not, so the line stays free for
/// `argonctl button` and anything else. `full` decides whether a press acts or only logs what it
/// would do; power actions are a full-mode operation like everything else here.
pub fn spawn(config: &Config, full: bool, stopping: Arc<AtomicBool>) -> Option<JoinHandle<()>> {
    if !config.button.armed() {
        return None;
    }
    let Some(chip) = discovery::header_gpio_chip(LINE) else {
        eprintln!("argond: button: no header GPIO chip found; the button is not watched");
        return None;
    };
    let watcher = match gpio::EdgeWatcher::open(&chip.path, LINE, "argond-button") {
        Ok(w) => w,
        Err(e) => {
            // Almost always the vendor's argononed, which holds this line while it runs.
            eprintln!(
                "argond: button: cannot claim GPIO{LINE} ({e}); is argononed still running? \
                 Hand the case over with /usr/libexec/argon-utils/mcu-takeover"
            );
            return None;
        }
    };
    eprintln!(
        "argond: button: watching GPIO{LINE}; a press {} a poweroff {} s out, and a second \
         press cancels it",
        if full {
            "schedules"
        } else {
            "would schedule (mode is not full, so it only logs)"
        },
        DELAY.as_secs()
    );
    Some(std::thread::spawn(move || {
        run(&watcher, full, &stopping);
    }))
}

fn run(watcher: &gpio::EdgeWatcher, full: bool, stopping: &AtomicBool) {
    let mut button = Button::new(true);
    let mut logind = Logind;
    let mut rising: Option<u64> = None;

    while !stopping.load(Ordering::Relaxed) {
        let event = match watcher.next_edge(Duration::from_secs(1)) {
            Ok(Some(e)) => e,
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "argond: button: edge wait failed ({e}); the button is no longer watched"
                );
                return;
            }
        };
        match event.edge {
            gpio::Edge::Rising => rising = Some(event.timestamp_ns),
            gpio::Edge::Falling => {
                let Some(start) = rising.take() else {
                    continue;
                };
                let width = Duration::from_nanos(event.timestamp_ns.saturating_sub(start));
                if !is_press(width) {
                    eprintln!(
                        "argond: button: ignored a {} us pulse on GPIO{LINE}: not a press",
                        width.as_micros()
                    );
                    continue;
                }
                // The kernel's monotonic timestamp, so bounce detection is not at the mercy of
                // how late this thread happened to wake.
                let now = Duration::from_nanos(event.timestamp_ns);
                let pending = match logind.pending() {
                    Ok(None) => Pending::Nothing,
                    Ok(Some(at)) => Pending::At(at),
                    Err(_) => Pending::CannotTell,
                };
                recognise(&mut button, &mut logind, pending);
                act(&mut button, &mut logind, now, pending, full);
            }
        }
    }
}

/// Adopts a pending poweroff as ours if it carries the button's own message.
///
/// A restarted argond has forgotten which poweroff it placed, and `dpkg` restarts it on every
/// upgrade. Without this, a press during the countdown after a restart would find the countdown
/// "not ours" and leave it running -- the button unable to cancel its own shutdown. The message
/// is what the low-battery path uses for the same purpose; it is distinct from theirs, so a
/// low-battery poweroff is never adopted.
fn recognise<P: PowerControl>(button: &mut Button, power: &mut P, pending: Pending) {
    if let Pending::At(at) = pending {
        if power.wall_message().ok().flatten().as_deref() == Some(MESSAGE) {
            button.placed(at);
        }
    }
}

fn act<P: PowerControl>(
    button: &mut Button,
    power: &mut P,
    now: Duration,
    pending: Pending,
    full: bool,
) {
    match button.press(now, pending) {
        Action::Schedule if !full => eprintln!(
            "argond: button: pressed; would power off in {} s, but mode is not full",
            DELAY.as_secs()
        ),
        Action::Schedule => match power.schedule_poweroff(DELAY, MESSAGE) {
            Ok(at) => {
                button.placed(at);
                eprintln!(
                    "argond: button: pressed; poweroff in {} s -- press again to cancel",
                    DELAY.as_secs()
                );
            }
            Err(e) => eprintln!("argond: button: pressed, but the poweroff was refused: {e}"),
        },
        Action::Cancel => match power.cancel() {
            Ok(()) => {
                button.cancelled();
                eprintln!("argond: button: pressed again; poweroff cancelled");
            }
            Err(e) => eprintln!("argond: button: pressed again, but the cancel failed: {e}"),
        },
        Action::Ignore(why) => eprintln!(
            "argond: button: pressed; nothing done: {}",
            match why {
                Why::NotConfigured => "[button] action is none",
                Why::Bounce => "too soon after the last press (one press, bouncing)",
                Why::SomeoneElses =>
                    "a shutdown is already pending that the button did not place, so it is \
                     left alone",
                Why::CannotTell =>
                    "logind could not be read, so nothing was scheduled or cancelled",
            }
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Records what it is asked to do instead of doing it.
    #[derive(Default)]
    struct Fake {
        scheduled: Vec<(Duration, String)>,
        cancels: u32,
        refuse: bool,
        wall: Option<String>,
    }

    impl PowerControl for Fake {
        fn schedule_poweroff(
            &mut self,
            delay: Duration,
            message: &str,
        ) -> argon_hal::Result<SystemTime> {
            if self.refuse {
                return Err(argon_hal::Error::Timeout);
            }
            self.scheduled.push((delay, message.to_owned()));
            Ok(UNIX_EPOCH + Duration::from_secs(1_060))
        }
        fn cancel(&mut self) -> argon_hal::Result<()> {
            self.cancels += 1;
            Ok(())
        }
        fn pending(&mut self) -> argon_hal::Result<Option<SystemTime>> {
            Ok(None)
        }
        fn wall_message(&mut self) -> argon_hal::Result<Option<String>> {
            Ok(self.wall.clone())
        }
    }

    const fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn in_full_mode_a_press_places_the_announced_poweroff() {
        let (mut b, mut p) = (Button::new(true), Fake::default());
        act(&mut b, &mut p, s(10), Pending::Nothing, true);
        assert_eq!(p.scheduled.len(), 1);
        assert_eq!(
            p.scheduled[0].0, DELAY,
            "not the announced one-minute delay"
        );
        assert!(
            p.scheduled[0].1.contains("again to cancel"),
            "the notice must say how"
        );
    }

    #[test]
    fn outside_full_mode_a_press_powers_nothing_off() {
        let (mut b, mut p) = (Button::new(true), Fake::default());
        act(&mut b, &mut p, s(10), Pending::Nothing, false);
        assert!(
            p.scheduled.is_empty(),
            "scheduled a poweroff in read-only mode"
        );
    }

    #[test]
    fn the_second_press_cancels_the_one_it_placed() {
        let (mut b, mut p) = (Button::new(true), Fake::default());
        act(&mut b, &mut p, s(10), Pending::Nothing, true);
        let placed = UNIX_EPOCH + Duration::from_secs(1_060);
        act(&mut b, &mut p, s(20), Pending::At(placed), true);
        assert_eq!(p.cancels, 1);
        assert_eq!(
            p.scheduled.len(),
            1,
            "scheduled a second one instead of cancelling"
        );
    }

    #[test]
    fn a_press_never_cancels_a_shutdown_it_did_not_place() {
        // A low-battery poweroff pending: the press must leave it alone.
        let (mut b, mut p) = (Button::new(true), Fake::default());
        act(
            &mut b,
            &mut p,
            s(10),
            Pending::At(UNIX_EPOCH + Duration::from_secs(1_120)),
            true,
        );
        assert_eq!(
            p.cancels, 0,
            "cancelled a shutdown the button did not place"
        );
        assert!(p.scheduled.is_empty());
    }

    #[test]
    fn after_a_restart_the_button_still_cancels_its_own_countdown() {
        // A fresh policy, as after dpkg restarted argond, with the button's poweroff pending.
        let at = UNIX_EPOCH + Duration::from_secs(1_060);
        let (mut b, mut p) = (
            Button::new(true),
            Fake {
                wall: Some(MESSAGE.to_owned()),
                ..Fake::default()
            },
        );
        recognise(&mut b, &mut p, Pending::At(at));
        act(&mut b, &mut p, s(10), Pending::At(at), true);
        assert_eq!(
            p.cancels, 1,
            "a restarted daemon could not cancel its own countdown"
        );
    }

    #[test]
    fn a_low_battery_poweroff_is_never_adopted() {
        // Pending, with someone else's message: still not ours, still left alone.
        let at = UNIX_EPOCH + Duration::from_secs(1_120);
        let (mut b, mut p) = (
            Button::new(true),
            Fake {
                wall: Some("Battery critical: powering off".to_owned()),
                ..Fake::default()
            },
        );
        recognise(&mut b, &mut p, Pending::At(at));
        act(&mut b, &mut p, s(10), Pending::At(at), true);
        assert_eq!(p.cancels, 0, "a press cancelled a low-battery poweroff");
    }

    #[test]
    fn a_refused_poweroff_is_not_recorded_as_ours() {
        // If logind refused, nothing is pending, and a later press must schedule rather than try
        // to cancel a poweroff that does not exist.
        let (mut b, mut p) = (
            Button::new(true),
            Fake {
                refuse: true,
                ..Fake::default()
            },
        );
        act(&mut b, &mut p, s(10), Pending::Nothing, true);
        p.refuse = false;
        act(&mut b, &mut p, s(20), Pending::Nothing, true);
        assert_eq!(p.scheduled.len(), 1);
        assert_eq!(p.cancels, 0);
    }
}
