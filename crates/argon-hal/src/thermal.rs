// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the CPU temperature.

use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Something that can report a temperature.
///
/// A trait so the fan control loop can be tested against a scripted temperature series,
/// without a Raspberry Pi and without waiting for a real CPU to heat up.
pub trait TemperatureSource {
    /// Reads the current temperature in tenths of a degree Celsius.
    ///
    /// # Errors
    ///
    /// Fails if the source cannot be read.
    fn read_decicelsius(&mut self) -> Result<i32>;

    /// A short description for logs.
    fn describe(&self) -> String;
}

/// A kernel thermal zone, e.g. `/sys/class/thermal/thermal_zone0`.
#[derive(Debug, Clone)]
pub struct ThermalZone {
    path: PathBuf,
    kind: String,
}

impl ThermalZone {
    /// Opens a specific thermal zone directory.
    ///
    /// # Errors
    ///
    /// Fails if the zone's `type` cannot be read.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let path = dir.as_ref().to_owned();
        let kind = crate::read_trimmed(path.join("type"))?;
        Ok(Self { path, kind })
    }

    /// Finds the CPU thermal zone by its `type`, rather than assuming zone 0.
    ///
    /// Zone numbering depends on probe order. On the development machine the CPU happens to
    /// be zone 0, but a board with more sensors would order them differently, and reading
    /// the wrong zone would drive the fan from something that is not the CPU.
    ///
    /// # Errors
    ///
    /// Fails if no zone reports a CPU-like type.
    pub fn find_cpu() -> Result<Self> {
        let entries = std::fs::read_dir("/sys/class/thermal")?;
        let mut seen = Vec::new();
        for e in entries.flatten() {
            let path = e.path();
            if !path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("thermal_zone"))
            {
                continue;
            }
            let Ok(kind) = crate::read_trimmed(path.join("type")) else {
                continue;
            };
            if kind.contains("cpu") || kind.contains("soc") {
                return Ok(Self { path, kind });
            }
            seen.push(kind);
        }
        Err(Error::Parse {
            what: "a CPU thermal zone",
            got: format!("zones present: {}", seen.join(", ")),
        })
    }

    /// The zone's `type` string, e.g. `cpu-thermal`.
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The zone directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TemperatureSource for ThermalZone {
    fn read_decicelsius(&mut self) -> Result<i32> {
        let raw = crate::read_trimmed(self.path.join("temp"))?;
        let millidegrees: i32 = raw.parse().map_err(|_| Error::Parse {
            what: "a thermal zone temperature",
            got: raw.clone(),
        })?;
        // The kernel reports millidegrees; the fan curve works in tenths.
        Ok(millidegrees / 100)
    }

    fn describe(&self) -> String {
        format!("{} ({})", self.path.display(), self.kind)
    }
}

/// A temperature source that replays a fixed series, for tests.
#[derive(Debug, Clone)]
pub struct ScriptedTemperature {
    /// `None` marks a reading that should fail.
    readings: Vec<Option<i32>>,
    index: usize,
}

impl ScriptedTemperature {
    /// Replays these temperatures in order, then repeats the last one.
    #[must_use]
    pub fn new(decicelsius: impl IntoIterator<Item = i32>) -> Self {
        Self {
            readings: decicelsius.into_iter().map(Some).collect(),
            index: 0,
        }
    }

    /// Replays a series where `None` entries fail.
    #[must_use]
    pub fn with_failures(readings: Vec<Option<i32>>) -> Self {
        Self { readings, index: 0 }
    }
}

impl TemperatureSource for ScriptedTemperature {
    fn read_decicelsius(&mut self) -> Result<i32> {
        let i = self.index.min(self.readings.len().saturating_sub(1));
        self.index += 1;
        match self.readings.get(i) {
            Some(Some(v)) => Ok(*v),
            Some(None) => Err(Error::Parse {
                what: "a scripted temperature",
                got: "simulated failure".to_owned(),
            }),
            None => Err(Error::Parse {
                what: "a scripted temperature",
                got: "no readings".to_owned(),
            }),
        }
    }

    fn describe(&self) -> String {
        "scripted".into()
    }
}
