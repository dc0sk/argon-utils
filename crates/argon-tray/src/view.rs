// SPDX-License-Identifier: GPL-3.0-or-later
//! What the tray shows, as a pure function of what was read.
//!
//! No D-Bus, no filesystem and no clock in here, so every rule about what the icon says --
//! which is the whole point of the tray -- can be tested without a desktop.

use argon_device::status::{LevelName, Reading, UpsStatus, interpret};
use argon_hal::fan_hwmon::FanReading;
use std::time::SystemTime;

/// Everything the tray read on one poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The status `argond` publishes, if the file could be read and parsed.
    pub ups: Option<UpsStatus>,
    /// When this snapshot was taken.
    pub now: SystemTime,
    /// CPU temperature in tenths of a degree, if a sensor was found.
    pub cpu_decicelsius: Option<i32>,
    /// The kernel fan, if there is one.
    pub fan: Option<FanReading>,
    /// Which icon set to name.
    pub icons: Icons,
    /// What the daemon says about battery monitoring, when there is no published status.
    pub monitoring: Monitoring,
}

/// What argond is monitoring, asked over D-Bus when no status file is being published.
///
/// Without this the tray cannot tell a stopped daemon from a healthy one with nothing to
/// report, and it accused the first of the second: "argond is not publishing status. Is it
/// running?" on a machine where argond was running exactly as configured.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Monitoring {
    /// The daemon could not be reached, so it may genuinely be stopped.
    #[default]
    Unknown,
    /// The daemon answered: it is monitoring nothing, by configuration.
    Off,
    /// The daemon answered: it monitors this source, but has published nothing yet.
    Source(String),
}

/// Which battery icons to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icons {
    /// The freedesktop `battery-level-N-symbolic` set: eleven levels, but drawn in one dark
    /// colour that a panel may not recolour -- nearly invisible on a dark panel, as the Raspberry
    /// Pi panel showed.
    Symbolic,
    /// The full-colour legacy set (`battery-good`, `battery-low-charging`, ...): five levels, and
    /// visible on light and dark panels alike. Needs a theme that has them (`AdwaitaLegacy`).
    Colour,
}

/// How loudly the tray should present itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Normal: shown, nothing to act on.
    Normal,
    /// Something the user should look at now.
    Attention,
}

/// The rendered tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    /// A freedesktop icon name. Every name used exists in Adwaita, which the Raspberry Pi
    /// desktop's `PiXtrix` theme inherits from.
    pub icon: &'static str,
    /// How urgent this is.
    pub urgency: Urgency,
    /// One line: the thing a glance should tell you.
    pub headline: String,
    /// Further lines, for the tooltip and the menu.
    pub details: Vec<String>,
    /// A poweroff is scheduled, so offering to cancel it makes sense.
    pub shutdown_pending: bool,
}

/// Renders a snapshot. `hhmm` formats a time of day, and is a parameter so tests need no
/// clock or locale.
#[must_use]
pub fn render(s: &Snapshot, hhmm: &dyn Fn(SystemTime) -> String) -> View {
    let mut view = render_ups(s, hhmm);
    if let Some(dc) = s.cpu_decicelsius {
        view.details.push(format!("CPU {} °C", dc / 10));
    }
    if let Some(fan) = s.fan {
        view.details.push(fan_line(fan));
    }
    view
}

fn render_ups(s: &Snapshot, hhmm: &dyn Fn(SystemTime) -> String) -> View {
    // The rules about what may be shown as current live in `status::interpret`, shared with
    // the case OLED, so the two displays cannot disagree about a stale file.
    let plain = |icon, urgency, headline: String, detail: &str| View {
        icon,
        urgency,
        headline,
        details: vec![detail.to_owned()],
        shutdown_pending: false,
    };
    match interpret(s.ups.as_ref(), s.now) {
        Reading::NoData => match &s.monitoring {
            // Healthy, and said so: a machine with no battery has nothing to publish.
            Monitoring::Off => plain(
                missing(s.icons),
                Urgency::Normal,
                "No battery monitored".to_owned(),
                "argond is running; this machine has no battery configured.",
            ),
            Monitoring::Source(src) => plain(
                missing(s.icons),
                Urgency::Normal,
                "Battery: no reading yet".to_owned(),
                &format!("argond is running and monitoring {src}, but has published nothing yet."),
            ),
            Monitoring::Unknown => plain(
                missing(s.icons),
                Urgency::Normal,
                "Battery: no data".to_owned(),
                "argond is not publishing status, and did not answer on the bus. Is it running?",
            ),
        },
        Reading::Stale { age } => plain(
            missing(s.icons),
            Urgency::Attention,
            format!("Battery: no update for {} s", age.as_secs()),
            "argond has stopped reporting: the battery is not being watched.",
        ),
        Reading::NoUps => plain(
            missing(s.icons),
            Urgency::Normal,
            "No UPS connected".to_owned(),
            "argond looks for an Argon UPS and picks one up when it is plugged in.",
        ),
        Reading::UpsMissing { name, last_seen } => missing_ups(s, name, last_seen),
        Reading::Failed => plain(
            missing(s.icons),
            Urgency::Normal,
            "Battery: reading failed".to_owned(),
            "The last battery read did not succeed.",
        ),
        Reading::PowerOffPending { percent, at } => View {
            icon: caution(s.icons),
            urgency: Urgency::Attention,
            headline: format!("Battery {percent} % · powering off at {}", hhmm(at)),
            details: vec!["Restore mains power to cancel.".to_owned()],
            shutdown_pending: true,
        },
        Reading::Current { percent, level } => {
            let (icon, urgency, what) = match level {
                LevelName::OnMains => (
                    level_icon(percent, true, s.icons),
                    Urgency::Normal,
                    "on mains",
                ),
                LevelName::OnBattery => (
                    level_icon(percent, false, s.icons),
                    Urgency::Normal,
                    "on battery",
                ),
                LevelName::Low => (
                    level_icon(percent, false, s.icons),
                    Urgency::Attention,
                    "low",
                ),
                LevelName::Critical => (caution(s.icons), Urgency::Attention, "critical"),
                LevelName::Unrecognised(_) => (
                    level_icon(percent, false, s.icons),
                    Urgency::Normal,
                    "unrecognised state",
                ),
            };
            View {
                icon,
                urgency,
                headline: format!("Battery {percent} % · {what}"),
                details: Vec::new(),
                shutdown_pending: false,
            }
        }
    }
}

/// The UPS seen before is gone: battery protection was lost without anyone deciding so.
fn missing_ups(s: &Snapshot, name: &str, last_seen: Option<SystemTime>) -> View {
    let seen = last_seen
        .and_then(|t| s.now.duration_since(t).ok())
        .map_or_else(String::new, |age| format!(", last seen {} ago", ago(age)));
    View {
        icon: caution(s.icons),
        urgency: Urgency::Attention,
        headline: "UPS not connected".to_owned(),
        details: vec![
            format!("{name} was connected before{seen}."),
            "No battery is being watched until it is back.".to_owned(),
            "Removed on purpose? sudo argonctl ups --forget".to_owned(),
        ],
        shutdown_pending: false,
    }
}

/// A duration in the largest whole unit: `45 s`, `12 min`, `5 h`, `3 days`.
fn ago(d: std::time::Duration) -> String {
    match d.as_secs() {
        s @ 0..=119 => format!("{s} s"),
        s @ 120..=7_199 => format!("{} min", s / 60),
        s @ 7_200..=172_799 => format!("{} h", s / 3_600),
        s => format!("{} days", s / 86_400),
    }
}

fn fan_line(fan: FanReading) -> String {
    let duty = u32::from(fan.pwm) * 100 / 255;
    match fan.rpm {
        Some(0) | None if fan.pwm == 0 => "Fan off".to_owned(),
        Some(rpm) => format!("Fan {rpm} rpm ({duty} %)"),
        None => format!("Fan {duty} %"),
    }
}

const MISSING: &str = "battery-missing-symbolic";
const CAUTION: &str = "battery-caution-symbolic";

/// The Adwaita battery icon for a level, rounded DOWN to the nearest ten.
///
/// Down, not to nearest: an icon that shows more charge than there is errs in the direction
/// that matters. At 95 % the icon shows 90.
const fn missing(icons: Icons) -> &'static str {
    match icons {
        Icons::Symbolic => MISSING,
        Icons::Colour => "battery-missing",
    }
}

const fn caution(icons: Icons) -> &'static str {
    match icons {
        Icons::Symbolic => CAUTION,
        Icons::Colour => "battery-caution",
    }
}

fn level_icon(pct: u8, on_mains: bool, icons: Icons) -> &'static str {
    match icons {
        Icons::Symbolic => symbolic_level(pct, on_mains),
        Icons::Colour => colour_level(pct, on_mains),
    }
}

/// The legacy colour set's five levels. There is no `battery-empty-charging`: an empty battery
/// on the charger shows as caution, charging.
const fn colour_level(pct: u8, on_mains: bool) -> &'static str {
    match (pct, on_mains) {
        (100.., true) => "battery-full-charged",
        (80.., true) => "battery-full-charging",
        (40.., true) => "battery-good-charging",
        (20.., true) => "battery-low-charging",
        (_, true) => "battery-caution-charging",
        (80.., false) => "battery-full",
        (40.., false) => "battery-good",
        (20.., false) => "battery-low",
        (10.., false) => "battery-caution",
        (_, false) => "battery-empty",
    }
}

/// Whether an installed icon theme has the legacy colour battery icons, looked for as
/// `<theme>/<size>/<context>/battery-good.png` in the usual icon directories.
#[must_use]
pub fn colour_icons_installed(bases: &[std::path::PathBuf]) -> bool {
    let dirs = |p: &std::path::Path| {
        std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect::<Vec<_>>()
    };
    bases.iter().any(|base| {
        dirs(base).iter().any(|theme| {
            dirs(theme).iter().any(|size| {
                dirs(size)
                    .iter()
                    .any(|ctx| ctx.join("battery-good.png").is_file())
            })
        })
    })
}

fn symbolic_level(pct: u8, on_mains: bool) -> &'static str {
    const ON_BATTERY: [&str; 11] = [
        "battery-level-0-symbolic",
        "battery-level-10-symbolic",
        "battery-level-20-symbolic",
        "battery-level-30-symbolic",
        "battery-level-40-symbolic",
        "battery-level-50-symbolic",
        "battery-level-60-symbolic",
        "battery-level-70-symbolic",
        "battery-level-80-symbolic",
        "battery-level-90-symbolic",
        "battery-level-100-symbolic",
    ];
    const ON_MAINS: [&str; 11] = [
        "battery-level-0-charging-symbolic",
        "battery-level-10-charging-symbolic",
        "battery-level-20-charging-symbolic",
        "battery-level-30-charging-symbolic",
        "battery-level-40-charging-symbolic",
        "battery-level-50-charging-symbolic",
        "battery-level-60-charging-symbolic",
        "battery-level-70-charging-symbolic",
        "battery-level-80-charging-symbolic",
        "battery-level-90-charging-symbolic",
        // Adwaita has no "100-charging": at 100 on mains it is "charged".
        "battery-level-100-charged-symbolic",
    ];
    let i = usize::from(pct.min(100) / 10);
    if on_mains { ON_MAINS[i] } else { ON_BATTERY[i] }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon_device::status::STALE_AFTER;
    use std::time::{Duration, UNIX_EPOCH};

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn snap(level: &str, percent: Option<u8>, shutdown_at: Option<u64>) -> Snapshot {
        Snapshot {
            ups: Some(UpsStatus {
                updated: at(1_000),
                level: level.to_owned(),
                percent,
                shutdown_at: shutdown_at.map(at),
                missing: None,
            }),
            now: at(1_005),
            cpu_decicelsius: None,
            fan: None,
            icons: Icons::Symbolic,
            monitoring: Monitoring::Unknown,
        }
    }

    fn nothing_published(monitoring: Monitoring) -> Snapshot {
        Snapshot {
            ups: None,
            now: at(1_005),
            cpu_decicelsius: Some(535),
            fan: None,
            icons: Icons::Symbolic,
            monitoring,
        }
    }

    #[test]
    fn a_machine_with_no_battery_is_not_accused_of_a_stopped_daemon() {
        // What the Pi 4's panel said while argond was running perfectly: "argond is not
        // publishing status. Is it running?" -- because [ups] source = "none" means there is
        // nothing to publish.
        let v = render(&nothing_published(Monitoring::Off), &fixed);
        assert_eq!(v.headline, "No battery monitored");
        assert!(
            v.details[0].contains("argond is running"),
            "{:?}",
            v.details
        );
        assert!(!v.details[0].contains("Is it running?"));
        assert_eq!(v.urgency, Urgency::Normal);
        // The rest of the panel still works: this machine has a CPU reading to show.
        assert!(
            v.details.iter().any(|d| d.contains("CPU")),
            "{:?}",
            v.details
        );
    }

    #[test]
    fn a_case_without_a_ups_is_calm_about_it() {
        // Any Argon case may run without one: the V3 showed "no reading yet ... monitoring
        // serial", which read like a fault on a machine that had nothing to monitor.
        let v = render(&snap("absent", None, None), &fixed);
        assert_eq!(v.headline, "No UPS connected");
        assert_eq!(v.urgency, Urgency::Normal);
    }

    #[test]
    fn a_ups_that_was_there_before_and_is_gone_asks_for_attention() {
        let mut s = snap("missing", None, None);
        if let Some(u) = s.ups.as_mut() {
            u.missing = Some(argon_device::status::MissingUps {
                name: "Argon USB".into(),
                last_seen: Some(at(20_000 - 3 * 3_600)),
            });
        }
        s.now = at(20_000);
        if let Some(u) = s.ups.as_mut() {
            u.updated = at(19_995);
        }
        let v = render(&s, &fixed);
        assert_eq!(v.headline, "UPS not connected");
        assert_eq!(v.urgency, Urgency::Attention);
        assert!(v.details[0].contains("Argon USB"), "{:?}", v.details);
        assert!(v.details[0].contains("3 h ago"), "{:?}", v.details);
        assert!(v.details.iter().any(|d| d.contains("--forget")));
    }

    #[test]
    fn a_daemon_that_does_not_answer_is_still_reported_as_possibly_stopped() {
        // The accusation is right when the bus says nothing: do not soften this one.
        let v = render(&nothing_published(Monitoring::Unknown), &fixed);
        assert_eq!(v.headline, "Battery: no data");
        assert!(v.details[0].contains("Is it running?"), "{:?}", v.details);
    }

    #[test]
    fn a_configured_source_with_nothing_published_yet_names_it() {
        let v = render(
            &nothing_published(Monitoring::Source("serial".into())),
            &fixed,
        );
        assert!(v.details[0].contains("serial"), "{:?}", v.details);
        assert!(!v.details[0].contains("Is it running?"));
    }

    #[test]
    fn the_colour_set_names_its_five_levels() {
        assert_eq!(colour_level(100, true), "battery-full-charged");
        assert_eq!(colour_level(92, true), "battery-full-charging");
        assert_eq!(colour_level(50, true), "battery-good-charging");
        assert_eq!(colour_level(5, true), "battery-caution-charging");
        assert_eq!(colour_level(64, false), "battery-good");
        assert_eq!(colour_level(19, false), "battery-caution");
        assert_eq!(colour_level(9, false), "battery-empty");
        let mut s = snap("critical", Some(9), None);
        s.icons = Icons::Colour;
        assert_eq!(render(&s, &fixed).icon, "battery-caution");
    }

    #[test]
    fn the_colour_set_is_found_only_where_a_theme_has_it() {
        let root = std::env::temp_dir().join(format!("argon-icons-{}", std::process::id()));
        let legacy = root.join("AdwaitaLegacy/24x24/legacy");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(root.join("Adwaita/symbolic/status")).unwrap();
        assert!(!colour_icons_installed(std::slice::from_ref(&root)));
        std::fs::write(legacy.join("battery-good.png"), b"").unwrap();
        assert!(colour_icons_installed(std::slice::from_ref(&root)));
        let _ = std::fs::remove_dir_all(&root);
        assert!(!colour_icons_installed(&[root]));
    }

    fn fixed(_: SystemTime) -> String {
        "10:38".to_owned()
    }

    #[test]
    fn on_mains_shows_the_level_as_charging() {
        let v = render(&snap("on-mains", Some(87), None), &fixed);
        assert_eq!(v.icon, "battery-level-80-charging-symbolic");
        assert_eq!(v.headline, "Battery 87 % · on mains");
        assert_eq!(v.urgency, Urgency::Normal);
        assert!(!v.shutdown_pending);
    }

    #[test]
    fn the_icon_rounds_down_never_up() {
        // Showing more charge than there is errs in the direction that matters.
        assert_eq!(
            level_icon(99, false, Icons::Symbolic),
            "battery-level-90-symbolic"
        );
        assert_eq!(
            level_icon(9, false, Icons::Symbolic),
            "battery-level-0-symbolic"
        );
        assert_eq!(
            level_icon(100, true, Icons::Symbolic),
            "battery-level-100-charged-symbolic"
        );
        assert_eq!(
            level_icon(250, false, Icons::Symbolic),
            "battery-level-100-symbolic"
        );
    }

    #[test]
    fn low_and_critical_ask_for_attention() {
        let low = render(&snap("low", Some(18), None), &fixed);
        assert_eq!(low.urgency, Urgency::Attention);
        assert_eq!(low.icon, "battery-level-10-symbolic");

        let crit = render(&snap("critical", Some(9), None), &fixed);
        assert_eq!(crit.urgency, Urgency::Attention);
        assert_eq!(crit.icon, CAUTION);
        assert_eq!(crit.headline, "Battery 9 % · critical");
    }

    #[test]
    fn a_scheduled_poweroff_outranks_the_level_and_offers_the_cancel() {
        let v = render(&snap("critical", Some(9), Some(2_000)), &fixed);
        assert_eq!(v.headline, "Battery 9 % · powering off at 10:38");
        assert!(
            v.shutdown_pending,
            "no cancel offered for a pending poweroff"
        );
        assert_eq!(v.urgency, Urgency::Attention);
    }

    #[test]
    fn a_stale_file_is_never_shown_as_current() {
        // The file still says "on mains, 95 %". If argond stopped writing it, that is not
        // true any more -- and nothing is watching the battery.
        let mut s = snap("on-mains", Some(95), None);
        s.now = at(1_000) + STALE_AFTER + Duration::from_secs(1);
        let v = render(&s, &fixed);
        assert_eq!(v.icon, MISSING);
        assert_eq!(v.urgency, Urgency::Attention);
        assert!(
            !v.headline.contains("95"),
            "showed a stale reading: {}",
            v.headline
        );
    }

    #[test]
    fn a_stale_file_hides_a_stale_poweroff_too() {
        // A scheduled time from a daemon that is no longer reporting cannot be vouched for.
        let mut s = snap("critical", Some(9), Some(2_000));
        s.now = at(1_000) + STALE_AFTER + Duration::from_secs(1);
        let v = render(&s, &fixed);
        assert!(!v.shutdown_pending);
        assert_eq!(v.icon, MISSING);
    }

    #[test]
    fn a_missing_file_and_a_failed_read_are_distinguished() {
        let mut s = snap("on-mains", Some(90), None);
        s.ups = None;
        assert_eq!(render(&s, &fixed).headline, "Battery: no data");

        let v = render(&snap("unknown", None, None), &fixed);
        assert_eq!(v.headline, "Battery: reading failed");
        assert_eq!(v.icon, MISSING);
    }

    #[test]
    fn an_unknown_level_with_a_percentage_is_still_a_failed_read() {
        // argond publishes the last percentage alongside level=unknown. The level is the
        // authority on whether the reading is current.
        let v = render(&snap("unknown", Some(80), None), &fixed);
        assert_eq!(v.headline, "Battery: reading failed");
    }

    #[test]
    fn an_unrecognised_level_says_so_rather_than_guessing() {
        let v = render(&snap("on-generator", Some(70), None), &fixed);
        assert!(v.headline.contains("unrecognised"), "{}", v.headline);
    }

    #[test]
    fn temperature_and_fan_are_added_when_known() {
        let mut s = snap("on-mains", Some(90), None);
        s.cpu_decicelsius = Some(456);
        s.fan = Some(FanReading {
            pwm: 0,
            rpm: Some(0),
        });
        let v = render(&s, &fixed);
        assert_eq!(
            v.details,
            vec!["CPU 45 °C".to_owned(), "Fan off".to_owned()]
        );

        s.fan = Some(FanReading {
            pwm: 128,
            rpm: Some(2400),
        });
        assert_eq!(render(&s, &fixed).details[1], "Fan 2400 rpm (50 %)");

        s.fan = Some(FanReading {
            pwm: 255,
            rpm: None,
        });
        assert_eq!(render(&s, &fixed).details[1], "Fan 100 %");
    }

    #[test]
    fn a_clock_behind_the_file_is_not_stale() {
        // `updated` in the future (a clock step) must not read as a huge age.
        let mut s = snap("on-mains", Some(90), None);
        s.now = at(500);
        assert_eq!(render(&s, &fixed).urgency, Urgency::Normal);
    }
}
