// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU frequency limits, through the kernel's cpufreq sysfs interface.
//!
//! Only `scaling_max_freq` is ever written, and only with a value inside the hardware's own
//! range (`cpuinfo_min_freq` .. `cpuinfo_max_freq`): the governor stays as it is, and so does
//! everything else.

use crate::{Error, Result};
use std::path::{Path, PathBuf};

const ROOT: &str = "/sys/devices/system/cpu/cpufreq";

/// One cpufreq policy: a group of CPUs that share a clock. The Pi 5 has one, for all four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    dir: PathBuf,
    /// The lowest frequency the hardware runs at, kHz.
    pub min_khz: u32,
    /// The highest, kHz.
    pub max_khz: u32,
}

/// The cpufreq policies. Empty without cpufreq.
#[must_use]
pub fn policies() -> Vec<Policy> {
    policies_in(Path::new(ROOT))
}

fn read_khz(path: &Path) -> Result<u32> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Io(std::io::Error::other(format!("{}: {e}", path.display()))))?;
    text.trim().parse().map_err(|_| Error::Parse {
        what: "a cpufreq value in kHz",
        got: text.trim().to_owned(),
    })
}

fn policies_in(root: &Path) -> Vec<Policy> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<Policy> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("policy"))
        })
        .filter_map(|dir| {
            Some(Policy {
                min_khz: read_khz(&dir.join("cpuinfo_min_freq")).ok()?,
                max_khz: read_khz(&dir.join("cpuinfo_max_freq")).ok()?,
                dir,
            })
        })
        .collect();
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

impl Policy {
    /// The policy's directory, for logs.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// The current upper limit, kHz.
    ///
    /// # Errors
    ///
    /// Fails if it cannot be read.
    pub fn scaling_max(&self) -> Result<u32> {
        read_khz(&self.dir.join("scaling_max_freq"))
    }

    /// Sets the upper limit, kHz.
    ///
    /// # Errors
    ///
    /// Refuses a value outside the hardware's range; fails if the write fails.
    pub fn set_scaling_max(&self, khz: u32) -> Result<()> {
        if !(self.min_khz..=self.max_khz).contains(&khz) {
            return Err(Error::WriteBlocked {
                what: format!("{} scaling_max_freq <- {khz}", self.dir.display()),
                reason: "outside the hardware's own frequency range",
            });
        }
        let path = self.dir.join("scaling_max_freq");
        std::fs::write(&path, khz.to_string())
            .map_err(|e| Error::Io(std::io::Error::other(format!("{}: {e}", path.display()))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake(tag: &str, max: u32) -> (PathBuf, Policy) {
        let root = std::env::temp_dir().join(format!("argon-cpufreq-{}-{tag}", std::process::id()));
        let dir = root.join("policy0");
        std::fs::create_dir_all(&dir).unwrap();
        for (f, v) in [
            ("cpuinfo_min_freq", 1_500_000),
            ("cpuinfo_max_freq", 2_400_000),
            ("scaling_max_freq", max),
        ] {
            std::fs::write(dir.join(f), format!("{v}\n")).unwrap();
        }
        let p = policies_in(&root).pop().unwrap();
        (root, p)
    }

    #[test]
    fn reads_the_range_and_sets_the_limit_within_it() {
        let (root, p) = fake("set", 2_400_000);
        assert_eq!((p.min_khz, p.max_khz), (1_500_000, 2_400_000));
        p.set_scaling_max(1_500_000).unwrap();
        assert_eq!(p.scaling_max().unwrap(), 1_500_000);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn refuses_a_limit_outside_the_hardware_range() {
        let (root, p) = fake("refuse", 2_400_000);
        assert!(p.set_scaling_max(600_000).is_err());
        assert!(p.set_scaling_max(3_000_000).is_err());
        assert_eq!(
            p.scaling_max().unwrap(),
            2_400_000,
            "a refused value was written"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
