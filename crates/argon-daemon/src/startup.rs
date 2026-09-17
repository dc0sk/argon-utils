// SPDX-License-Identifier: GPL-3.0-or-later
//! Deciding whether it is safe to start, and in what mode.

use argon_device::config::Config;
use argon_hal::foreign;
use argon_hal::mode::Mode;
use std::fmt;

/// What drives this machine's fan, decided once at startup from what actually answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanPlan {
    /// An Argon MCU acknowledged its address: the fan is ours to drive (Pi 4-era cases).
    Mcu,
    /// No MCU, but the kernel's `pwm-fan` drives the fan. Reported, never driven: taking it
    /// over would mean disabling the thermal zone and its critical trip.
    KernelReportOnly,
    /// Neither.
    NoFan,
}

/// Decides the fan plan.
///
/// On an Argon ONE V5 with a Pi 5 there is no MCU at all. Assuming one there made the daemon's
/// first action a write to an address nothing answers, and the resulting `EREMOTEIO` killed
/// the daemon before UPS monitoring started.
#[must_use]
pub const fn fan_plan(mcu_answers: bool, kernel_fan: bool) -> FanPlan {
    if mcu_answers {
        FanPlan::Mcu
    } else if kernel_fan {
        FanPlan::KernelReportOnly
    } else {
        FanPlan::NoFan
    }
}

/// The vendor unit that drives the Argon MCU, and so contends with us for it.
///
/// Only this one. The vendor's UPS daemons contend for the UPS, which the UPS thread checks
/// for itself; counting them here made the fan subsystem refuse control on a machine where it
/// had nothing to contend over.
pub const MCU_VENDOR_UNIT: &str = "argononed.service";

/// Why the daemon declined to take control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Another program already owns the hardware.
    Contended {
        /// The units found active.
        units: Vec<String>,
    },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Contended { units } => write!(
                f,
                "{} is running and drives the Argon MCU this daemon would drive.\n\n\
                 Two writers on one MCU corrupt each other's state, so argond leaves the fan \
                 alone while it runs. Stop it to let argond drive the fan:\n\n    \
                 sudo systemctl disable --now {}",
                units.join(", "),
                units.join(" ")
            ),
        }
    }
}

/// What the daemon decided to do at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
    /// What drives the fan.
    pub fan: FanPlan,
    /// The mode actually adopted.
    pub mode: Mode,
    /// The mode requested by configuration.
    pub requested: Mode,
    /// Why the requested mode was not adopted, if it was not.
    pub refusal: Option<Refusal>,
}

impl Startup {
    /// Whether the daemon fell back to a safer mode than configured.
    #[must_use]
    pub const fn degraded(&self) -> bool {
        self.refusal.is_some()
    }
}

/// Decides the fan subsystem's mode from configuration, the fan plan, and what else is running.
///
/// A contended MCU degrades to read-only rather than refusing to start. The daemon is
/// genuinely useful without driving the fan -- UPS monitoring, status -- and a service that
/// exits on a condition the operator can fix without it is a service that teaches people to
/// disable it.
///
/// Contention only exists when there is an MCU to contend for. Without one, nothing here writes
/// to the fan, so the vendor's fan daemon running or not makes no difference.
#[must_use]
pub fn decide(config: &Config, active_units: &[String], fan: FanPlan) -> Startup {
    let requested = config.mode().unwrap_or_default();
    let contended: Vec<String> = active_units
        .iter()
        .filter(|u| u.as_str() == MCU_VENDOR_UNIT)
        .cloned()
        .collect();

    if fan != FanPlan::Mcu || requested == Mode::ReadOnly || contended.is_empty() {
        return Startup {
            fan,
            mode: requested,
            requested,
            refusal: None,
        };
    }

    Startup {
        fan,
        mode: Mode::ReadOnly,
        requested,
        refusal: Some(Refusal::Contended { units: contended }),
    }
}

/// The vendor units that must be assumed to be running on this machine.
///
/// Includes units whose state could not be read at all. A `systemctl` that cannot be run is
/// not evidence that the vendor daemon is absent, and this list gates both fan writes and the
/// UPS poweroff -- so it fails closed, towards read-only and dry run.
#[must_use]
pub fn active_vendor_units() -> Vec<String> {
    foreign::vendor_units()
        .into_iter()
        .filter(|u| {
            if u.is_unknown() {
                eprintln!(
                    "argond: cannot tell whether {} is running; assuming it is",
                    u.unit
                );
            }
            u.contends()
        })
        .map(|u| u.unit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_mode(mode: &str) -> Config {
        Config::from_toml(&format!("mode = {mode:?}\n")).unwrap()
    }

    fn units(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn the_fan_plan_follows_what_answers() {
        assert_eq!(
            fan_plan(true, true),
            FanPlan::Mcu,
            "an MCU wins even with a kernel fan"
        );
        assert_eq!(fan_plan(false, true), FanPlan::KernelReportOnly);
        assert_eq!(fan_plan(false, false), FanPlan::NoFan);
    }

    #[test]
    fn an_uncontended_machine_gets_the_configured_mode() {
        for mode in ["read-only", "managed", "full"] {
            let d = decide(&config_with_mode(mode), &[], FanPlan::Mcu);
            assert_eq!(d.mode.as_str(), mode);
            assert!(!d.degraded());
        }
    }

    #[test]
    fn a_contended_mcu_degrades_to_read_only() {
        let d = decide(
            &config_with_mode("managed"),
            &units(&["argononed.service"]),
            FanPlan::Mcu,
        );
        assert_eq!(
            d.mode,
            Mode::ReadOnly,
            "took control while the vendor daemon was running"
        );
        assert_eq!(d.requested, Mode::Managed);
        assert!(d.degraded());
    }

    #[test]
    fn without_an_mcu_the_vendor_fan_daemon_is_not_contention() {
        // The ONE V5 case that shipped broken: no MCU, argononed running. There is nothing to
        // contend over, so nothing to degrade and nothing to warn about.
        for plan in [FanPlan::KernelReportOnly, FanPlan::NoFan] {
            let d = decide(
                &config_with_mode("full"),
                &units(&["argononed.service"]),
                plan,
            );
            assert_eq!(d.mode, Mode::Full);
            assert!(!d.degraded(), "degraded with plan {plan:?}");
            assert!(
                d.refusal.is_none(),
                "warned about an MCU that does not exist"
            );
        }
    }

    #[test]
    fn the_vendor_ups_daemons_are_not_mcu_contention() {
        // They contend for the UPS, which the UPS thread checks for itself.
        let active = units(&["argononeupsd.service", "argonupsrtcd.service"]);
        let d = decide(&config_with_mode("full"), &active, FanPlan::Mcu);
        assert_eq!(d.mode, Mode::Full);
        assert!(d.refusal.is_none());
    }

    #[test]
    fn read_only_is_never_degraded_because_it_contends_with_nothing() {
        let d = decide(
            &config_with_mode("read-only"),
            &units(&["argononed.service"]),
            FanPlan::Mcu,
        );
        assert_eq!(d.mode, Mode::ReadOnly);
        assert!(
            !d.degraded(),
            "read-only should not report itself as degraded"
        );
    }

    #[test]
    fn the_refusal_names_the_unit_and_how_to_fix_it() {
        let d = decide(
            &config_with_mode("managed"),
            &units(&["argononed.service"]),
            FanPlan::Mcu,
        );
        let msg = d.refusal.unwrap().to_string();
        assert!(msg.contains("argononed.service"), "{msg}");
        assert!(
            msg.contains("systemctl disable --now"),
            "no remedy given: {msg}"
        );
        assert!(
            !msg.contains("argonupsrtcd"),
            "blamed a UPS daemon for MCU contention: {msg}"
        );
    }

    #[test]
    fn an_unparseable_mode_falls_back_to_read_only() {
        let config = Config {
            mode: "nonsense".to_owned(),
            ..Config::default()
        };
        assert_eq!(decide(&config, &[], FanPlan::Mcu).mode, Mode::ReadOnly);
    }
}
