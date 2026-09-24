// SPDX-License-Identifier: GPL-3.0-or-later
//! The UPS status file the daemon publishes, and what is worth telling a person about it.
//!
//! The daemon runs as a system service and cannot reach a desktop session's bus, so it writes
//! its state to a file and an agent inside the session turns changes into notifications. A
//! plain `key=value` file keeps that boundary trivial to inspect with `cat`.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where the daemon publishes UPS status by default.
pub const DEFAULT_PATH: &str = "/run/argon-utils/ups.state";

/// The format version. Bumped on any incompatible change.
pub const VERSION: u32 = 1;

/// Which power source a level implies: `mains`, `battery` or `unknown`.
///
/// Every level but `on-mains` and `unknown` is a battery level. That is obvious from the names
/// and still worth having in one place, so the tray, the CLI and the D-Bus service cannot come
/// to different conclusions about the same word.
#[must_use]
pub fn source_of(level: &str) -> &'static str {
    match level {
        "on-mains" => "mains",
        "on-battery" | "low" | "critical" => "battery",
        _ => "unknown",
    }
}

/// The level published when no UPS is connected and none was seen before.
///
/// Healthy: any Argon case may run without a UPS, and argond keeps looking for one.
pub const ABSENT: &str = "absent";

/// The level published when the UPS seen before is not connected.
///
/// Not healthy: a machine that had battery protection has lost it.
pub const MISSING: &str = "missing";

/// A UPS that was connected before and cannot be found now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingUps {
    /// What it called itself, e.g. its USB product string.
    pub name: String,
    /// When it was last read successfully, if known.
    pub last_seen: Option<SystemTime>,
}

/// A snapshot of UPS state as published by the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsStatus {
    /// When this was written.
    pub updated: SystemTime,
    /// The policy's level: `on-mains`, `on-battery`, `low`, `critical` or `unknown` -- or
    /// [`ABSENT`] or [`MISSING`] when there is no UPS to read.
    pub level: String,
    /// Charge percentage, if the last read succeeded.
    pub percent: Option<u8>,
    /// When our scheduled poweroff will happen, if one is pending.
    pub shutdown_at: Option<SystemTime>,
    /// With [`MISSING`], the UPS that is missing.
    pub missing: Option<MissingUps>,
}

impl UpsStatus {
    /// Serialises to the file format.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "version={VERSION}");
        let _ = writeln!(s, "updated={}", secs(self.updated));
        let _ = writeln!(s, "level={}", self.level);
        let _ = writeln!(
            s,
            "percent={}",
            self.percent.map_or_else(String::new, |p| p.to_string())
        );
        let _ = writeln!(
            s,
            "shutdown_at={}",
            self.shutdown_at
                .map_or_else(String::new, |t| secs(t).to_string())
        );
        // Only written when there is something to say: an older agent ignores unknown keys, and
        // every other status stays byte-for-byte what it was.
        if let Some(m) = &self.missing {
            let _ = writeln!(s, "missing={}", one_line(&m.name));
            let _ = writeln!(
                s,
                "last_seen={}",
                m.last_seen
                    .map_or_else(String::new, |t| secs(t).to_string())
            );
        }
        s
    }

    /// Parses the file format.
    ///
    /// # Errors
    ///
    /// Fails on an unknown version or a missing or malformed required field. Unknown keys are
    /// ignored, so a newer daemon can add fields without breaking an older agent.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut version = None;
        let mut updated = None;
        let mut level = None;
        let mut percent = None;
        let mut shutdown_at = None;
        let mut missing_name = None;
        let mut last_seen = None;

        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let v = v.trim();
            match k.trim() {
                "version" => version = v.parse::<u32>().ok(),
                "updated" => updated = v.parse::<u64>().ok().map(from_secs),
                "level" => level = Some(v.to_owned()),
                "percent" if !v.is_empty() => {
                    percent = Some(v.parse::<u8>().map_err(|_| format!("bad percent {v:?}"))?);
                }
                "shutdown_at" if !v.is_empty() => {
                    shutdown_at = Some(
                        v.parse::<u64>()
                            .map(from_secs)
                            .map_err(|_| format!("bad shutdown_at {v:?}"))?,
                    );
                }
                "missing" => missing_name = Some(v.to_owned()),
                "last_seen" => last_seen = v.parse::<u64>().ok().map(from_secs),
                _ => {}
            }
        }

        match version {
            Some(VERSION) => {}
            Some(other) => return Err(format!("unsupported status version {other}")),
            None => return Err("missing version".to_owned()),
        }
        Ok(Self {
            updated: updated.ok_or("missing updated")?,
            level: level.ok_or("missing level")?,
            percent,
            shutdown_at,
            missing: missing_name.map(|name| MissingUps { name, last_seen }),
        })
    }

    /// Writes the file atomically.
    ///
    /// Written to a temporary file in the same directory and renamed over the target, so a
    /// reader never sees half a file. A rename within one filesystem is atomic; a plain
    /// write-in-place is not.
    ///
    /// # Errors
    ///
    /// Fails if the directory is not writable.
    pub fn write_atomic(&self, path: &Path) -> std::io::Result<()> {
        let tmp = path.with_extension("state.tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, path)
    }
}

/// How urgent a notification is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// Informational.
    Normal,
    /// Something is about to happen to the machine.
    Critical,
}

/// Something worth telling a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// How urgent.
    pub urgency: Urgency,
    /// What to say.
    pub text: String,
}

/// How old a status may be before it counts as stale.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

/// What a status file means at a given moment.
///
/// Every display of UPS state -- the tray, the case OLED -- renders from this, so the rules
/// about when a reading may be shown as current live in exactly one place. The one that
/// matters most is staleness: a file argond stopped updating still says whatever it said
/// last, and a display that showed "on mains, 95 %" from it would claim the machine is
/// protected at the moment nothing is watching the battery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading<'a> {
    /// No status file, or one that could not be parsed.
    NoData,
    /// The file has not been updated for this long.
    Stale {
        /// How long since argond last wrote it.
        age: Duration,
    },
    /// argond is running but its last read of the UPS failed.
    Failed,
    /// A poweroff is scheduled. Outranks the level: it is the one thing with a deadline.
    PowerOffPending {
        /// Charge, percent.
        percent: u8,
        /// When the machine goes off.
        at: SystemTime,
    },
    /// No UPS is connected, and none was seen before. Healthy: nothing to watch.
    NoUps,
    /// The UPS seen before is not connected, so no battery is being watched.
    UpsMissing {
        /// What it called itself.
        name: &'a str,
        /// When it was last read, if known.
        last_seen: Option<SystemTime>,
    },
    /// A current reading.
    Current {
        /// Charge, percent.
        percent: u8,
        /// The level, as named in the status file.
        level: LevelName<'a>,
    },
}

/// A level from the status file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelName<'a> {
    /// `on-mains`.
    OnMains,
    /// `on-battery`.
    OnBattery,
    /// `low`.
    Low,
    /// `critical`.
    Critical,
    /// A name this build does not know -- a newer argond. Shown as such rather than guessed.
    Unrecognised(&'a str),
}

/// Interprets a status file at `now`.
///
/// Order matters and is the point of this function: absent, then stale, then no UPS, then
/// failed, then a pending poweroff, then the level. A stale file's poweroff time cannot be vouched for either,
/// so staleness is checked before anything it says is believed.
#[must_use]
pub fn interpret(status: Option<&UpsStatus>, now: SystemTime) -> Reading<'_> {
    let Some(s) = status else {
        return Reading::NoData;
    };
    // A file dated in the future (a clock step) is not old.
    let age = now.duration_since(s.updated).unwrap_or_default();
    if age > STALE_AFTER {
        return Reading::Stale { age };
    }
    match s.level.as_str() {
        ABSENT => return Reading::NoUps,
        MISSING => {
            return Reading::UpsMissing {
                name: s.missing.as_ref().map_or("the UPS", |m| m.name.as_str()),
                last_seen: s.missing.as_ref().and_then(|m| m.last_seen),
            };
        }
        _ => {}
    }
    // argond keeps publishing the last percentage next to `level=unknown`; the level is the
    // authority on whether that number is current.
    let Some(percent) = s.percent.filter(|_| s.level != "unknown") else {
        return Reading::Failed;
    };
    if let Some(at) = s.shutdown_at {
        return Reading::PowerOffPending { percent, at };
    }
    let level = match s.level.as_str() {
        "on-mains" => LevelName::OnMains,
        "on-battery" => LevelName::OnBattery,
        "low" => LevelName::Low,
        "critical" => LevelName::Critical,
        other => LevelName::Unrecognised(other),
    };
    Reading::Current { percent, level }
}

/// Watches successive reads of the status file and decides what to say.
///
/// Staleness lives here rather than in [`notice`] because it cannot be derived from the
/// snapshots: when the daemon stops writing, every read returns the same file with the same
/// timestamp, so "was this already stale last time" is only knowable by remembering having
/// said so.
#[derive(Debug, Default)]
pub struct Watcher {
    last: Option<UpsStatus>,
    stale_reported: bool,
}

impl Watcher {
    /// A watcher that has seen nothing yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            stale_reported: false,
        }
    }

    /// Feeds one read of the status file. `None` means the file is absent.
    pub fn observe(
        &mut self,
        current: Option<UpsStatus>,
        now: SystemTime,
        format_time: &dyn Fn(SystemTime) -> String,
    ) -> Option<Notice> {
        let Some(current) = current else {
            // No file: the daemon is not running. Say nothing -- that is also the normal state
            // on a machine without the daemon, and a notification there would be noise.
            return None;
        };

        let stale = now
            .duration_since(current.updated)
            .is_ok_and(|age| age > STALE_AFTER);
        if stale {
            let say = self.last.is_some() && !self.stale_reported;
            self.stale_reported = true;
            return say.then(|| Notice {
                urgency: Urgency::Normal,
                text: "Battery monitoring has stopped updating.".to_owned(),
            });
        }
        self.stale_reported = false;

        let out = notice(self.last.as_ref(), &current, format_time);
        self.last = Some(current);
        out
    }
}

/// Decides whether a change in status deserves a notification.
///
/// `format_time` renders a shutdown time for display; it is passed in so this stays pure.
///
/// The bias is towards saying less. A notification at every login saying "on mains" trains
/// people to dismiss them, and the one that matters -- a shutdown is scheduled -- has to be
/// read.
pub fn notice(
    previous: Option<&UpsStatus>,
    current: &UpsStatus,
    format_time: &dyn Fn(SystemTime) -> String,
) -> Option<Notice> {
    let pct = current
        .percent
        .map_or_else(String::new, |p| format!(" ({p}%)"));

    // A shutdown being scheduled outranks everything else.
    let was_scheduled = previous.and_then(|p| p.shutdown_at);
    if let (Some(at), None) = (current.shutdown_at, was_scheduled) {
        return Some(Notice {
            urgency: Urgency::Critical,
            text: format!(
                "Battery critical{pct}: powering off at {}. Restore mains power to cancel.",
                format_time(at)
            ),
        });
    }
    if let (None, Some(_)) = (current.shutdown_at, was_scheduled) {
        return Some(Notice {
            urgency: Urgency::Normal,
            text: if current.level == "on-mains" {
                format!("Mains power restored{pct}: shutdown cancelled.")
            } else {
                "Scheduled shutdown cancelled.".to_owned()
            },
        });
    }

    let Some(prev) = previous else {
        // First sight of the status, e.g. at login. Only speak if something needs attention.
        return match current.level.as_str() {
            MISSING => Some(missing_notice(current)),
            "low" => Some(Notice {
                urgency: Urgency::Normal,
                text: format!("Battery low{pct}."),
            }),
            "critical" => Some(Notice {
                urgency: Urgency::Critical,
                text: format!("Battery critical{pct}."),
            }),
            _ => None,
        };
    };

    if prev.level == current.level {
        return None;
    }
    let text = match (prev.level.as_str(), current.level.as_str()) {
        (_, MISSING) => return Some(missing_notice(current)),
        (_, ABSENT) => return None,
        // Before the level arms below: a UPS coming back on mains is not mains returning.
        (MISSING, _) => format!("The UPS is connected again{pct}."),
        (ABSENT, _) => format!("UPS connected{pct}."),
        (_, "on-battery") if prev.level == "on-mains" => format!("Running on battery{pct}."),
        (_, "low") => format!("Battery low{pct}."),
        (_, "critical") => format!("Battery critical{pct}."),
        (_, "on-mains") => format!("Mains power restored{pct}."),
        (_, "unknown") => "Lost contact with the battery.".to_owned(),
        _ => return None,
    };
    let urgency = if current.level == "critical" {
        Urgency::Critical
    } else {
        Urgency::Normal
    };
    Some(Notice { urgency, text })
}

/// What to say when the UPS seen before is gone.
fn missing_notice(current: &UpsStatus) -> Notice {
    let name = current
        .missing
        .as_ref()
        .map_or("the UPS", |m| m.name.as_str());
    Notice {
        urgency: Urgency::Normal,
        text: format!(
            "The UPS connected before ({name}) cannot be found: no battery is being watched."
        ),
    }
}

/// A value safe for one `key=value` line: a newline would end it and start a forged key.
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// Local time as HH:MM, via `date`, to avoid a date library for one field.
///
/// Shared by the notification agent and the tray, which both show when a scheduled poweroff
/// will happen. Falls back to `@<unix seconds>` if `date` cannot be run, so the time is never
/// silently dropped.
#[must_use]
pub fn local_hhmm(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    std::process::Command::new("date")
        .args(["-d", &format!("@{secs}"), "+%H:%M"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| format!("@{secs}"), |s| s.trim().to_owned())
}

/// The status-file name for a policy level.
#[must_use]
pub const fn level_name(level: argon_proto::ups::policy::Level) -> &'static str {
    use argon_proto::ups::policy::Level;
    match level {
        Level::OnMains => "on-mains",
        Level::OnBattery => "on-battery",
        Level::Low => "low",
        Level::Critical => "critical",
        Level::Unknown => "unknown",
    }
}

fn secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn from_secs(s: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(s)
}
