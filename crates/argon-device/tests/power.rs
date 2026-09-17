// SPDX-License-Identifier: GPL-3.0-or-later
//! The shutdown coordinator, against a fake logind. Nothing here schedules a real poweroff.

use argon_device::power::{Action, PowerControl, ShutdownCoordinator, parse_scheduled_shutdown};
use argon_hal::{Error, Result};
use argon_proto::ups::policy::Advice;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct FakeLogind {
    pending: Option<SystemTime>,
    schedules: u32,
    cancels: u32,
    fail_schedule: bool,
    fail_cancel: bool,
}

impl PowerControl for FakeLogind {
    fn schedule_poweroff(&mut self, delay: Duration, _message: &str) -> Result<SystemTime> {
        if self.fail_schedule {
            return Err(Error::Io(std::io::Error::other("polkit: not authorised")));
        }
        self.schedules += 1;
        let at = UNIX_EPOCH + Duration::from_secs(1_000_000) + delay;
        self.pending = Some(at);
        Ok(at)
    }
    fn cancel(&mut self) -> Result<()> {
        if self.fail_cancel {
            return Err(Error::Io(std::io::Error::other("cancel failed")));
        }
        self.cancels += 1;
        self.pending = None;
        Ok(())
    }
    fn pending(&mut self) -> Result<Option<SystemTime>> {
        Ok(self.pending)
    }
}

const DELAY: Duration = Duration::from_secs(120);

fn coordinator(fake: FakeLogind) -> ShutdownCoordinator<FakeLogind> {
    ShutdownCoordinator::new(fake, DELAY, false)
}

#[test]
fn advice_schedules_exactly_once() {
    // The policy repeats its advice on every poll. Rescheduling each time would keep pushing
    // the poweroff back and it would never happen.
    let mut c = coordinator(FakeLogind::default());
    assert!(matches!(
        c.on_advice(Advice::Shutdown),
        Action::Scheduled { .. }
    ));
    for _ in 0..10 {
        assert_eq!(c.on_advice(Advice::Shutdown), Action::None);
    }
    assert_eq!(c_power(&mut c).schedules, 1);
}

#[test]
fn mains_returning_cancels_our_shutdown() {
    let mut c = coordinator(FakeLogind::default());
    c.on_advice(Advice::Shutdown);
    assert_eq!(c.on_advice(Advice::None), Action::Cancelled);
    assert_eq!(c_power(&mut c).cancels, 1);
    assert_eq!(c.scheduled_at(), None);
}

#[test]
fn a_held_shutdown_is_not_scheduled_and_cancels_nothing_it_did_not_schedule() {
    let mut c = coordinator(FakeLogind::default());
    let held = Advice::HeldForUptime {
        remaining: Duration::from_secs(30),
    };
    assert_eq!(c.on_advice(held), Action::None);
    assert_eq!(c_power(&mut c).schedules, 0);
    assert_eq!(c_power(&mut c).cancels, 0);
}

#[test]
fn someone_elses_pending_shutdown_is_left_alone() {
    // An administrator's shutdown is not ours to claim, and not ours to cancel later.
    let theirs = UNIX_EPOCH + Duration::from_secs(42);
    let mut c = coordinator(FakeLogind {
        pending: Some(theirs),
        ..FakeLogind::default()
    });
    assert_eq!(
        c.on_advice(Advice::Shutdown),
        Action::AlreadyPending { at: theirs }
    );
    assert_eq!(
        c.on_advice(Advice::None),
        Action::None,
        "cancelled a shutdown it did not schedule"
    );
    let power = c_power(&mut c);
    assert_eq!((power.schedules, power.cancels), (0, 0));
    assert_eq!(power.pending, Some(theirs));
}

#[test]
fn an_operator_cancelling_ours_makes_us_stand_down() {
    // Scheduling it again on the next poll would fight the person who just cancelled it.
    let mut c = coordinator(FakeLogind::default());
    c.on_advice(Advice::Shutdown);
    c_power(&mut c).pending = None; // someone ran `shutdown -c`
    assert_eq!(c.on_advice(Advice::Shutdown), Action::OverriddenByOperator);
    for _ in 0..5 {
        assert_eq!(c.on_advice(Advice::Shutdown), Action::None);
    }
    assert_eq!(
        c_power(&mut c).schedules,
        1,
        "rescheduled after an operator cancelled"
    );
}

#[test]
fn standing_down_ends_when_the_advice_clears() {
    let mut c = coordinator(FakeLogind::default());
    c.on_advice(Advice::Shutdown);
    c_power(&mut c).pending = None;
    c.on_advice(Advice::Shutdown); // stood down
    c.on_advice(Advice::None); // mains back
    // A later, separate critical episode is acted on again.
    assert!(matches!(
        c.on_advice(Advice::Shutdown),
        Action::Scheduled { .. }
    ));
    assert_eq!(c_power(&mut c).schedules, 2);
}

#[test]
fn a_failed_schedule_is_retried_on_the_next_poll() {
    // Level-triggered where it matters: a refusal (say, a missing polkit rule) must not leave
    // the coordinator believing a shutdown is pending.
    let mut c = coordinator(FakeLogind {
        fail_schedule: true,
        ..FakeLogind::default()
    });
    assert!(matches!(c.on_advice(Advice::Shutdown), Action::Failed(_)));
    assert_eq!(c.scheduled_at(), None);
    c_power(&mut c).fail_schedule = false;
    assert!(matches!(
        c.on_advice(Advice::Shutdown),
        Action::Scheduled { .. }
    ));
}

#[test]
fn a_failed_cancel_keeps_trying() {
    let mut c = coordinator(FakeLogind::default());
    c.on_advice(Advice::Shutdown);
    c_power(&mut c).fail_cancel = true;
    assert!(matches!(c.on_advice(Advice::None), Action::Failed(_)));
    assert!(
        c.scheduled_at().is_some(),
        "forgot a shutdown it failed to cancel"
    );
    c_power(&mut c).fail_cancel = false;
    assert_eq!(c.on_advice(Advice::None), Action::Cancelled);
}

#[test]
fn dry_run_touches_nothing() {
    let mut c = ShutdownCoordinator::new(FakeLogind::default(), DELAY, true);
    assert_eq!(c.on_advice(Advice::Shutdown), Action::WouldSchedule);
    assert_eq!(c.on_advice(Advice::Shutdown), Action::None);
    assert_eq!(c.on_advice(Advice::None), Action::WouldCancel);
    let power = c_power(&mut c);
    assert_eq!((power.schedules, power.cancels), (0, 0));
}

#[test]
fn logind_idle_output_parses_as_nothing_pending() {
    // Observed on the development machine with no shutdown scheduled.
    assert_eq!(
        parse_scheduled_shutdown("(st) \"\" 18446744073709551615\n"),
        None
    );
}

#[test]
fn logind_pending_output_parses() {
    let (kind, at) = parse_scheduled_shutdown("(st) \"poweroff\" 1789999999000000").unwrap();
    assert_eq!(kind, "poweroff");
    assert_eq!(at, UNIX_EPOCH + Duration::from_secs(1_789_999_999));
}

#[test]
fn malformed_logind_output_is_not_a_pending_shutdown() {
    for junk in [
        "",
        "garbage",
        "(st)",
        "(st) \"poweroff\" notanumber",
        "(s) \"poweroff\" 5",
    ] {
        assert_eq!(parse_scheduled_shutdown(junk), None, "{junk:?}");
    }
}

/// Test-only access to the fake behind the coordinator.
fn c_power(c: &mut ShutdownCoordinator<FakeLogind>) -> &mut FakeLogind {
    c.power_mut()
}
