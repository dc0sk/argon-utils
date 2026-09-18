// SPDX-License-Identifier: GPL-3.0-or-later
//! "Power off and wake": what the tray offers, and asking argond for it over D-Bus.
//!
//! argond does the work and polkit decides who may ask; the tray only offers the choice when the
//! answer can be yes, and reports what came back.

use argon_device::control::{BUS_NAME, INTERFACE, OBJECT_PATH};
use std::process::Command;

/// One offered wake time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    /// The menu label.
    pub label: String,
    /// When to wake, unix seconds.
    pub at_unix: u64,
}

/// Whether to offer "Power off and wake" at all.
///
/// Only when polkit could say yes -- "yes", or "challenge" meaning after a password -- and not
/// while a poweroff is already scheduled: a second one would replace it, and a wake set now
/// would come with a poweroff the user did not choose.
#[must_use]
pub fn offer(can: Option<&str>, shutdown_pending: bool) -> bool {
    !shutdown_pending && matches!(can, Some("yes" | "challenge"))
}

/// The presets offered, given now and the next 07:00 (both unix seconds).
///
/// Every one is well past argond's 15-minute minimum lead, so none can be refused for being too
/// soon. The 07:00 preset is left out when that 07:00 comes before "in 8 hours" -- late in the
/// evening, say -- since it would then sit oddly after it in the menu and add nothing -- and when
/// `next_seven` could not work it out.
#[must_use]
pub fn presets(now_unix: u64, next_seven: Option<(u64, String)>) -> Vec<Preset> {
    let mut out = vec![
        Preset {
            label: "…and wake in 1 hour".into(),
            at_unix: now_unix + 3_600,
        },
        Preset {
            label: "…and wake in 8 hours".into(),
            at_unix: now_unix + 8 * 3_600,
        },
    ];
    if let Some((at, when)) = next_seven {
        if at > now_unix + 8 * 3_600 {
            out.push(Preset {
                label: format!("…and wake {when} at 07:00"),
                at_unix: at,
            });
        }
    }
    out
}

/// The next local 07:00 more than 15 minutes away, and whether it is "today" or "tomorrow".
#[must_use]
pub fn next_seven(now_unix: u64) -> Option<(u64, String)> {
    for (spec, word) in [("today 07:00", "today"), ("tomorrow 07:00", "tomorrow")] {
        let at = date_to_unix(spec)?;
        if at > now_unix + 15 * 60 {
            return Some((at, word.into()));
        }
    }
    None
}

fn date_to_unix(spec: &str) -> Option<u64> {
    let out = Command::new("date")
        .args(["-d", spec, "+%s"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

/// Asks argond whether this user may power off with a wake. `None` when argond is not on the bus.
#[must_use]
pub fn can() -> Option<String> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).ok()?;
    proxy.call("CanPoweroffWithWake", &()).ok()
}

/// Asks argond to power off and wake at `at_unix`. Returns the wake as the UPS holds it and the
/// poweroff time, or argond's reason for refusing.
///
/// # Errors
///
/// argond refused, polkit refused, or argond could not be reached.
pub fn poweroff_with_wake(at_unix: u64) -> Result<(u64, u64), String> {
    let conn = zbus::blocking::Connection::system().map_err(|e| e.to_string())?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE)
        .map_err(|e| e.to_string())?;
    proxy
        .call("PoweroffWithWake", &(at_unix,))
        .map_err(|e| match e {
            zbus::Error::MethodError(_, Some(msg), _) => msg,
            other => other.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_789_740_000;

    #[test]
    fn offered_only_when_polkit_could_say_yes_and_nothing_is_pending() {
        assert!(offer(Some("yes"), false));
        assert!(offer(Some("challenge"), false));
        assert!(!offer(Some("no"), false));
        assert!(
            !offer(None, false),
            "offered while argond is not on the bus"
        );
        assert!(
            !offer(Some("yes"), true),
            "offered on top of a scheduled poweroff"
        );
    }

    #[test]
    fn every_preset_clears_argonds_minimum_lead() {
        let tomorrow = Some((NOW + 20 * 3_600, "tomorrow".to_owned()));
        for p in presets(NOW, tomorrow) {
            assert!(
                p.at_unix >= NOW + argon_device::wake::MIN_LEAD.as_secs() + 60,
                "{p:?} would be refused as too soon"
            );
        }
    }

    #[test]
    fn a_seven_oclock_sooner_than_the_fixed_presets_is_not_offered() {
        // It would read as "tomorrow" and come before "in 8 hours": confusing, and redundant.
        let soon = Some((NOW + 2 * 3_600, "today".to_owned()));
        assert_eq!(presets(NOW, soon).len(), 2);
        assert_eq!(presets(NOW, None).len(), 2);
        let later = Some((NOW + 20 * 3_600, "tomorrow".to_owned()));
        let all = presets(NOW, later);
        assert_eq!(all.len(), 3);
        assert!(
            all[2].label.contains("tomorrow at 07:00"),
            "{}",
            all[2].label
        );
    }

    #[test]
    fn the_next_seven_is_in_the_future_and_within_two_days() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (at, word) = next_seven(now).expect("date could not work out 07:00");
        assert!(at > now + 15 * 60 && at < now + 48 * 3_600, "{word}: {at}");
    }
}
