// SPDX-License-Identifier: GPL-3.0-or-later
//! One UPS monitoring cycle: read, decide, act, publish.
//!
//! Kept as a single step with no sleeping and no threads, so the whole chain -- from a battery
//! reading to a scheduled poweroff to the status file an agent reads -- can be tested end to
//! end against a scripted UPS and a fake logind.

use crate::power::{Action, PowerControl, ShutdownCoordinator};
use crate::status::{UpsStatus, level_name};
use crate::ups::{Monitor, Poll};
use std::time::{Duration, SystemTime};

/// The vendor units that contend for the UPS.
///
/// `argonupsrtcd` holds the serial port. `argononeupsd` does not touch the port, but it runs
/// its own `shutdown +1` / `shutdown -c` logic from the vendor's status log, so leaving it
/// running would mean two programs scheduling and cancelling shutdowns on the same machine.
pub const UPS_VENDOR_UNITS: &[&str] = &["argonupsrtcd.service", "argononeupsd.service"];

/// The vendor unit responsible for the ONE UP's battery (and its lid).
///
/// Unlike the PWR UPS's serial port, the gauge's I2C bus arbitrates between readers, so reading
/// alongside it is safe. Acting on the battery alongside it is not: two programs would each
/// decide when to power off.
pub const ONEUP_VENDOR_UNITS: &[&str] = &["argononeupd.service"];

/// Which of the given active units contend for the UPS.
#[must_use]
pub fn contention(active_units: &[String]) -> Vec<String> {
    contention_among(UPS_VENDOR_UNITS, active_units)
}

/// Which of the given active units contend for the ONE UP's battery.
#[must_use]
pub fn oneup_contention(active_units: &[String]) -> Vec<String> {
    contention_among(ONEUP_VENDOR_UNITS, active_units)
}

fn contention_among(units: &[&str], active_units: &[String]) -> Vec<String> {
    active_units
        .iter()
        .filter(|u| units.contains(&u.as_str()))
        .cloned()
        .collect()
}

/// The outcome of one cycle.
#[derive(Debug)]
pub struct Cycle {
    /// What was read and decided.
    pub poll: Poll,
    /// What the coordinator did about it.
    pub action: Action,
    /// The status to publish.
    pub status: UpsStatus,
}

/// Runs one monitoring cycle.
pub fn step<M: Monitor, P: PowerControl>(
    monitor: &mut M,
    coordinator: &mut ShutdownCoordinator<P>,
    uptime: Duration,
    now: SystemTime,
) -> Cycle {
    let poll = monitor.poll(uptime);
    let action = coordinator.on_decision(&poll.decision);
    let status = UpsStatus {
        updated: now,
        level: level_name(poll.decision.level).to_owned(),
        percent: poll.battery.map(|b| b.percent),
        shutdown_at: coordinator.scheduled_at(),
    };
    Cycle {
        poll,
        action,
        status,
    }
}
