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
        blocked: Vec::new(),
    };
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

/// Carries out effects, and remembers what it turned off.
struct Actor {
    dry_run: bool,
    conn: zbus::blocking::Connection,
    /// The rfkill indices this agent blocked, to unblock exactly those.
    blocked: Vec<u32>,
}

impl Actor {
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
                for r in rfkill::radios() {
                    let ours = matches!(r.kind, Kind::Wlan | Kind::Bluetooth);
                    // Only what is on: a radio the user had off stays off when the lid opens.
                    if ours && !r.soft_blocked && !r.hard_blocked {
                        rfkill::set_soft_block(r.index, true).map_err(|e| e.to_string())?;
                        self.blocked.push(r.index);
                    }
                }
                Ok(())
            }
            Effect::RadiosRestore => {
                let mut failed = Vec::new();
                for index in self.blocked.drain(..) {
                    if let Err(e) = rfkill::set_soft_block(index, false) {
                        failed.push(format!("rfkill{index}: {e}"));
                    }
                }
                if failed.is_empty() {
                    Ok(())
                } else {
                    Err(failed.join("; "))
                }
            }
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
