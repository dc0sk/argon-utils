// SPDX-License-Identifier: GPL-3.0-or-later
//! The shutdown coordinator, against a fake logind. Nothing here schedules a real poweroff.
//!
//! # Why this fake is shaped the way it is
//!
//! The previous fake made `schedule_poweroff` one infallible step and `pending()` infallible.
//! The real [`argon_device::power::Logind`] is two commands -- `shutdown`, then a `busctl`
//! read-back -- either of which can fail on its own, and it holds a wall message that
//! identifies whose shutdown is pending. Four defects lived in exactly that gap and the
//! suite was green with all four present, so this fake models all three parts: the pending
//! time, the wall message, and independent failure of each operation.

use argon_device::power::{
    Action, PowerControl, SHUTDOWN_MESSAGE, ShutdownCoordinator, parse_scheduled_shutdown,
};
use argon_hal::{Error, Result};
use argon_proto::ups::policy::{Advice, Decision, Level};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A stand-in for logind that can fail each operation independently.
///
/// The flags are separate bools on purpose: each models one command failing on its own, which
/// is exactly the shape the real two-command `Logind` has and exactly what the previous fake
/// could not express.
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent failure injections"
)]
#[derive(Default)]
struct FakeLogind {
    /// What logind would report as scheduled, with the wall message that came with it.
    pending: Option<(SystemTime, String)>,
    schedules: u32,
    cancels: u32,
    fail_schedule: bool,
    fail_cancel: bool,
    /// `pending()` fails, as a `busctl` that cannot be spawned or times out would.
    fail_readback: bool,
    /// The wall message cannot be read.
    fail_wall: bool,
}

impl FakeLogind {
    /// Someone else's shutdown, already pending before we look.
    fn with_foreign_shutdown() -> Self {
        Self {
            pending: Some((
                now() + Duration::from_secs(3600),
                "operator: back shortly".into(),
            )),
            ..Self::default()
        }
    }

    /// Our own shutdown, pending with no in-memory record of it -- what a restart leaves.
    fn with_our_shutdown_from_before_a_restart() -> Self {
        Self {
            pending: Some((
                now() + Duration::from_secs(120),
                SHUTDOWN_MESSAGE.to_owned(),
            )),
            ..Self::default()
        }
    }
}

impl PowerControl for FakeLogind {
    fn schedule_poweroff(&mut self, delay: Duration, message: &str) -> Result<SystemTime> {
        if self.fail_schedule {
            return Err(Error::Io(std::io::Error::other("polkit: not authorised")));
        }
        self.schedules += 1;
        let at = now() + delay;
        // The command ran: logind holds the shutdown from here on, whatever the read-back
        // does. Returning the intended time when it fails is the real Logind's contract.
        self.pending = Some((at, message.to_owned()));
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
        if self.fail_readback {
            return Err(Error::Io(std::io::Error::other(
                "busctl: connection refused",
            )));
        }
        Ok(self.pending.as_ref().map(|(at, _)| *at))
    }

    fn wall_message(&mut self) -> Result<Option<String>> {
        if self.fail_wall {
            return Err(Error::Io(std::io::Error::other(
                "busctl: connection refused",
            )));
        }
        Ok(self.pending.as_ref().map(|(_, m)| m.clone()))
    }
}

/// A fixed "now" for the fake, so scheduled times are comparable across calls.
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(2_000_000_000)
}
const DELAY: Duration = Duration::from_secs(120);

fn coordinator(fake: FakeLogind) -> ShutdownCoordinator<FakeLogind> {
    ShutdownCoordinator::new(fake, DELAY, false)
}

fn c_power(c: &mut ShutdownCoordinator<FakeLogind>) -> &mut FakeLogind {
    c.power_mut()
}

/// The battery is confirmed critical and shutdown is advised.
const fn critical() -> Decision {
    Decision {
        level: Level::Critical,
        changed_from: None,
        advice: Advice::Shutdown,
        confirmed_recovery: false,
    }
}

/// Mains is back and confirmed: the only decision that cancels.
const fn on_mains() -> Decision {
    Decision {
        level: Level::OnMains,
        changed_from: None,
        advice: Advice::None,
        confirmed_recovery: true,
    }
}

/// One mains reading on a flapping supply. Not yet a recovery.
const fn mains_blip() -> Decision {
    Decision {
        confirmed_recovery: false,
        ..on_mains()
    }
}

/// A failed read. Not evidence of anything.
const fn unknown() -> Decision {
    Decision {
        level: Level::Unknown,
        changed_from: None,
        advice: Advice::None,
        confirmed_recovery: false,
    }
}

/// On battery, above critical: a gauge that wobbled upward is not a recovery.
const fn low_on_battery() -> Decision {
    Decision {
        level: Level::Low,
        changed_from: None,
        advice: Advice::None,
        confirmed_recovery: false,
    }
}

const fn held(remaining: Duration) -> Decision {
    Decision {
        level: Level::Critical,
        changed_from: None,
        advice: Advice::HeldForUptime { remaining },
        confirmed_recovery: false,
    }
}

#[test]
fn advice_schedules_exactly_once() {
    // The policy repeats its advice on every poll. Rescheduling each time would keep pushing
    // the poweroff back and it would never happen.
    let mut c = coordinator(FakeLogind::default());
    assert!(matches!(
        c.on_decision(&critical()),
        Action::Scheduled { .. }
    ));
    for _ in 0..10 {
        assert_eq!(c.on_decision(&critical()), Action::None);
    }
    assert_eq!(c_power(&mut c).schedules, 1);
}

#[test]
fn mains_returning_cancels_our_shutdown() {
    let mut c = coordinator(FakeLogind::default());
    c.on_decision(&critical());
    assert_eq!(c.on_decision(&on_mains()), Action::Cancelled);
    assert_eq!(c_power(&mut c).cancels, 1);
    assert_eq!(c.scheduled_at(), None);
}

#[test]
fn a_held_shutdown_is_not_scheduled_and_cancels_nothing_it_did_not_schedule() {
    let mut c = coordinator(FakeLogind::default());
    assert_eq!(c.on_decision(&held(Duration::from_secs(30))), Action::None);
    assert_eq!(c_power(&mut c).schedules, 0);
    assert_eq!(c_power(&mut c).cancels, 0);
}

#[test]
fn a_failed_reading_does_not_cancel_a_placed_poweroff() {
    // The defect this replaces: any advice other than Shutdown cancelled. On a battery that
    // is genuinely emptying, the serial link is the first thing to flake -- so one timeout
    // threw away a confirmed shutdown, and a link flaking once a minute meant the poweroff
    // was never reached at all.
    let mut c = coordinator(FakeLogind::default());
    let at = match c.on_decision(&critical()) {
        Action::Scheduled { at } => at,
        other => panic!("expected a schedule, got {other:?}"),
    };

    assert_eq!(
        c.on_decision(&unknown()),
        Action::None,
        "cancelled on a failed read"
    );
    assert_eq!(c.on_decision(&held(Duration::from_secs(5))), Action::None);
    assert_eq!(c.on_decision(&low_on_battery()), Action::None);
    assert_eq!(
        c.on_decision(&mains_blip()),
        Action::None,
        "cancelled on one mains reading from a flapping supply"
    );

    assert_eq!(
        c_power(&mut c).cancels,
        0,
        "cancelled without mains returning"
    );
    assert_eq!(c.scheduled_at(), Some(at), "lost track of the poweroff");
    // ... and mains returning still cancels it.
    assert_eq!(c.on_decision(&on_mains()), Action::Cancelled);
}

#[test]
fn a_failed_readback_does_not_orphan_the_poweroff() {
    // The worst of the lot. `shutdown` succeeds, the read-back fails; the coordinator used to
    // record nothing, then report "already pending" forever and never cancel -- so the
    // machine powered off with mains restored, with no notice in the status file either.
    let mut c = coordinator(FakeLogind {
        fail_readback: true,
        ..FakeLogind::default()
    });

    assert!(matches!(
        c.on_decision(&critical()),
        Action::Scheduled { .. }
    ));
    assert!(
        c.scheduled_at().is_some(),
        "did not record what it scheduled"
    );

    // Still failing: it must not schedule a second one.
    for _ in 0..5 {
        assert_eq!(c.on_decision(&critical()), Action::None);
    }
    assert_eq!(
        c_power(&mut c).schedules,
        1,
        "rescheduled while it could not read the state, pushing the poweroff away each time"
    );

    // Mains returns while the read-back is still broken: cancel anyway. It is ours.
    assert_eq!(c.on_decision(&on_mains()), Action::Cancelled);
    assert_eq!(c_power(&mut c).cancels, 1);
}

#[test]
fn someone_elses_pending_shutdown_is_left_alone() {
    let mut c = coordinator(FakeLogind::with_foreign_shutdown());
    assert!(matches!(
        c.on_decision(&critical()),
        Action::AlreadyPending { .. }
    ));
    assert_eq!(c_power(&mut c).schedules, 0);
    assert_eq!(
        c.scheduled_at(),
        None,
        "claimed a shutdown it did not place"
    );

    // And it is not cancelled when the battery recovers.
    assert_eq!(c.on_decision(&on_mains()), Action::None);
    assert_eq!(
        c_power(&mut c).cancels,
        0,
        "cancelled someone else's shutdown"
    );
    assert!(c_power(&mut c).pending.is_some());
}

#[test]
fn a_shutdown_that_replaced_ours_is_not_cancelled_by_us() {
    // Ours is placed; an operator cancels it and schedules their own. Theirs is not ours to
    // cancel later -- and the coordinator must not wedge, which is what happened when `ours`
    // stayed set forever: every later critical episode saw "something is pending" and never
    // scheduled again.
    let mut c = coordinator(FakeLogind::default());
    c.on_decision(&critical());
    c_power(&mut c).pending = Some((
        now() + Duration::from_secs(3600),
        "operator: rebooting for maintenance".into(),
    ));

    assert!(
        matches!(c.on_decision(&critical()), Action::AlreadyPending { .. }),
        "did not notice its poweroff had been replaced"
    );
    assert_eq!(c.on_decision(&on_mains()), Action::None);
    assert_eq!(
        c_power(&mut c).cancels,
        0,
        "cancelled the operator's own shutdown"
    );

    // A later episode, after theirs is gone, still works.
    c_power(&mut c).pending = None;
    assert!(
        matches!(c.on_decision(&critical()), Action::Scheduled { .. }),
        "wedged: never scheduled again"
    );
}

#[test]
fn our_poweroff_is_adopted_again_after_a_restart() {
    // A restart -- including the one dpkg performs on every package upgrade -- loses the
    // in-memory record. logind's wall message is what identifies the poweroff as ours, and
    // without adopting it an `apt upgrade` during a mains outage turned a cancellable
    // poweroff into an unconditional one.
    let mut c = coordinator(FakeLogind::with_our_shutdown_from_before_a_restart());

    assert!(
        matches!(c.on_decision(&critical()), Action::Adopted { .. }),
        "did not recognise its own poweroff after a restart"
    );
    assert!(c.scheduled_at().is_some());
    assert_eq!(c_power(&mut c).schedules, 0, "scheduled a second poweroff");

    assert_eq!(c.on_decision(&on_mains()), Action::Cancelled);
    assert_eq!(c_power(&mut c).cancels, 1);
}

#[test]
fn an_unreadable_wall_message_means_not_ours() {
    // Adopting on a guess would mean cancelling a shutdown somebody else placed. The
    // conservative answer is to leave it alone.
    let mut c = coordinator(FakeLogind {
        fail_wall: true,
        ..FakeLogind::with_our_shutdown_from_before_a_restart()
    });
    assert!(matches!(
        c.on_decision(&critical()),
        Action::AlreadyPending { .. }
    ));
    assert_eq!(c.scheduled_at(), None);
}

#[test]
fn an_operator_cancelling_ours_stands_us_down() {
    let mut c = coordinator(FakeLogind::default());
    c.on_decision(&critical());
    c_power(&mut c).pending = None; // `shutdown -c`

    assert_eq!(c.on_decision(&critical()), Action::OverriddenByOperator);
    for _ in 0..5 {
        assert_eq!(c.on_decision(&critical()), Action::None);
    }
    assert_eq!(
        c_power(&mut c).schedules,
        1,
        "fought the operator by rescheduling"
    );
}

#[test]
fn standing_down_ends_when_the_advice_clears() {
    let mut c = coordinator(FakeLogind::default());
    c.on_decision(&critical());
    c_power(&mut c).pending = None;
    c.on_decision(&critical()); // stood down
    c.on_decision(&on_mains()); // mains back: the episode is over

    assert!(
        matches!(c.on_decision(&critical()), Action::Scheduled { .. }),
        "stayed stood down into the next episode"
    );
}

#[test]
fn a_failed_schedule_is_retried_on_the_next_poll() {
    let mut c = coordinator(FakeLogind {
        fail_schedule: true,
        ..FakeLogind::default()
    });
    assert!(matches!(c.on_decision(&critical()), Action::Failed(_)));
    assert_eq!(
        c.scheduled_at(),
        None,
        "recorded a poweroff that never happened"
    );

    c_power(&mut c).fail_schedule = false;
    assert!(matches!(
        c.on_decision(&critical()),
        Action::Scheduled { .. }
    ));
}

#[test]
fn a_failed_cancel_keeps_trying() {
    let mut c = coordinator(FakeLogind {
        fail_cancel: true,
        ..FakeLogind::default()
    });
    c.on_decision(&critical());
    assert!(matches!(c.on_decision(&on_mains()), Action::Failed(_)));
    assert!(
        c.scheduled_at().is_some(),
        "forgot a poweroff it had failed to cancel"
    );

    c_power(&mut c).fail_cancel = false;
    assert_eq!(c.on_decision(&on_mains()), Action::Cancelled);
}

#[test]
fn dry_run_schedules_nothing_and_publishes_no_shutdown_time() {
    // The published time is what the desktop agent turns into a critical "powering off at
    // ..." notice, so a dry run must not publish one.
    let mut c = ShutdownCoordinator::new(FakeLogind::default(), DELAY, true);
    assert_eq!(c.on_decision(&critical()), Action::WouldSchedule);
    assert_eq!(c.on_decision(&critical()), Action::None);
    assert_eq!(
        c.scheduled_at(),
        None,
        "dry run published a shutdown time nothing scheduled"
    );
    assert_eq!(c.on_decision(&on_mains()), Action::WouldCancel);
    assert_eq!(c_power(&mut c).schedules, 0);
    assert_eq!(c_power(&mut c).cancels, 0);
}

#[test]
fn nothing_scheduled_parses_as_nothing() {
    // Observed on the development machine with no shutdown pending.
    assert_eq!(
        parse_scheduled_shutdown("(st) \"\" 18446744073709551615"),
        None
    );
    assert_eq!(parse_scheduled_shutdown(""), None);
    assert_eq!(parse_scheduled_shutdown("(st) \"poweroff\" 0"), None);
}

#[test]
fn a_scheduled_poweroff_parses_to_its_kind_and_time() {
    let (kind, at) = parse_scheduled_shutdown("(st) \"poweroff\" 1789668727121393").unwrap();
    assert_eq!(kind, "poweroff");
    assert_eq!(
        at.duration_since(UNIX_EPOCH).unwrap().as_secs(),
        1_789_668_727
    );
}
