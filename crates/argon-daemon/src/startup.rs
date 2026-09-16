// SPDX-License-Identifier: GPL-3.0-or-later
//! Deciding whether it is safe to start, and in what mode.

use argon_device::config::Config;
use argon_hal::foreign;
use argon_hal::mode::Mode;
use std::fmt;

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
                "the vendor software is currently driving this hardware ({}).\n\n\
                 Two writers on one MCU corrupt each other's state, so argond will not take \
                 control while it is running. Either leave argond in read-only mode, or stop \
                 those units first:\n\n    sudo systemctl disable --now {}",
                units.join(", "),
                units.join(" ")
            ),
        }
    }
}

/// What the daemon decided to do at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Startup {
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

/// Decides the startup mode from configuration and what else is running.
///
/// A contended machine degrades to read-only rather than refusing to start. The daemon is
/// genuinely useful in read-only mode — status, metrics, discovery — and a service that
/// exits on a condition the operator can fix without it is a service that teaches people to
/// disable it.
#[must_use]
pub fn decide(config: &Config, active_units: &[String]) -> Startup {
    let requested = config.mode().unwrap_or_default();

    if requested == Mode::ReadOnly || active_units.is_empty() {
        return Startup {
            mode: requested,
            requested,
            refusal: None,
        };
    }

    Startup {
        mode: Mode::ReadOnly,
        requested,
        refusal: Some(Refusal::Contended {
            units: active_units.to_vec(),
        }),
    }
}

/// The vendor units currently active on this machine.
#[must_use]
pub fn active_vendor_units() -> Vec<String> {
    foreign::vendor_units()
        .into_iter()
        .filter(foreign::UnitState::is_active)
        .map(|u| u.unit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_mode(mode: &str) -> Config {
        Config::from_toml(&format!("mode = {mode:?}\n")).unwrap()
    }

    #[test]
    fn an_uncontended_machine_gets_the_configured_mode() {
        for mode in ["read-only", "managed", "full"] {
            let d = decide(&config_with_mode(mode), &[]);
            assert_eq!(d.mode.as_str(), mode);
            assert!(!d.degraded());
        }
    }

    #[test]
    fn a_contended_machine_degrades_to_read_only() {
        let units = vec!["argononed.service".to_owned()];
        let d = decide(&config_with_mode("managed"), &units);
        assert_eq!(
            d.mode,
            Mode::ReadOnly,
            "took control while the vendor daemon was running"
        );
        assert_eq!(d.requested, Mode::Managed);
        assert!(d.degraded());
    }

    #[test]
    fn full_mode_degrades_just_as_managed_does() {
        let units = vec!["argonupsrtcd.service".to_owned()];
        let d = decide(&config_with_mode("full"), &units);
        assert_eq!(d.mode, Mode::ReadOnly);
        assert!(d.degraded());
    }

    #[test]
    fn read_only_is_never_degraded_because_it_contends_with_nothing() {
        // Read-only writes no bytes, so another writer is not a conflict. Reporting it as
        // degraded would train operators to ignore the warning.
        let units = vec![
            "argononed.service".to_owned(),
            "argonupsrtcd.service".to_owned(),
        ];
        let d = decide(&config_with_mode("read-only"), &units);
        assert_eq!(d.mode, Mode::ReadOnly);
        assert!(
            !d.degraded(),
            "read-only should not report itself as degraded"
        );
    }

    #[test]
    fn the_refusal_names_the_units_and_how_to_fix_it() {
        // An operator reading this in the journal should not have to look anything up.
        let units = vec![
            "argononed.service".to_owned(),
            "argonupsrtcd.service".to_owned(),
        ];
        let d = decide(&config_with_mode("managed"), &units);
        let msg = d.refusal.unwrap().to_string();
        assert!(msg.contains("argononed.service"), "{msg}");
        assert!(msg.contains("argonupsrtcd.service"), "{msg}");
        assert!(
            msg.contains("systemctl disable --now"),
            "no remedy given: {msg}"
        );
    }

    #[test]
    fn an_unparseable_mode_falls_back_to_read_only() {
        // Config::from_toml would normally reject this, so reaching here means something
        // constructed a Config directly. The fallback must still be the safe one.
        let config = Config {
            mode: "nonsense".to_owned(),
            ..Config::default()
        };
        assert_eq!(decide(&config, &[]).mode, Mode::ReadOnly);
    }
}
