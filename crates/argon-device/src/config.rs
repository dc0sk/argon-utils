// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration.
//!
//! # Unknown keys are errors
//!
//! Every struct here denies unknown fields. A misspelled key that is silently ignored is how
//! a safety setting gets lost: `allow_stop` written as `allow_stops` would read as the
//! default and nobody would learn otherwise until the fan was off on a hot machine. The cost
//! is that a typo stops the daemon starting, which is the direction that failure should fall.

use argon_hal::mode::Mode;
use argon_proto::fan::{CurvePoint, FanCurve, FanDuty};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// The default location of the configuration file.
pub const DEFAULT_PATH: &str = "/etc/argon-utils/config.toml";

/// Upper bound on the fan poll interval, which is also the watchdog ping period.
const MAX_FAN_POLL_INTERVAL_S: u64 = 15;

/// The whole configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// How much the daemon may do to the hardware.
    pub mode: String,
    /// Fan control.
    pub fan: FanConfig,
    /// MCU addressing.
    pub mcu: McuConfig,
    /// UPS monitoring.
    pub ups: UpsConfig,
    /// Metrics export.
    pub telemetry: TelemetryConfig,
    /// The case OLED.
    pub oled: OledConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::ReadOnly.as_str().to_owned(),
            fan: FanConfig::default(),
            mcu: McuConfig::default(),
            ups: UpsConfig::default(),
            telemetry: TelemetryConfig::default(),
            oled: OledConfig::default(),
        }
    }
}

/// The case OLED status page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OledConfig {
    /// Whether argond draws on the OLED at all.
    ///
    /// Off by default: it is a write to hardware, and a display that lights up after an
    /// upgrade because a default changed is exactly the surprise this project avoids. It also
    /// needs `mode` to be `managed` or `full`, like every other device write.
    pub enabled: bool,
    /// The panel is mounted upside down.
    pub flip: bool,
    /// Brightness, 0-255.
    ///
    /// Lower than the controller's power-on default of 127, because a status page is static
    /// content and wear on an OLED scales with brightness as well as time.
    pub contrast: u8,
    /// Seconds between redraws. The panel is only written when the page actually changes.
    pub refresh_s: u64,
}

impl Default for OledConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            flip: false,
            contrast: 64,
            refresh_s: 5,
        }
    }
}

/// Metrics export.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TelemetryConfig {
    /// Whether to serve Prometheus metrics.
    pub enabled: bool,
    /// Address to listen on.
    ///
    /// Loopback by default. These metrics describe a machine's thermal and power state and
    /// the endpoint has no authentication, so exposing it beyond the host should be a
    /// deliberate act rather than something a default does quietly.
    pub listen: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: "127.0.0.1:9843".to_owned(),
        }
    }
}

/// Fan control settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FanConfig {
    /// Curve points, as `{ temp_c, duty }`.
    pub curve: Vec<CurveEntry>,
    /// How far the temperature must fall below a threshold before duty steps down.
    pub hysteresis_c: i32,
    /// Floor applied to any non-zero duty.
    pub min_duty: u8,
    /// Duty restored when the daemon stops.
    pub safe_duty: u8,
    /// At or above this temperature, a stopping daemon restores full duty instead.
    pub hot_threshold_c: i32,
    /// Whether the fan may be stopped entirely.
    ///
    /// Defaults to false. Stopping a fan is a decision, and the default should not make it
    /// on the operator's behalf.
    pub allow_stop: bool,
    /// Seconds between temperature readings.
    pub poll_interval_s: u64,
    /// Minimum milliseconds between writes to the MCU.
    pub min_write_interval_ms: u64,
}

impl Default for FanConfig {
    fn default() -> Self {
        Self {
            curve: vec![
                CurveEntry {
                    temp_c: 55,
                    duty: 30,
                },
                CurveEntry {
                    temp_c: 60,
                    duty: 55,
                },
                CurveEntry {
                    temp_c: 65,
                    duty: 100,
                },
            ],
            hysteresis_c: 3,
            min_duty: 10,
            safe_duty: 55,
            hot_threshold_c: 75,
            allow_stop: false,
            poll_interval_s: 5,
            min_write_interval_ms: 500,
        }
    }
}

/// One curve point in configuration form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurveEntry {
    /// Temperature threshold in whole degrees Celsius.
    pub temp_c: i32,
    /// Duty as a percentage.
    pub duty: u8,
}

/// MCU addressing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct McuConfig {
    /// Which protocol to speak.
    ///
    /// There is deliberately no `"auto"`: detection requires a transaction that sets the fan
    /// to full on legacy firmware. See ADR-0002.
    pub dialect: String,
    /// I2C bus device path, or `auto` to discover it.
    pub bus: String,
}

impl Default for McuConfig {
    fn default() -> Self {
        Self {
            dialect: "legacy".to_owned(),
            bus: "auto".to_owned(),
        }
    }
}

/// UPS monitoring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpsConfig {
    /// Where telemetry comes from: `serial` (the PWR UPS), `oneup` (the ONE UP's CW2217 fuel
    /// gauge, on the I2C bus named in `[mcu] bus`), `hid`, or `none`.
    pub source: String,
    /// Port path, or `auto`.
    pub port: String,
    /// Seconds between polls.
    pub poll_interval_s: u64,
    /// At or below this percentage on battery, the level is "low".
    pub low_percent: u8,
    /// At or below this percentage on battery, the level becomes "critical" once confirmed.
    pub critical_percent: u8,
    /// How far a reading must rise above a threshold, still on battery, to step back up.
    pub recover_margin: u8,
    /// Consecutive critical readings required before "critical".
    pub confirmations: u8,
    /// No shutdown is advised until the machine has been up this many seconds.
    pub min_uptime_s: u64,
    /// Minutes between the battery going critical and the poweroff. Mains returning inside
    /// this window cancels it.
    pub shutdown_delay_min: u64,
    /// Where the daemon publishes UPS status for the desktop agent.
    pub state_file: String,
    /// Keep the UPS clock set from the system clock. Only acts in `full` mode, and only while
    /// the system clock is NTP-synchronised; see `clock_sync`.
    pub sync_clock: bool,
}

impl Default for UpsConfig {
    fn default() -> Self {
        let p = argon_proto::ups::policy::PolicyConfig::default();
        Self {
            source: "serial".to_owned(),
            port: "auto".to_owned(),
            poll_interval_s: 10,
            low_percent: p.low_percent,
            critical_percent: p.critical_percent,
            recover_margin: p.recover_margin,
            confirmations: p.confirmations,
            min_uptime_s: p.min_uptime.as_secs(),
            shutdown_delay_min: 2,
            state_file: crate::status::DEFAULT_PATH.to_owned(),
            sync_clock: true,
        }
    }
}

impl UpsConfig {
    /// The battery policy configuration these settings describe.
    #[must_use]
    pub const fn policy(&self) -> argon_proto::ups::policy::PolicyConfig {
        argon_proto::ups::policy::PolicyConfig {
            low_percent: self.low_percent,
            critical_percent: self.critical_percent,
            recover_margin: self.recover_margin,
            confirmations: self.confirmations,
            min_uptime: std::time::Duration::from_secs(self.min_uptime_s),
        }
    }
}

impl Config {
    /// Parses configuration from TOML.
    ///
    /// # Errors
    ///
    /// Fails on malformed TOML, an unknown key, or a setting that does not make sense.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(|e| ConfigError::Toml(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Loads configuration from a file.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be read or does not validate.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
        Self::from_toml(&text)
    }

    /// Serialises back to TOML.
    ///
    /// # Errors
    ///
    /// Fails only if the configuration cannot be represented, which should not happen.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        toml::to_string_pretty(self).map_err(|e| ConfigError::Toml(e.to_string()))
    }

    /// The parsed operating mode.
    ///
    /// # Errors
    ///
    /// Fails if the mode name is not recognised.
    pub fn mode(&self) -> Result<Mode, ConfigError> {
        Mode::parse(&self.mode).ok_or_else(|| ConfigError::BadValue {
            key: "mode",
            got: self.mode.clone(),
            expected: "read-only, managed or full",
        })
    }

    /// The fan curve, validated.
    ///
    /// # Errors
    ///
    /// Fails if any duty is out of range or the curve itself is invalid.
    pub fn fan_curve(&self) -> Result<FanCurve, ConfigError> {
        let mut points = Vec::with_capacity(self.fan.curve.len());
        for e in &self.fan.curve {
            let duty = FanDuty::new(e.duty).map_err(|_| ConfigError::BadValue {
                key: "fan.curve.duty",
                got: e.duty.to_string(),
                expected: "0..=100",
            })?;
            points.push(CurvePoint::from_celsius(e.temp_c, duty));
        }
        FanCurve::new(points).map_err(|e| ConfigError::Curve(e.to_string()))
    }

    /// How often to read the temperature.
    #[must_use]
    pub const fn fan_poll_interval(&self) -> Duration {
        Duration::from_secs(self.fan.poll_interval_s)
    }

    /// Minimum interval between MCU writes.
    #[must_use]
    pub const fn min_write_interval(&self) -> Duration {
        Duration::from_millis(self.fan.min_write_interval_ms)
    }

    /// Checks settings that parsing alone cannot.
    fn validate(&self) -> Result<(), ConfigError> {
        self.mode()?;
        self.fan_curve()?;

        if self.oled.refresh_s == 0 {
            return Err(ConfigError::BadValue {
                key: "oled.refresh_s",
                got: "0".to_owned(),
                expected: "at least 1 second; zero is a busy loop on a shared I2C bus",
            });
        }

        if self.mcu.dialect != "legacy" {
            // "auto" is rejected explicitly rather than falling into the generic message,
            // because an operator writing it has a specific wrong idea worth correcting.
            let expected = if self.mcu.dialect == "auto" {
                "legacy (there is no auto: detection is unsafe, see ADR-0002)"
            } else {
                "legacy (register support requires a build with the unverified-register feature)"
            };
            return Err(ConfigError::BadValue {
                key: "mcu.dialect",
                got: self.mcu.dialect.clone(),
                expected,
            });
        }

        if let Err(e) = self.ups.policy().validate() {
            return Err(ConfigError::BadValue {
                key: "ups",
                got: format!(
                    "low_percent={}, critical_percent={}, recover_margin={}, confirmations={}",
                    self.ups.low_percent,
                    self.ups.critical_percent,
                    self.ups.recover_margin,
                    self.ups.confirmations
                ),
                expected: policy_expectation(e),
            });
        }

        if !(1..=30).contains(&self.ups.shutdown_delay_min) {
            return Err(ConfigError::BadValue {
                key: "ups.shutdown_delay_min",
                got: self.ups.shutdown_delay_min.to_string(),
                expected: "1 to 30 minutes: 0 would be immediate, and a long delay spends the \
                           battery the shutdown exists to protect",
            });
        }

        if !matches!(
            self.ups.source.as_str(),
            "serial" | "oneup" | "hid" | "none"
        ) {
            return Err(ConfigError::BadValue {
                key: "ups.source",
                got: self.ups.source.clone(),
                expected: "serial, oneup, hid or none",
            });
        }

        if self.fan.safe_duty == 0 {
            return Err(ConfigError::BadValue {
                key: "fan.safe_duty",
                got: "0".to_owned(),
                expected: "a duty that actually spins the fan; 0 would make the safety \
                           fallback a stopped fan",
            });
        }

        if self.telemetry.enabled
            && self
                .telemetry
                .listen
                .parse::<std::net::SocketAddr>()
                .is_err()
        {
            return Err(ConfigError::BadValue {
                key: "telemetry.listen",
                got: self.telemetry.listen.clone(),
                expected: "an address and port, e.g. 127.0.0.1:9843",
            });
        }

        if self.fan.poll_interval_s == 0 {
            return Err(ConfigError::BadValue {
                key: "fan.poll_interval_s",
                got: "0".to_owned(),
                expected: "at least 1 second; zero is a busy loop",
            });
        }

        // systemd's watchdog is fed once per control-loop iteration, so this interval is also
        // the watchdog period. The shipped unit sets WatchdogSec=30; an interval at or above
        // it makes systemd kill and restart a perfectly healthy daemon, which looks exactly
        // like a crash loop and is very hard to diagnose from the outside.
        if self.fan.poll_interval_s >= MAX_FAN_POLL_INTERVAL_S {
            return Err(ConfigError::BadValue {
                key: "fan.poll_interval_s",
                got: self.fan.poll_interval_s.to_string(),
                expected: "below 15 seconds: it is also the systemd watchdog ping period                            (WatchdogSec=30 in the shipped unit)",
            });
        }

        Ok(())
    }
}

/// What a rejected battery policy needed instead, for the error message.
const fn policy_expectation(e: argon_proto::ups::policy::PolicyError) -> &'static str {
    use argon_proto::ups::policy::PolicyError;
    match e {
        PolicyError::CriticalNotBelowLow => "critical_percent below low_percent",
        PolicyError::OutOfRange => "percentages within 0..=100",
        PolicyError::NoConfirmations => {
            "confirmations of at least 1; 0 would act on a single reading"
        }
        PolicyError::NoMargin => "recover_margin of at least 1; 0 lets a reading flap",
    }
}

/// Why a configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The file could not be read.
    Io(String),
    /// The TOML was malformed, or contained an unknown key.
    Toml(String),
    /// A setting had an unusable value.
    BadValue {
        /// Which key.
        key: &'static str,
        /// What was written.
        got: String,
        /// What was expected.
        expected: &'static str,
    },
    /// The fan curve was invalid.
    Curve(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Both already carry a fully-formed message from below.
            Self::Io(e) | Self::Toml(e) => write!(f, "{e}"),
            Self::BadValue { key, got, expected } => {
                write!(f, "{key}: {got:?} is not valid; expected {expected}")
            }
            Self::Curve(e) => write!(f, "fan curve: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}
