// SPDX-License-Identifier: GPL-3.0-or-later
//! What closing and opening a laptop lid does: the policy, without any I/O.
//!
//! Lid changes and the passing of time go in; [`Effect`]s come out, for `argonctl lid-agent` to
//! carry out in the desktop session. Two behaviours, chosen in `[lid] action`:
//!
//! - **power-save**: while the lid is closed the screen is off, and optionally the radios and
//!   the CPU are turned down. Opening the lid undoes exactly what closing it did.
//! - **shutdown**: closing the lid raises an alert with a sound, and after `shutdown_delay_s`
//!   the machine powers off -- unless the lid was opened in the meantime.
//!
//! Facts: the lid is `ONEUP-GPIO27`, reported to logind by the lid overlay (T20, T21).

use crate::config::LidConfig;
use std::time::Duration;

/// Something to do about the lid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Turn the screen off.
    ScreenOff,
    /// Turn the screen back on -- and wake it: on the ONE UP the panel goes dark with the lid
    /// whatever software does, and the desktop does not bring it back by itself (T22).
    ScreenOn,
    /// Turn off the Wi-Fi and Bluetooth that are on, remembering which.
    RadiosOff,
    /// Turn back on what [`Effect::RadiosOff`] turned off.
    RadiosRestore,
    /// Cap the CPU at its lowest frequency (`true`), or lift the cap (`false`).
    CpuCap(bool),
    /// Tell the user, with a sound, that the machine powers off in this long.
    ShutdownAlert(Duration),
    /// Tell the user the shutdown was called off.
    ShutdownCancelled,
    /// Power off now.
    PowerOff,
}

/// Which behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    PowerSave,
    Shutdown,
}

/// What a power-save close did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Saving {
    radios: bool,
    cpu: bool,
}

/// The lid policy.
#[derive(Debug, Clone)]
pub struct Lid {
    action: Action,
    radios_off: bool,
    cpu_throttle: bool,
    delay: Duration,
    /// The lid state last seen; `None` before the first report.
    closed: Option<bool>,
    /// What closing the lid turned down, so opening it restores exactly that.
    saving: Option<Saving>,
    /// When the poweroff is due, on the caller's clock.
    shutdown_at: Option<Duration>,
    /// Set once the poweroff has been asked for, so it is asked for once.
    powering_off: bool,
}

impl Lid {
    /// A policy from the configuration. The configuration has been validated when loaded.
    #[must_use]
    pub fn new(config: &LidConfig) -> Self {
        Self {
            action: if config.action == "shutdown" {
                Action::Shutdown
            } else {
                Action::PowerSave
            },
            radios_off: config.radios_off,
            cpu_throttle: config.cpu_throttle,
            delay: Duration::from_secs(config.shutdown_delay_s),
            closed: None,
            saving: None,
            shutdown_at: None,
            powering_off: false,
        }
    }

    /// The lid is reported closed (`true`) or open, at `now` on the caller's monotonic clock.
    ///
    /// The first report only sets the starting point: a lid already closed when the agent starts
    /// -- at login, with an external screen -- must not power the machine off.
    pub fn report(&mut self, closed: bool, now: Duration) -> Vec<Effect> {
        let before = self.closed.replace(closed);
        match (before, closed) {
            (Some(false), true) => self.on_close(now),
            (Some(true), false) => self.on_open(),
            _ => Vec::new(),
        }
    }

    /// Time has passed. Returns the poweroff once it is due.
    pub fn tick(&mut self, now: Duration) -> Vec<Effect> {
        match self.shutdown_at {
            Some(at) if now >= at && self.closed == Some(true) && !self.powering_off => {
                self.shutdown_at = None;
                self.powering_off = true;
                vec![Effect::PowerOff]
            }
            _ => Vec::new(),
        }
    }

    /// When [`Lid::tick`] next has something to do.
    #[must_use]
    pub const fn next_deadline(&self) -> Option<Duration> {
        self.shutdown_at
    }

    /// What to undo if the agent stops while the lid is closed: the screen, radios and CPU cap
    /// must not stay down because the agent went away.
    pub fn restore_all(&mut self) -> Vec<Effect> {
        self.shutdown_at = None;
        self.saving.take().map_or_else(Vec::new, Self::undo)
    }

    fn on_close(&mut self, now: Duration) -> Vec<Effect> {
        match self.action {
            Action::PowerSave => {
                let saving = Saving {
                    radios: self.radios_off,
                    cpu: self.cpu_throttle,
                };
                self.saving = Some(saving);
                let mut out = vec![Effect::ScreenOff];
                if saving.radios {
                    out.push(Effect::RadiosOff);
                }
                if saving.cpu {
                    out.push(Effect::CpuCap(true));
                }
                out
            }
            Action::Shutdown => {
                if self.powering_off {
                    return Vec::new();
                }
                self.shutdown_at = Some(now + self.delay);
                vec![Effect::ShutdownAlert(self.delay)]
            }
        }
    }

    fn on_open(&mut self) -> Vec<Effect> {
        // The screen is woken on every open, not only after a power-save close: the panel goes
        // dark with the lid on its own, and a shutdown cancelled by opening the lid would
        // otherwise leave a black screen (T22).
        let mut out = self
            .saving
            .take()
            .map_or_else(|| vec![Effect::ScreenOn], Self::undo);
        if self.shutdown_at.take().is_some() {
            out.push(Effect::ShutdownCancelled);
        }
        out
    }

    fn undo(saving: Saving) -> Vec<Effect> {
        let mut out = vec![Effect::ScreenOn];
        if saving.radios {
            out.push(Effect::RadiosRestore);
        }
        if saving.cpu {
            out.push(Effect::CpuCap(false));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lid(action: &str, radios: bool, cpu: bool, delay: u64) -> Lid {
        Lid::new(&LidConfig {
            action: action.into(),
            radios_off: radios,
            cpu_throttle: cpu,
            shutdown_delay_s: delay,
        })
    }

    const fn s(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn power_save_by_default_turns_only_the_screen_off_and_back_on() {
        let mut l = Lid::new(&LidConfig::default());
        assert!(l.report(false, s(0)).is_empty());
        assert_eq!(l.report(true, s(1)), vec![Effect::ScreenOff]);
        assert!(l.tick(s(100)).is_empty(), "power-save powered off");
        assert_eq!(l.report(false, s(101)), vec![Effect::ScreenOn]);
    }

    #[test]
    fn opening_undoes_exactly_what_closing_did() {
        let mut l = lid("power-save", true, true, 1);
        l.report(false, s(0));
        assert_eq!(
            l.report(true, s(1)),
            vec![Effect::ScreenOff, Effect::RadiosOff, Effect::CpuCap(true)]
        );
        assert_eq!(
            l.report(false, s(2)),
            vec![
                Effect::ScreenOn,
                Effect::RadiosRestore,
                Effect::CpuCap(false)
            ]
        );
    }

    #[test]
    fn stopping_with_the_lid_closed_puts_everything_back() {
        let mut l = lid("power-save", true, true, 1);
        l.report(false, s(0));
        l.report(true, s(1));
        assert_eq!(
            l.restore_all(),
            vec![
                Effect::ScreenOn,
                Effect::RadiosRestore,
                Effect::CpuCap(false)
            ]
        );
        assert!(l.restore_all().is_empty(), "restored twice");
    }

    #[test]
    fn shutdown_alerts_then_powers_off_after_the_delay_once() {
        let mut l = lid("shutdown", false, false, 1);
        l.report(false, s(0));
        assert_eq!(l.report(true, s(10)), vec![Effect::ShutdownAlert(s(1))]);
        assert_eq!(l.next_deadline(), Some(s(11)));
        assert!(l.tick(Duration::from_millis(10_999)).is_empty(), "early");
        assert_eq!(l.tick(s(11)), vec![Effect::PowerOff]);
        assert!(l.tick(s(12)).is_empty(), "asked twice");
        // A bounce of the lid after the poweroff was asked for does not start another.
        l.report(false, s(12));
        assert!(l.report(true, s(13)).is_empty());
    }

    #[test]
    fn opening_the_lid_in_time_cancels_the_shutdown() {
        let mut l = lid("shutdown", false, false, 5);
        l.report(false, s(0));
        l.report(true, s(1));
        assert_eq!(
            l.report(false, s(3)),
            vec![Effect::ScreenOn, Effect::ShutdownCancelled]
        );
        assert!(
            l.tick(s(10)).is_empty(),
            "powered off after the lid was opened"
        );
        assert_eq!(l.next_deadline(), None);
    }

    #[test]
    fn opening_the_lid_always_wakes_the_screen() {
        // shutdown, opened in time: the screen must not stay dark behind the cancellation.
        let mut l = lid("shutdown", false, false, 60);
        l.report(false, s(0));
        l.report(true, s(1));
        assert!(l.report(false, s(2)).contains(&Effect::ScreenOn));
        // A lid closed at start and then opened wakes it too.
        let mut l = Lid::new(&LidConfig::default());
        l.report(true, s(0));
        assert_eq!(l.report(false, s(1)), vec![Effect::ScreenOn]);
    }

    #[test]
    fn a_lid_already_closed_when_the_agent_starts_does_nothing() {
        let mut l = lid("shutdown", false, false, 1);
        assert!(l.report(true, s(0)).is_empty());
        assert!(l.tick(s(60)).is_empty());
        // It takes a real close, after an open, to act.
        l.report(false, s(61));
        assert_eq!(l.report(true, s(62)), vec![Effect::ShutdownAlert(s(1))]);
    }

    #[test]
    fn repeated_reports_of_the_same_state_are_not_changes() {
        let mut l = lid("power-save", true, false, 1);
        l.report(false, s(0));
        l.report(true, s(1));
        assert!(l.report(true, s(2)).is_empty());
        l.report(false, s(3));
        assert!(l.report(false, s(4)).is_empty());
    }

    #[test]
    fn a_zero_delay_powers_off_at_the_next_tick() {
        let mut l = lid("shutdown", false, false, 0);
        l.report(false, s(0));
        assert_eq!(l.report(true, s(5)), vec![Effect::ShutdownAlert(s(0))]);
        assert_eq!(l.tick(s(5)), vec![Effect::PowerOff]);
    }
}
