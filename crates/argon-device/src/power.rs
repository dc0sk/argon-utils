// SPDX-License-Identifier: GPL-3.0-or-later
//! Acting on shutdown advice: a delayed, cancellable poweroff through logind.
//!
//! The battery policy decides *whether* shutdown is advised. This module decides what to do
//! about it, and the rules are about not surprising anyone:
//!
//! - **Delayed, never immediate.** A scheduled poweroff leaves a window in which mains coming
//!   back cancels it, and in which an operator can see it coming.
//! - **Scheduled once.** The policy's advice is level-triggered and repeats on every poll; the
//!   action is not. Rescheduling on every poll would keep pushing the shutdown back.
//! - **Only our own shutdowns are cancelled.** If someone else already has a shutdown pending,
//!   it is left alone and not claimed. "Ours" means the pending time matches the one we
//!   placed, not merely that we once placed one: an operator who cancels ours and schedules
//!   their own must not have theirs cancelled by us later.
//! - **An operator's cancellation is respected.** If the shutdown we scheduled disappears
//!   while the advice still stands, someone ran `shutdown -c`. Scheduling it again on the next
//!   poll would fight them, so we stand down until the advice clears.
//! - **Only a confirmed recovery cancels.** A failed reading, or a battery gauge wobbling
//!   upward while still on battery, leaves a placed poweroff alone. Losing a reading is not
//!   the same as mains returning, and treating it as such is how a machine on a dying
//!   battery never powers off at all.
//!
//! # Why ownership is tracked against what logind reports
//!
//! `Logind::schedule_poweroff` is two operations: run `shutdown`, then read the deadline back.
//! The first can succeed while the second fails. An earlier version returned that read failure
//! as a scheduling failure, which left a *real* poweroff pending that this coordinator did not
//! know about -- so it was never cancelled when mains returned, and the machine powered off on
//! mains with nothing in the status file to warn anyone. Every branch below therefore
//! distinguishes "nothing is pending" from "cannot tell", and never treats the second as the
//! first.

use argon_hal::{Error, Result};
use argon_proto::ups::policy::{Advice, Decision, Level};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Something that can schedule and cancel a poweroff.
pub trait PowerControl {
    /// Schedules a poweroff after `delay`, returning when it will happen.
    ///
    /// # Errors
    ///
    /// Fails if the request is refused or cannot be made.
    fn schedule_poweroff(&mut self, delay: Duration, message: &str) -> Result<SystemTime>;

    /// Cancels a pending poweroff.
    ///
    /// # Errors
    ///
    /// Fails if the request is refused or cannot be made.
    fn cancel(&mut self) -> Result<()>;

    /// When a pending shutdown will happen, if one is pending.
    ///
    /// # Errors
    ///
    /// Fails if the state cannot be read.
    fn pending(&mut self) -> Result<Option<SystemTime>>;

    /// The wall message attached to the pending shutdown, if it can be read.
    ///
    /// This is how a restarted daemon recognises its own poweroff. Without it, a poweroff
    /// placed before the restart looks like a stranger's: it would never be cancelled when
    /// mains returned, and `dpkg` restarts this service on every package upgrade.
    ///
    /// # Errors
    ///
    /// Fails if the state cannot be read.
    fn wall_message(&mut self) -> Result<Option<String>>;
}

/// Poweroff through systemd-logind, via the `shutdown` command.
///
/// `shutdown` on a systemd machine is systemctl talking to logind: it schedules a real poweroff
/// that `shutdown -c` cancels, and logind broadcasts its own notice to logged-in terminals.
/// A process that is not in an active session needs a polkit rule to be allowed to do this.
#[derive(Debug, Default)]
pub struct Logind;

impl PowerControl for Logind {
    fn schedule_poweroff(&mut self, delay: Duration, message: &str) -> Result<SystemTime> {
        // `shutdown` takes whole minutes. Round up, and never below one: "+0" is immediate,
        // which is exactly what this module exists not to do.
        let minutes = delay.as_secs().div_ceil(60).max(1);
        let intended = SystemTime::now() + Duration::from_secs(minutes * 60);
        run("shutdown", &["--poweroff", &format!("+{minutes}"), message])?;
        Ok(scheduled_time(self.pending(), intended))
    }

    fn cancel(&mut self) -> Result<()> {
        run("shutdown", &["-c"])
    }

    fn wall_message(&mut self) -> Result<Option<String>> {
        let text = get_property("WallMessage")?;
        // A string property comes back as `s "..."`.
        let trimmed = text.trim().strip_prefix('s').map_or("", str::trim);
        let message = trimmed.trim_matches('"');
        Ok((!message.is_empty()).then(|| message.to_owned()))
    }

    fn pending(&mut self) -> Result<Option<SystemTime>> {
        let out = get_property("ScheduledShutdown")?;
        Ok(parse_scheduled_shutdown(&out).map(|(_, at)| at))
    }
}

/// Reads one property of logind's manager object.
fn get_property(name: &str) -> Result<String> {
    let out = Command::new("busctl")
        .args([
            "get-property",
            "org.freedesktop.login1",
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            name,
        ])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Io(std::io::Error::other(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Resolves what to report after `shutdown` has already run.
///
/// The command has run, so logind really does have a poweroff scheduled: a failed read-back
/// must not surface as a failure to schedule. That is what orphaned a real poweroff, because
/// the caller then had no record of it to cancel when mains came back. Falling back to the
/// time we asked for is within a minute of the truth.
fn scheduled_time(readback: Result<Option<SystemTime>>, intended: SystemTime) -> SystemTime {
    readback.ok().flatten().unwrap_or(intended)
}

/// Parses `busctl get-property ... ScheduledShutdown` output.
///
/// With nothing scheduled, logind reports an empty kind and `u64::MAX` -- observed on the
/// development machine as `(st) "" 18446744073709551615`. Returns the shutdown kind and time
/// otherwise.
#[must_use]
pub fn parse_scheduled_shutdown(output: &str) -> Option<(String, SystemTime)> {
    let rest = output.trim().strip_prefix("(st)")?.trim();
    let (kind, usec) = rest.rsplit_once(' ')?;
    let kind = kind.trim().trim_matches('"').to_owned();
    let usec: u64 = usec.trim().parse().ok()?;
    if kind.is_empty() || usec == 0 || usec == u64::MAX {
        return None;
    }
    Some((kind, UNIX_EPOCH + Duration::from_micros(usec)))
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Io(std::io::Error::other(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))))
    }
}

/// What the coordinator did with one decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing needed doing.
    None,
    /// A poweroff was scheduled.
    Scheduled {
        /// When it will happen.
        at: SystemTime,
    },
    /// A poweroff we had placed before a restart was recognised and taken back over.
    Adopted {
        /// When it will happen.
        at: SystemTime,
    },
    /// Shutdown is advised, but someone else already has one pending. Left alone.
    AlreadyPending {
        /// When theirs will happen.
        at: SystemTime,
    },
    /// Our pending poweroff was cancelled because the advice cleared.
    Cancelled,
    /// Our poweroff was cancelled by someone else while advice still stood. Standing down.
    OverriddenByOperator,
    /// Dry run: a poweroff would have been scheduled.
    WouldSchedule,
    /// Dry run: a poweroff would have been cancelled.
    WouldCancel,
    /// Scheduling or cancelling failed. Retried on the next poll.
    Failed(String),
}

/// What logind has pending, as far as we can tell.
///
/// The third case is the point of this type: "cannot tell" is not "nothing".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Nothing is scheduled.
    Nothing,
    /// A shutdown is scheduled for this time.
    At(SystemTime),
    /// The state could not be read.
    Unknown,
}

/// How far apart two shutdown times may be and still be the same shutdown.
///
/// `shutdown` takes whole minutes, so logind's deadline is that rounding away from the moment
/// the command ran, and when the read-back fails all we know is what we asked for. Wide enough
/// to cover that, far narrower than any plausible second scheduler's time.
const SAME_SHUTDOWN_TOLERANCE: Duration = Duration::from_secs(90);

/// Whether two reported shutdown times are the same shutdown.
fn same_shutdown(a: SystemTime, b: SystemTime) -> bool {
    a.duration_since(b)
        .or_else(|_| b.duration_since(a))
        .is_ok_and(|d| d <= SAME_SHUTDOWN_TOLERANCE)
}

/// Turns battery advice into at most one scheduled poweroff at a time.
pub struct ShutdownCoordinator<P: PowerControl> {
    power: P,
    delay: Duration,
    dry_run: bool,
    /// The poweroff we scheduled, if any. Never set in dry run: it is published to the status
    /// file, and a status file advertising a poweroff that was never scheduled made the
    /// desktop agent raise a critical "powering off at ..." notice in dry run.
    ours: Option<SystemTime>,
    /// The poweroff a dry run would have scheduled. Keeps `WouldSchedule` from repeating on
    /// every poll, without being mistaken for a real one.
    dry_pending: Option<SystemTime>,
    /// Set when an operator cancelled ours; cleared when the advice clears.
    stood_down: bool,
}

/// The message logind broadcasts, and that the desktop agent shows.
pub const SHUTDOWN_MESSAGE: &str =
    "UPS battery critical: powering off to protect the filesystem. Restore mains power to cancel.";

impl<P: PowerControl> ShutdownCoordinator<P> {
    /// Creates a coordinator. With `dry_run`, it reports what it would do and calls nothing.
    pub const fn new(power: P, delay: Duration, dry_run: bool) -> Self {
        Self {
            power,
            delay,
            dry_run,
            ours: None,
            dry_pending: None,
            stood_down: false,
        }
    }

    /// The power control, for inspection.
    pub const fn power_mut(&mut self) -> &mut P {
        &mut self.power
    }

    /// When the poweroff we scheduled will happen, if we have one pending.
    pub const fn scheduled_at(&self) -> Option<SystemTime> {
        self.ours
    }

    /// Acts on one decision.
    ///
    /// Takes the whole decision, not just the advice, because `Advice::None` covers two
    /// situations that must not be treated alike: mains is back, and we have no idea. Only the
    /// first cancels a poweroff.
    pub fn on_decision(&mut self, decision: &Decision) -> Action {
        match (decision.advice, decision.level) {
            (Advice::Shutdown, _) => self.advised(),
            // Mains is back, and has been for long enough to believe it. An unconfirmed
            // mains reading falls through to the hold below: on a flapping supply it is
            // followed by another outage, and cancelling on each blip means the poweroff is
            // pushed away for as long as the flapping lasts.
            (_, Level::OnMains) if decision.confirmed_recovery => self.recovered(),
            // Everything else -- a failed read, a gauge wobbling up while still on battery, or
            // the post-boot hold -- leaves a placed poweroff exactly where it is.
            _ => Self::holding(),
        }
    }

    fn advised(&mut self) -> Action {
        if self.stood_down {
            return Action::None;
        }

        if self.dry_run {
            return if self.dry_pending.is_some() {
                Action::None
            } else {
                self.dry_pending = Some(SystemTime::now() + self.delay);
                Action::WouldSchedule
            };
        }

        if let Some(mine) = self.ours {
            return match self.pending_state() {
                // Ours, still standing. Nothing to do -- and in particular not rescheduling,
                // which would push the poweroff further away on every poll.
                Pending::At(at) if same_shutdown(mine, at) => Action::None,
                // Something else is pending in its place: an operator cancelled ours and
                // scheduled their own. Theirs is not ours to cancel or to replace.
                Pending::At(at) => {
                    self.ours = None;
                    self.stood_down = true;
                    Action::AlreadyPending { at }
                }
                Pending::Nothing => {
                    self.ours = None;
                    self.stood_down = true;
                    Action::OverriddenByOperator
                }
                // Not evidence of anything. Assume ours still stands rather than scheduling a
                // second one.
                Pending::Unknown => Action::None,
            };
        }

        match self.pending_state() {
            // Pending, but we have no record of placing it. It may still be ours: a restart
            // clears the record, and dpkg restarts this service on every upgrade. logind's
            // wall message settles it -- ours carries SHUTDOWN_MESSAGE.
            Pending::At(at) => {
                if self.wall_message_is_ours() {
                    self.ours = Some(at);
                    return Action::Adopted { at };
                }
                // Genuinely someone else's. Left alone: the machine is going down either way,
                // and claiming it would mean cancelling theirs later.
                Action::AlreadyPending { at }
            }
            // Nothing pending, or no way to tell. Scheduling wins over not scheduling: the
            // battery is confirmed critical, and an unprotected filesystem is the worse
            // outcome. `shutdown` replaces any existing schedule, so ownership is recorded
            // either way.
            Pending::Nothing | Pending::Unknown => {
                match self.power.schedule_poweroff(self.delay, SHUTDOWN_MESSAGE) {
                    Ok(at) => {
                        self.ours = Some(at);
                        Action::Scheduled { at }
                    }
                    Err(e) => Action::Failed(e.to_string()),
                }
            }
        }
    }

    /// Mains is back: cancel a poweroff, if the one pending is still ours.
    fn recovered(&mut self) -> Action {
        self.stood_down = false;

        if self.dry_run {
            return if self.dry_pending.take().is_some() {
                Action::WouldCancel
            } else {
                Action::None
            };
        }

        let Some(mine) = self.ours else {
            return Action::None;
        };

        // A shutdown that is no longer the one we placed is not ours to cancel. Forgetting it
        // here is what stops a refused cancellation from wedging the coordinator: without
        // this, `ours` stayed set forever and every later critical episode saw "something is
        // pending" and never scheduled again.
        if let Pending::At(at) = self.pending_state() {
            if !same_shutdown(mine, at) {
                self.ours = None;
                return Action::None;
            }
        }

        match self.power.cancel() {
            Ok(()) => {
                self.ours = None;
                Action::Cancelled
            }
            Err(e) => Action::Failed(e.to_string()),
        }
    }

    /// Neither advised nor recovered: leave whatever is pending alone.
    ///
    /// Deliberately a named branch rather than a bare `Action::None` at the match arm, so
    /// that "does nothing" reads as a decision. It is the arm a failed reading takes, and
    /// making that arm cancel was a real defect.
    const fn holding() -> Action {
        Action::None
    }

    /// Whether the pending shutdown carries our wall message.
    ///
    /// A failure to read, or an empty message, answers "not ours": adopting on a guess would
    /// mean cancelling a shutdown somebody else placed.
    fn wall_message_is_ours(&mut self) -> bool {
        self.power
            .wall_message()
            .ok()
            .flatten()
            .is_some_and(|m| m.contains(SHUTDOWN_MESSAGE))
    }

    fn pending_state(&mut self) -> Pending {
        match self.power.pending() {
            Ok(Some(at)) => Pending::At(at),
            Ok(None) => Pending::Nothing,
            Err(_) => Pending::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTENDED: SystemTime = UNIX_EPOCH;

    #[test]
    fn a_failed_readback_still_reports_a_scheduled_time() {
        // The rule this pins: `shutdown` already ran. Reporting an error here made the
        // coordinator disown a poweroff that logind was really holding.
        let err = Err(Error::Io(std::io::Error::other("busctl: no bus")));
        assert_eq!(scheduled_time(err, INTENDED), INTENDED);
    }

    #[test]
    fn an_empty_readback_still_reports_a_scheduled_time() {
        // logind reporting nothing right after we scheduled is a race, not a cancellation.
        assert_eq!(scheduled_time(Ok(None), INTENDED), INTENDED);
    }

    #[test]
    fn a_readback_that_worked_is_preferred_over_our_estimate() {
        let real = UNIX_EPOCH + Duration::from_secs(1_789_668_727);
        assert_eq!(scheduled_time(Ok(Some(real)), INTENDED), real);
    }

    #[test]
    fn times_within_the_tolerance_are_the_same_shutdown() {
        let base = UNIX_EPOCH + Duration::from_secs(2_000_000_000);
        assert!(same_shutdown(base, base));
        assert!(same_shutdown(base, base + Duration::from_secs(59)));
        assert!(same_shutdown(base + Duration::from_secs(59), base));
        assert!(!same_shutdown(base, base + Duration::from_secs(3600)));
        assert!(!same_shutdown(base + Duration::from_secs(3600), base));
    }
}
