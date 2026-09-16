// SPDX-License-Identifier: GPL-3.0-or-later
//! The Raspberry Pi's own PWM fan, as exposed through hwmon and the thermal governor.
//!
//! On an Argon ONE V5 with a Pi 5 this — not an I2C MCU — is what drives the case fan. See
//! `docs/protocol/captures/OBS-2026-09-16-v5-fan-is-kernel-controlled.md`.

use crate::read_trimmed;
use std::path::{Path, PathBuf};

/// A kernel-managed PWM fan.
#[derive(Debug, Clone)]
pub struct PwmFan {
    hwmon: PathBuf,
}

/// A snapshot of a kernel fan's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanReading {
    /// Raw PWM value, 0–255.
    pub pwm: u8,
    /// Measured speed in RPM, if the fan has a tachometer.
    pub rpm: Option<u32>,
}

impl FanReading {
    /// Whether the fan is currently turning.
    #[must_use]
    pub fn is_spinning(&self) -> bool {
        self.rpm.is_some_and(|r| r > 0) || self.pwm > 0
    }
}

impl PwmFan {
    /// Finds the kernel PWM fan, if there is one.
    #[must_use]
    pub fn find() -> Option<Self> {
        let entries = std::fs::read_dir("/sys/class/hwmon").ok()?;
        for e in entries.flatten() {
            let path = e.path();
            if read_trimmed(path.join("name")).is_ok_and(|n| n == "pwmfan") {
                return Some(Self { hwmon: path });
            }
        }
        None
    }

    /// The hwmon directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.hwmon
    }

    /// Reads the fan's current state.
    #[must_use]
    pub fn read(&self) -> Option<FanReading> {
        let pwm = read_trimmed(self.hwmon.join("pwm1")).ok()?.parse().ok()?;
        let rpm = read_trimmed(self.hwmon.join("fan1_input"))
            .ok()
            .and_then(|v| v.parse().ok());
        Some(FanReading { pwm, rpm })
    }
}

/// A thermal cooling device bound to a zone.
#[derive(Debug, Clone)]
pub struct CoolingDevice {
    /// The device's type, e.g. `pwm-fan`.
    pub kind: String,
    /// Current cooling state.
    pub state: u32,
    /// Maximum cooling state.
    pub max_state: u32,
}

impl CoolingDevice {
    /// Whether the governor is currently calling for cooling.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.state > 0
    }
}

/// Lists the cooling devices the kernel has bound to thermal zones.
///
/// A non-empty result means something other than us is already controlling a fan. Taking that
/// over is a different operation from taking over from a userspace daemon: the thermal
/// governor is a working controller with a critical trip point, and replacing it needs a
/// reason better than being able to.
#[must_use]
pub fn cooling_devices() -> Vec<CoolingDevice> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/thermal") else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("cooling_device"))
        {
            continue;
        }
        let Ok(kind) = read_trimmed(path.join("type")) else {
            continue;
        };
        let state = read_trimmed(path.join("cur_state"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let max_state = read_trimmed(path.join("max_state"))
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        out.push(CoolingDevice {
            kind,
            state,
            max_state,
        });
    }
    out
}
