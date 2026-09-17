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
//!   it is left alone and not claimed.
//! - **An operator's cancellation is respected.** If the shutdown we scheduled disappears
//!   while the advice still stands, someone ran `shutdown -c`. Scheduling it again on the next
//!   poll would fight them, so we stand down until the advice clears.

use argon_hal::{Error, Result};
use argon_proto::ups::policy::Advice;
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
        run("shutdown", &["--poweroff", &format!("+{minutes}"), message])?;
        Ok(self
            .pending()?
            .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(minutes * 60)))
    }

    fn cancel(&mut self) -> Result<()> {
        run("shutdown", &["-c"])
    }

    fn pending(&mut self) -> Result<Option<SystemTime>> {
        let out = Command::new("busctl")
            .args([
                "get-property",
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                "ScheduledShutdown",
            ])
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Io(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            )));
        }
        Ok(parse_scheduled_shutdown(&String::from_utf8_lossy(&out.stdout)).map(|(_, at)| at))
    }
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

/// Turns battery advice into at most one scheduled poweroff at a time.
pub struct ShutdownCoordinator<P: PowerControl> {
    power: P,
    delay: Duration,
    dry_run: bool,
    /// The poweroff we scheduled, if any.
    ours: Option<SystemTime>,
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

    /// Acts on one piece of advice.
    pub fn on_advice(&mut self, advice: Advice) -> Action {
        if advice == Advice::Shutdown {
            self.advised()
        } else {
            self.cleared()
        }
    }

    fn advised(&mut self) -> Action {
        if self.stood_down {
            return Action::None;
        }

        if self.ours.is_some() {
            if self.dry_run {
                return Action::None;
            }
            // Still pending? If it has vanished, an operator cancelled it.
            return match self.power.pending() {
                Ok(None) => {
                    self.ours = None;
                    self.stood_down = true;
                    Action::OverriddenByOperator
                }
                // A read failure is not evidence of cancellation; assume it still stands.
                Ok(Some(_)) | Err(_) => Action::None,
            };
        }

        if self.dry_run {
            self.ours = Some(SystemTime::now() + self.delay);
            return Action::WouldSchedule;
        }

        if let Ok(Some(at)) = self.power.pending() {
            return Action::AlreadyPending { at };
        }

        match self.power.schedule_poweroff(self.delay, SHUTDOWN_MESSAGE) {
            Ok(at) => {
                self.ours = Some(at);
                Action::Scheduled { at }
            }
            Err(e) => Action::Failed(e.to_string()),
        }
    }

    fn cleared(&mut self) -> Action {
        self.stood_down = false;
        if self.ours.is_none() {
            return Action::None;
        }
        if self.dry_run {
            self.ours = None;
            return Action::WouldCancel;
        }
        match self.power.cancel() {
            Ok(()) => {
                self.ours = None;
                Action::Cancelled
            }
            Err(e) => Action::Failed(e.to_string()),
        }
    }
}
