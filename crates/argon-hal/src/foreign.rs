// SPDX-License-Identifier: GPL-3.0-or-later
//! Detection of other software that already owns our hardware.
//!
//! Advisory locks and D-Bus name ownership arbitrate between instances of *this* project.
//! They do nothing about the vendor's Python daemons, which is what actually runs on a
//! machine we are migrating. This module finds those.
//!
//! The serial port is the sharp edge: CDC-ACM has no arbitration whatsoever, so two readers
//! split the byte stream and both desynchronise mid-frame. Every serial open must be
//! preceded by an ownership check, and discovery must avoid opening the port at all.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Vendor systemd units that conflict with us, and what each one owns.
pub const VENDOR_UNITS: &[(&str, &str)] = &[
    ("argononed.service", "fan, power button, OLED"),
    ("argononeupsd.service", "UPS battery notifications"),
    ("argonupsrtcd.service", "UPS serial port and RTC"),
    ("argononeupd.service", "ONE UP battery and lid"),
    ("argoneond.service", "EON RTC"),
];

/// A systemd unit and whether it is currently active.
#[derive(Debug, Clone)]
pub struct UnitState {
    /// Unit name.
    pub unit: String,
    /// What it owns, for the operator's benefit.
    pub owns: String,
    /// `systemctl is-active` output, e.g. `active`, `inactive`, `failed`.
    pub state: String,
}

impl UnitState {
    /// Whether this unit is currently running and therefore contending with us.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.state == "active" || self.state == "activating"
    }
}

/// Queries the vendor units' states.
///
/// Units that do not exist are reported as `not-installed` rather than omitted, because
/// "we checked and it is absent" is different information from "we did not check".
#[must_use]
pub fn vendor_units() -> Vec<UnitState> {
    VENDOR_UNITS
        .iter()
        .map(|(unit, owns)| {
            let state = Command::new("systemctl")
                .args(["is-active", unit])
                .output()
                .ok()
                .map_or_else(
                    || "unknown".to_owned(),
                    |o| {
                        let s = String::from_utf8_lossy(&o.stdout).trim().to_owned();
                        if s.is_empty() {
                            "not-installed".to_owned()
                        } else {
                            s
                        }
                    },
                );
            UnitState {
                unit: (*unit).to_owned(),
                owns: (*owns).to_owned(),
                state,
            }
        })
        .collect()
}

/// A process holding an open file descriptor on a device node.
#[derive(Debug, Clone)]
pub struct PortOwner {
    /// Process ID.
    pub pid: u32,
    /// Process command name, from `/proc/<pid>/comm`.
    pub comm: String,
}

/// Finds processes holding `dev` open, by scanning `/proc/*/fd`.
///
/// Requires privileges to see other users' processes; without them the scan silently sees
/// only our own, so an empty result is **not** proof the port is free. Callers that are
/// about to open a port must treat "no owner found, and we are not root" as inconclusive
/// and rely on an exclusive open as the real gate.
#[must_use]
pub fn port_owners(dev: &Path) -> Vec<PortOwner> {
    let Ok(target) = std::fs::canonicalize(dev) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return out;
    };

    for p in procs.flatten() {
        let name = p.file_name();
        let Ok(pid) = name.to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        let mut holds = false;
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|l| l == target) {
                holds = true;
                break;
            }
        }
        if holds {
            let comm = crate::read_trimmed(p.path().join("comm")).unwrap_or_else(|_| "?".into());
            out.push(PortOwner { pid, comm });
        }
    }
    out
}

/// Whether the `/proc` scan can see other users' processes.
///
/// Used to label an empty [`port_owners`] result honestly.
#[must_use]
pub fn can_see_all_processes() -> bool {
    // If we are root the scan is complete. Otherwise hidepid and plain permissions mean we
    // may only be seeing ourselves.
    PathBuf::from("/proc/1/fd").read_dir().is_ok()
}
