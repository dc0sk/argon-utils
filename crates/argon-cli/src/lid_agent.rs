// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl lid-agent` — what closing a laptop lid does, carried out in the desktop session.
//!
//! The policy is `argon_device::lid`; this watches logind's `LidClosed` and carries out what the
//! policy asks. It runs in the session because almost everything it does belongs to the
//! logged-in user: the screen (Wayland), the sound and the notification, the radios (`/dev/rfkill`
//! carries an ACL for the active user), and the poweroff (logind allows the active session).
//! Capping the CPU is the exception, and goes through argond's `SetCpuCap`.
//!
//! On a machine without a lid switch it exits at once, so the autostart entry is harmless there.

use crate::notify::deliver_titled;
use argon_device::config::{self, Config};
use argon_device::control::{BUS_NAME, INTERFACE, OBJECT_PATH};
use argon_device::lid::{Effect, Lid};
use argon_device::status::{Notice, Urgency};
use argon_hal::rfkill::{self, Kind};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The sound played with the shutdown alert.
const ALERT_SOUND: &str = "/usr/share/sounds/freedesktop/stereo/dialog-warning.oga";

/// The longest the loop waits before checking whether it has been asked to stop.
const MAX_WAIT: Duration = Duration::from_secs(1);

#[derive(clap::Args)]
pub struct Args {
    /// Configuration file, for the `[lid]` section.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Print what it would do on each lid change, and change nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: &Args) -> ExitCode {
    if !has_lid_switch(Path::new("/sys/class/input")) {
        eprintln!("argonctl: lid-agent: this machine has no lid switch; nothing to do");
        return ExitCode::SUCCESS;
    }
    let Some(config) = load_config(args.config.as_deref()) else {
        return ExitCode::FAILURE;
    };

    let conn = match zbus::blocking::Connection::system() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("argonctl: lid-agent: no system bus: {e}");
            return ExitCode::FAILURE;
        }
    };
    let logind = match logind(&conn) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argonctl: lid-agent: cannot reach logind: {e}");
            return ExitCode::FAILURE;
        }
    };

    let stopping = stop_flag();
    let rx = watch_lid(&logind);
    eprintln!("argonctl: lid-agent: {}", describe(&config.lid));

    let start = Instant::now();
    let mut lid = Lid::new(&config.lid);
    let mut act = Actor {
        dry_run: args.dry_run,
        conn,
        record: radio_record_path(),
    };
    // Radios a previous run turned off and never turned back on -- it was killed, or the
    // machine powered off with the lid shut. systemd-rfkill would otherwise keep them off across
    // the reboot, and the user would find Wi-Fi gone for no visible reason.
    if !args.dry_run {
        match act.restore_radios() {
            Ok(0) => {}
            Ok(n) => eprintln!(
                "argonctl: lid-agent: turned {n} radio(s) back on, left off by a previous run"
            ),
            Err(e) => eprintln!("argonctl: lid-agent: could not turn leftover radios back on: {e}"),
        }
    }
    match logind.get_property::<bool>("LidClosed") {
        Ok(closed) => {
            eprintln!(
                "argonctl: lid-agent: lid {} at start",
                if closed { "closed" } else { "open" }
            );
            act.all(lid.report(closed, start.elapsed()));
        }
        Err(e) => {
            eprintln!("argonctl: lid-agent: LidClosed unreadable ({e}); waiting for a change");
        }
    }

    let mut code = ExitCode::SUCCESS;
    while !stopping.load(Ordering::Relaxed) {
        let wait = lid
            .next_deadline()
            .map_or(MAX_WAIT, |d| d.saturating_sub(start.elapsed()))
            .min(MAX_WAIT);
        match rx.recv_timeout(wait) {
            Ok(closed) => {
                eprintln!(
                    "argonctl: lid-agent: lid {}",
                    if closed { "closed" } else { "opened" }
                );
                act.all(lid.report(closed, start.elapsed()));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                eprintln!("argonctl: lid-agent: lost logind's lid signals; stopping");
                code = ExitCode::FAILURE;
                break;
            }
        }
        act.all(lid.tick(start.elapsed()));
    }
    // Whatever closing the lid turned down must not stay down because the agent went away.
    act.all(lid.restore_all());
    code
}

fn load_config(path: Option<&Path>) -> Option<Config> {
    let path = path.unwrap_or_else(|| Path::new(config::DEFAULT_PATH));
    if !path.exists() {
        return Some(Config::default());
    }
    Config::load(path)
        .map_err(|e| eprintln!("argonctl: lid-agent: {}: {e}", path.display()))
        .ok()
}

/// Set by SIGTERM, SIGINT or SIGHUP: a session ending, or a manual stop.
fn stop_flag() -> Arc<AtomicBool> {
    let stopping = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGHUP,
    ] {
        let _ = signal_hook::flag::register(sig, Arc::clone(&stopping));
    }
    stopping
}

/// Lid changes, as logind signals them. A thread turns the signals into channel messages so
/// the main loop can also wake for the shutdown deadline.
fn watch_lid(logind: &zbus::blocking::Proxy<'static>) -> mpsc::Receiver<bool> {
    let (tx, rx) = mpsc::channel();
    let watcher = logind.clone();
    std::thread::spawn(move || {
        for change in watcher.receive_property_changed::<bool>("LidClosed") {
            if let Ok(closed) = change.get() {
                if tx.send(closed).is_err() {
                    break;
                }
            }
        }
    });
    rx
}

fn describe(c: &argon_device::config::LidConfig) -> String {
    let mut s = format!("action {}", c.action);
    if c.action == "shutdown" {
        let _ = write!(s, ", {} s after the alert", c.shutdown_delay_s);
    } else {
        if c.radios_off {
            s += ", radios off";
        }
        if c.cpu_throttle {
            s += ", CPU capped";
        }
    }
    s
}

/// Whether any input device reports a lid switch (`SW_LID`, bit 0 of its switch capabilities).
fn has_lid_switch(input: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(input) else {
        return false;
    };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("capabilities/sw"))
            .ok()
            .and_then(|s| s.split_whitespace().last().map(str::to_owned))
            .and_then(|word| u64::from_str_radix(&word, 16).ok())
            .is_some_and(|bits| bits & 1 == 1)
    })
}

fn logind(conn: &zbus::blocking::Connection) -> zbus::Result<zbus::blocking::Proxy<'static>> {
    zbus::blocking::Proxy::new(
        conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
}

/// Where the agent records the radios it turned off: `$XDG_STATE_HOME/argon-utils/lid-radios`,
/// or `~/.local/state/argon-utils/lid-radios`.
fn radio_record_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("argon-utils/lid-radios"))
}

const fn kind_name(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Wlan => Some("wlan"),
        Kind::Bluetooth => Some("bluetooth"),
        Kind::Other => None,
    }
}

/// The radios to switch off: Wi-Fi and Bluetooth that are on. A radio the user had off stays
/// off when the lid opens, because it is never recorded.
fn to_block(radios: &[rfkill::Radio]) -> Vec<(u32, String)> {
    radios
        .iter()
        .filter(|r| !r.soft_blocked && !r.hard_blocked)
        .filter_map(|r| kind_name(r.kind).map(|k| (r.index, format!("{k} {}", r.name))))
        .collect()
}

/// The indices, now, of the recorded radios that are still switched off in software. Matched by
/// type and name: rfkill's indices are not stable across reboots.
fn to_unblock(record: &str, radios: &[rfkill::Radio]) -> Vec<u32> {
    let wanted: Vec<&str> = record
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    radios
        .iter()
        .filter(|r| r.soft_blocked)
        .filter(|r| {
            kind_name(r.kind).is_some_and(|k| wanted.contains(&format!("{k} {}", r.name).as_str()))
        })
        .map(|r| r.index)
        .collect()
}

/// Carries out effects.
struct Actor {
    dry_run: bool,
    conn: zbus::blocking::Connection,
    /// The record of radios this agent turned off, which survives it.
    record: Option<PathBuf>,
}

impl Actor {
    /// Turns back on the radios on the record, and removes the record. Returns how many.
    fn restore_radios(&self) -> Result<usize, String> {
        let Some(record) = self.record.as_ref() else {
            return Ok(0);
        };
        let Ok(text) = std::fs::read_to_string(record) else {
            return Ok(0);
        };
        let indices = to_unblock(&text, &rfkill::radios());
        let mut failed = Vec::new();
        for index in &indices {
            if let Err(e) = rfkill::set_soft_block(*index, false) {
                failed.push(format!("rfkill{index}: {e}"));
            }
        }
        if failed.is_empty() {
            let _ = std::fs::remove_file(record);
            Ok(indices.len())
        } else {
            // The record stays, so the next start tries again.
            Err(failed.join("; "))
        }
    }

    fn all(&mut self, effects: Vec<Effect>) {
        for e in effects {
            if self.dry_run {
                println!("would: {e:?}");
                continue;
            }
            match self.one(e) {
                Ok(()) => eprintln!("argonctl: lid-agent: {e:?}"),
                Err(err) => eprintln!("argonctl: lid-agent: {e:?} failed: {err}"),
            }
        }
    }

    fn one(&mut self, e: Effect) -> Result<(), String> {
        match e {
            Effect::ScreenOff => screens("--off"),
            Effect::ScreenOn => screens("--on"),
            Effect::RadiosOff => {
                let targets = to_block(&rfkill::radios());
                // Recorded before anything is switched: whatever interrupts this, nothing can
                // end up off without being on the record.
                let record = self
                    .record
                    .as_ref()
                    .ok_or("no place to record the radios")?;
                if let Some(dir) = record.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                let text = targets.iter().fold(String::new(), |mut t, (_, id)| {
                    let _ = writeln!(t, "{id}");
                    t
                });
                std::fs::write(record, text).map_err(|e| format!("{}: {e}", record.display()))?;
                for (index, _) in targets {
                    rfkill::set_soft_block(index, true).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            Effect::RadiosRestore => self.restore_radios().map(|_| ()),
            Effect::CpuCap(capped) => {
                zbus::blocking::Proxy::new(&self.conn, BUS_NAME, OBJECT_PATH, INTERFACE)
                    .and_then(|p| p.call::<_, _, ()>("SetCpuCap", &(capped,)))
                    .map_err(|e| format!("argond: {e}"))
            }
            Effect::ShutdownAlert(delay) => {
                // The sound first: it is what reaches someone who is not looking at the screen.
                let _ = Command::new("pw-play").arg(ALERT_SOUND).spawn();
                deliver_titled(
                    "Lid closed",
                    &Notice {
                        urgency: Urgency::Critical,
                        text: format!(
                            "Lid closed: powering off in {} s. Open the lid to cancel.",
                            delay.as_secs()
                        ),
                    },
                )
                .map(|_| ())
            }
            Effect::ShutdownCancelled => deliver_titled(
                "Lid opened",
                &Notice {
                    urgency: Urgency::Normal,
                    text: "Lid opened: the poweroff is cancelled.".into(),
                },
            )
            .map(|_| ()),
            Effect::PowerOff => logind(&self.conn)
                .and_then(|p| p.call::<_, _, ()>("PowerOff", &(false,)))
                .map_err(|e| format!("logind: {e}")),
        }
    }
}

/// Turns every output off or on, through the compositor (wlr-output-power-management).
fn screens(how: &str) -> Result<(), String> {
    let list = Command::new("wlopm")
        .output()
        .map_err(|e| format!("wlopm: {e}"))?;
    if !list.status.success() {
        return Err(format!(
            "wlopm: {}",
            String::from_utf8_lossy(&list.stderr).trim()
        ));
    }
    let names: Vec<String> = String::from_utf8_lossy(&list.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect();
    if names.is_empty() {
        return Err("wlopm lists no outputs".into());
    }
    for name in names {
        let ok = Command::new("wlopm")
            .args([how, &name])
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return Err(format!("wlopm {how} {name} failed"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio(index: u32, kind: Kind, name: &str, soft: bool, hard: bool) -> rfkill::Radio {
        rfkill::Radio {
            index,
            kind,
            name: name.into(),
            soft_blocked: soft,
            hard_blocked: hard,
        }
    }

    #[test]
    fn only_radios_that_are_on_are_switched_off_and_recorded() {
        let got = to_block(&[
            radio(0, Kind::Bluetooth, "hci0", false, false),
            radio(1, Kind::Wlan, "phy0", true, false),
            radio(2, Kind::Wlan, "phy1", false, true),
            radio(3, Kind::Other, "nfc0", false, false),
        ]);
        assert_eq!(got, vec![(0, "bluetooth hci0".to_owned())]);
    }

    #[test]
    fn the_record_finds_radios_by_name_after_their_indices_change() {
        // Recorded as bluetooth hci0 and wlan phy0; after a reboot the indices are swapped.
        let now = [
            radio(0, Kind::Wlan, "phy0", true, false),
            radio(1, Kind::Bluetooth, "hci0", true, false),
            radio(2, Kind::Wlan, "phy9", true, false),
        ];
        let mut got = to_unblock("bluetooth hci0\nwlan phy0\n", &now);
        got.sort_unstable();
        assert_eq!(got, vec![0, 1], "phy9 was never recorded, so it stays off");
    }

    #[test]
    fn a_recorded_radio_already_back_on_is_left_alone() {
        let now = [radio(0, Kind::Wlan, "phy0", false, false)];
        assert!(to_unblock("wlan phy0\n", &now).is_empty());
    }

    #[test]
    fn finds_a_lid_switch_by_its_capability_bit() {
        let root = std::env::temp_dir().join(format!("argon-input-{}", std::process::id()));
        for (n, sw) in [(0, "0"), (1, "1"), (2, "")] {
            let d = root.join(format!("input{n}/capabilities"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("sw"), format!("{sw}\n")).unwrap();
        }
        assert!(has_lid_switch(&root));
        std::fs::write(root.join("input1/capabilities/sw"), "20\n").unwrap();
        assert!(
            !has_lid_switch(&root),
            "a switch without SW_LID counted as a lid"
        );
        let _ = std::fs::remove_dir_all(&root);
        assert!(!has_lid_switch(&root));
    }
}
