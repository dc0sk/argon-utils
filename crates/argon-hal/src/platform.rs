// SPDX-License-Identifier: GPL-3.0-or-later
//! Host platform identification.

use crate::{Result, read_trimmed};

/// What kind of Raspberry Pi we are running on, as far as it matters to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiGeneration {
    /// BCM2711 — Raspberry Pi 4 / 400 / CM4.
    Pi4,
    /// BCM2712 — Raspberry Pi 5 / CM5.
    Pi5,
    /// A Raspberry Pi we do not specifically know about.
    Other,
    /// Not a Raspberry Pi, or the device tree does not say so.
    NotAPi,
}

impl PiGeneration {
    /// Whether the Raspberry Pi bootloader EEPROM settings apply to this board.
    ///
    /// `PSU_MAX_CURRENT` is a Pi 5 concept; applying Pi 5 EEPROM settings elsewhere is at
    /// best a no-op.
    #[must_use]
    pub const fn has_pi5_eeprom(self) -> bool {
        matches!(self, Self::Pi5)
    }
}

/// The host platform.
#[derive(Debug, Clone)]
pub struct Platform {
    /// Model string from the device tree, e.g. `Raspberry Pi 5 Model B Rev 1.1`.
    pub model: String,
    /// Board revision code from `/proc/cpuinfo`, e.g. `e04171`.
    pub revision: Option<String>,
    /// Which generation, derived from the device-tree `compatible` string.
    pub generation: PiGeneration,
    /// Kernel release.
    pub kernel: String,
    /// Distribution `PRETTY_NAME`.
    pub os: String,
    /// CPU architecture.
    pub arch: String,
}

impl Platform {
    /// Detects the host platform. Read-only.
    pub fn detect() -> Result<Self> {
        let model = read_trimmed("/proc/device-tree/model").unwrap_or_else(|_| "unknown".into());

        // The generation comes from the device-tree `compatible` list rather than from the
        // model string, because the model string is marketing text and the compatible list
        // is what the kernel actually binds drivers against. Entries are NUL-separated.
        let compatible = std::fs::read("/proc/device-tree/compatible").unwrap_or_default();
        let compatible = String::from_utf8_lossy(&compatible);
        let generation = if compatible.contains("bcm2712") {
            PiGeneration::Pi5
        } else if compatible.contains("bcm2711") {
            PiGeneration::Pi4
        } else if compatible.contains("raspberrypi") || compatible.contains("brcm,bcm2") {
            PiGeneration::Other
        } else {
            PiGeneration::NotAPi
        };

        let revision = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Revision"))
                .and_then(|v| v.split(':').nth(1))
                .map(|v| v.trim().to_owned())
        });

        let kernel =
            read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|_| "unknown".into());
        let os = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                    .map(|v| v.trim_matches('"').to_owned())
            })
            .unwrap_or_else(|| "unknown".into());

        let arch = std::env::consts::ARCH.to_owned();

        Ok(Self {
            model,
            revision,
            generation,
            kernel,
            os,
            arch,
        })
    }
}

/// Time since boot, from `/proc/uptime`.
///
/// # Errors
///
/// Fails if `/proc/uptime` cannot be read or parsed.
pub fn uptime() -> Result<std::time::Duration> {
    let text = read_trimmed("/proc/uptime")?;
    let secs = text
        .split_whitespace()
        .next()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .ok_or_else(|| crate::Error::Parse {
            what: "/proc/uptime",
            got: text.clone(),
        })?;
    Ok(std::time::Duration::from_secs_f64(secs))
}
