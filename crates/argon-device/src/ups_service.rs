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
        missing: None,
    };
    Cycle {
        poll,
        action,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The units the packaged service stops by `Conflicts=`.
    fn unit_conflicts() -> Vec<String> {
        let unit = include_str!("../../../packaging/systemd/argond.service");
        unit.lines()
            .filter_map(|l| l.strip_prefix("Conflicts="))
            .flat_map(|v| v.split_whitespace().map(str::to_owned))
            .collect()
    }

    #[test]
    fn the_sandbox_admits_every_device_class_argond_opens() {
        // The device cgroup refuses an open before file permissions are looked at, so a device
        // class missing here fails in production and nowhere else: the tests run unsandboxed.
        // That is how the case button first shipped -- the argon user had the gpio group, and
        // argond still reported "no header GPIO chip found".
        let unit = include_str!("../../../packaging/systemd/argond.service");
        let allowed: Vec<&str> = unit
            .lines()
            .filter_map(|l| l.strip_prefix("DeviceAllow="))
            .filter_map(|v| v.split_whitespace().next())
            .collect();
        for class in [
            "char-i2c",      // the case MCU, the OLED, the ONE UP's fuel gauge
            "char-ttyACM",   // the PWR UPS serial link
            "char-hidraw",   // the PWR UPS HID interface
            "char-gpiochip", // the case button
        ] {
            assert!(
                allowed.contains(&class),
                "{class} missing from DeviceAllow="
            );
        }
    }

    #[test]
    fn the_service_conflicts_with_exactly_the_ups_vendor_units() {
        // Conflicts= stops a unit when argond starts, with nothing recorded to restore it. Only
        // the UPS daemons the package retires and restores may be listed -- not argononed, and
        // not argononeupd, which also holds the ONE UP's lid.
        let mut got = unit_conflicts();
        got.sort();
        let mut want: Vec<String> = UPS_VENDOR_UNITS.iter().map(|u| (*u).to_owned()).collect();
        want.sort();
        assert_eq!(got, want);
        for u in ONEUP_VENDOR_UNITS {
            assert!(!got.iter().any(|g| g == u), "{u} is stopped on install");
        }
    }
}
