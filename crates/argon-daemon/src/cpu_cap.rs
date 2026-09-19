// SPDX-License-Identifier: GPL-3.0-or-later
//! Capping the CPU at its lowest frequency, and putting the limit back.
//!
//! What the lid agent asks for while a laptop lid is closed. The limit in force before the cap
//! is remembered and restored exactly -- not reset to the maximum, which would undo whatever
//! the administrator had set -- and it is restored when argond stops, too: a cap must not
//! outlive the daemon that remembers what to put back.

use argon_hal::cpufreq::{self, Policy};

/// Something that can cap and uncap the CPU. A trait so the D-Bus handler is testable.
pub trait CapControl: Send {
    /// Caps every policy at its lowest frequency. Capping twice changes nothing.
    ///
    /// # Errors
    ///
    /// Fails if a limit cannot be read or written; whatever was already capped is put back.
    fn cap(&mut self) -> Result<(), String>;

    /// Puts back the limits in force before [`CapControl::cap`]. Nothing to do if not capped.
    ///
    /// # Errors
    ///
    /// Fails if a limit cannot be written back.
    fn lift(&mut self) -> Result<(), String>;
}

/// The real thing, over sysfs.
#[derive(Default)]
pub struct CpuCap {
    /// The policies capped, with the limit each had before.
    saved: Vec<(Policy, u32)>,
}

impl CapControl for CpuCap {
    fn cap(&mut self) -> Result<(), String> {
        if !self.saved.is_empty() {
            return Ok(());
        }
        let policies = cpufreq::policies();
        if policies.is_empty() {
            return Err("this machine has no cpufreq policies to cap".into());
        }
        for p in policies {
            let before = p.scaling_max().map_err(|e| e.to_string());
            let capped = before.and_then(|b| {
                p.set_scaling_max(p.min_khz)
                    .map(|()| b)
                    .map_err(|e| e.to_string())
            });
            match capped {
                Ok(b) => self.saved.push((p, b)),
                Err(e) => {
                    let _ = self.lift();
                    return Err(format!("capping {}: {e}", p.path().display()));
                }
            }
        }
        Ok(())
    }

    fn lift(&mut self) -> Result<(), String> {
        let mut failed = Vec::new();
        for (p, before) in self.saved.drain(..) {
            if let Err(e) = p.set_scaling_max(before) {
                failed.push(format!("{}: {e}", p.path().display()));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }
}

impl Drop for CpuCap {
    fn drop(&mut self) {
        if !self.saved.is_empty() {
            match self.lift() {
                Ok(()) => eprintln!("argond: cpu: cap lifted on the way out"),
                Err(e) => eprintln!("argond: cpu: could not lift the cap on the way out: {e}"),
            }
        }
    }
}
