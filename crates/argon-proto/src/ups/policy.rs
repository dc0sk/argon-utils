// SPDX-License-Identifier: GPL-3.0-or-later
//! Deciding what a battery reading means.
//!
//! Pure: observations in, a decision out, no clock and no I/O. The caller supplies uptime and
//! decides what to do with the advice. Nothing here shuts anything down.
//!
//! # Deliberate differences from the vendor's behaviour
//!
//! - **Level-triggered, not edge-triggered.** The vendor issues its shutdown only when the
//!   notification *text* changes, so one missed transition means no shutdown at all. Here,
//!   every observation taken while critical returns the advice again.
//! - **Confirmed, not instant.** A single critical reading is not acted on. Fuel gauges
//!   re-estimate near empty, and one glitched byte on a serial link that can desynchronise
//!   should not be able to power the machine off.
//! - **No advice without a fresh reading.** A failed or missing read is `Unknown`, and
//!   `Unknown` never recommends shutdown, whatever the battery said before.
//! - **A mains blip does not undo a confirmation.** The level follows the reading honestly --
//!   a mains reading is reported as `OnMains` at once -- but the critical streak survives
//!   until mains is confirmed by as many consecutive readings as the battery needed. A supply
//!   flapping faster than the poll interval (a failing PSU, a bad socket, a generator) would
//!   otherwise reset the streak on every blip, and the battery would empty into a hard power
//!   cut with nothing ever confirmed. `Decision::confirmed_recovery` says which kind of mains
//!   reading this is, so a caller can hold a placed poweroff until the recovery is real.
//! - **Higher default thresholds.** The vendor shuts down at 5%. A percentage from a fuel
//!   gauge is least trustworthy exactly there, so the defaults leave more margin.

use super::PowerSource;
use core::fmt;
use core::time::Duration;

/// One battery reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// Charge percentage, 0–100.
    pub percent: u8,
    /// Where power is coming from.
    pub source: PowerSource,
}

/// How the power situation is classified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Mains present. Battery level does not matter.
    OnMains,
    /// Running from the battery with charge to spare.
    OnBattery,
    /// On battery and at or below the low threshold.
    Low,
    /// On battery, at or below the critical threshold, confirmed.
    Critical,
    /// No usable reading.
    Unknown,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OnMains => "on mains",
            Self::OnBattery => "on battery",
            Self::Low => "battery low",
            Self::Critical => "battery critical",
            Self::Unknown => "unknown",
        })
    }
}

/// What the policy advises about shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advice {
    /// Nothing to do.
    None,
    /// The battery is confirmed critical: shut down.
    Shutdown,
    /// Critical, but the machine has only just booted. Shutting down now risks a boot loop
    /// if power is flapping, so wait this much longer.
    HeldForUptime {
        /// Time left before the hold expires.
        remaining: Duration,
    },
}

/// The result of one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// The classification now in force.
    pub level: Level,
    /// The previous classification, if this observation changed it.
    pub changed_from: Option<Level>,
    /// What to do about it.
    pub advice: Advice,
    /// Mains is present and has been for `confirmations` consecutive readings.
    ///
    /// Only this justifies undoing something done because the battery was critical. A single
    /// mains reading is not enough: on a flapping supply it is followed by another outage.
    pub confirmed_recovery: bool,
}

/// Thresholds and timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyConfig {
    /// At or below this percentage on battery, the level is `Low`.
    pub low_percent: u8,
    /// At or below this percentage on battery, the level becomes `Critical` once confirmed.
    pub critical_percent: u8,
    /// How far the percentage must rise above a threshold, while still on battery, before the
    /// level steps back up. Stops a reading bouncing between 9% and 10% from flapping.
    pub recover_margin: u8,
    /// Consecutive critical readings required before `Critical`.
    pub confirmations: u8,
    /// No shutdown is advised until the machine has been up this long.
    pub min_uptime: Duration,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            low_percent: 20,
            critical_percent: 10,
            recover_margin: 5,
            confirmations: 2,
            min_uptime: Duration::from_secs(120),
        }
    }
}

/// Why a policy configuration was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    /// The critical threshold was not below the low threshold.
    CriticalNotBelowLow,
    /// A threshold was above 100.
    OutOfRange,
    /// Zero confirmations would act on a single reading.
    NoConfirmations,
    /// Zero margin would let a reading flap across a threshold.
    NoMargin,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CriticalNotBelowLow => "critical_percent must be below low_percent",
            Self::OutOfRange => "thresholds must be within 0..=100",
            Self::NoConfirmations => {
                "confirmations must be at least 1; 0 would act on a single reading"
            }
            Self::NoMargin => "recover_margin must be at least 1; 0 lets a reading flap",
        })
    }
}

impl core::error::Error for PolicyError {}

impl PolicyConfig {
    /// Checks the configuration makes sense.
    ///
    /// # Errors
    ///
    /// Returns the first problem found.
    pub const fn validate(&self) -> Result<(), PolicyError> {
        if self.low_percent > 100 || self.critical_percent > 100 {
            return Err(PolicyError::OutOfRange);
        }
        if self.critical_percent >= self.low_percent {
            return Err(PolicyError::CriticalNotBelowLow);
        }
        if self.confirmations == 0 {
            return Err(PolicyError::NoConfirmations);
        }
        if self.recover_margin == 0 {
            return Err(PolicyError::NoMargin);
        }
        Ok(())
    }
}

/// The battery policy state machine.
#[derive(Debug, Clone)]
pub struct BatteryPolicy {
    config: PolicyConfig,
    /// The last classification made from a real reading. Survives `Unknown`.
    known: Level,
    /// Consecutive readings at or below the critical threshold, on battery.
    critical_streak: u8,
    /// Consecutive readings showing mains. Confirms a recovery the way `critical_streak`
    /// confirms a critical battery.
    mains_streak: u8,
    /// What was last reported, including `Unknown`.
    reported: Level,
}

impl BatteryPolicy {
    /// Creates a policy.
    ///
    /// # Errors
    ///
    /// Fails if the configuration is invalid.
    pub const fn new(config: PolicyConfig) -> Result<Self, PolicyError> {
        match config.validate() {
            Ok(()) => Ok(Self {
                config,
                known: Level::Unknown,
                critical_streak: 0,
                mains_streak: 0,
                reported: Level::Unknown,
            }),
            Err(e) => Err(e),
        }
    }

    /// The classification last reported.
    #[must_use]
    pub const fn level(&self) -> Level {
        self.reported
    }

    /// Feeds one poll result. `None` means the read failed or the data is stale.
    pub fn observe(&mut self, observation: Option<Observation>, uptime: Duration) -> Decision {
        let previous = self.reported;
        let level = match observation {
            None => {
                // Require fresh confirmations when readings resume: a gap is exactly when a
                // reading cannot be trusted to continue a streak. Resetting the streak alone is
                // not enough, because the hysteresis branch below keeps a known-critical level
                // without consulting the streak; so a critical state steps down to Low and has
                // to be confirmed again.
                self.critical_streak = 0;
                self.mains_streak = 0;
                if self.known == Level::Critical {
                    self.known = Level::Low;
                }
                Level::Unknown
            }
            Some(o) => {
                let level = self.classify(o);
                self.known = level;
                level
            }
        };
        self.reported = level;

        let advice = if level == Level::Critical {
            match self.config.min_uptime.checked_sub(uptime) {
                Some(remaining) if !remaining.is_zero() => Advice::HeldForUptime { remaining },
                _ => Advice::Shutdown,
            }
        } else {
            Advice::None
        };

        Decision {
            level,
            changed_from: (level != previous).then_some(previous),
            advice,
            confirmed_recovery: level == Level::OnMains
                && self.mains_streak >= self.config.confirmations,
        }
    }

    fn classify(&mut self, o: Observation) -> Level {
        let c = &self.config;
        let percent = o.percent.min(100);

        if o.source == PowerSource::Mains {
            self.mains_streak = self.mains_streak.saturating_add(1);
            // The streak is kept until mains is confirmed, so that one blip in a flapping
            // supply does not send the confirmation count back to zero. The level itself is
            // reported honestly: claiming Critical while mains is present would advise a
            // shutdown at the moment power came back.
            if self.mains_streak >= c.confirmations {
                self.critical_streak = 0;
            }
            return Level::OnMains;
        }
        self.mains_streak = 0;

        if percent <= c.critical_percent {
            self.critical_streak = self.critical_streak.saturating_add(1);
        } else {
            self.critical_streak = 0;
        }

        // Hysteresis: while still on battery, a level only steps back up once the reading
        // clears its threshold by the margin.
        let clears = |threshold: u8| {
            u16::from(percent) >= u16::from(threshold) + u16::from(c.recover_margin)
        };

        match self.known {
            Level::Critical if !clears(c.critical_percent) => return Level::Critical,
            Level::Critical | Level::Low if !clears(c.low_percent) => {
                return if self.critical_streak >= c.confirmations {
                    Level::Critical
                } else {
                    Level::Low
                };
            }
            _ => {}
        }

        if percent <= c.critical_percent {
            if self.critical_streak >= c.confirmations {
                Level::Critical
            } else {
                // Not yet confirmed. At or below critical is certainly at or below low.
                Level::Low
            }
        } else if percent <= c.low_percent {
            Level::Low
        } else {
            Level::OnBattery
        }
    }
}
