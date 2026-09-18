// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl notify-agent` — desktop notifications for UPS events.
//!
//! Runs inside a desktop session, because a system daemon cannot reach a session's bus. Reads
//! the status file the daemon publishes and turns changes into notifications.

use argon_device::status::{self, Notice, UpsStatus, Urgency, Watcher};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::{Duration, SystemTime};

#[derive(clap::Args)]
pub struct Args {
    /// Status file to watch.
    #[arg(long, value_name = "PATH", default_value = status::DEFAULT_PATH)]
    pub state: PathBuf,

    /// Seconds between checks.
    #[arg(long, default_value = "2")]
    pub interval: u64,

    /// Send one test notification and exit, to check delivery works on this desktop.
    #[arg(long)]
    pub test: bool,
}

pub fn run(args: &Args) -> ExitCode {
    if args.test {
        let n = Notice {
            urgency: Urgency::Normal,
            text: "argon-utils: notification test, no action needed.".to_owned(),
        };
        return match deliver(&n) {
            Ok(how) => {
                println!("sent via {how}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("argonctl: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let mut watcher = Watcher::new();
    let interval = Duration::from_secs(args.interval.max(1));
    loop {
        let current = std::fs::read_to_string(&args.state)
            .ok()
            .and_then(|t| UpsStatus::parse(&t).ok());
        if let Some(n) = watcher.observe(current, SystemTime::now(), &status::local_hhmm) {
            if let Err(e) = deliver(&n) {
                // Keep going: a missed notification is bad, a dead agent is worse.
                eprintln!("argonctl: could not deliver {:?}: {e}", n.text);
            }
        }
        std::thread::sleep(interval);
    }
}

/// Delivers a notification by the first mechanism this desktop supports.
///
/// The Raspberry Pi panel first: its binary accepts `notify` and `critical` commands on the
/// session bus, which is how the Pi desktop shows notifications (it has no standard
/// notification daemon). Then the freedesktop `notify-send`, for other desktops.
fn deliver(n: &Notice) -> Result<&'static str, String> {
    let panel_cmd = match n.urgency {
        Urgency::Normal => "notify",
        Urgency::Critical => "critical",
    };
    // The panel's method sends no reply, so waiting for one only produces a timeout error.
    let panel = Command::new("busctl")
        .args([
            "--user",
            "call",
            "--expect-reply=no",
            "org.wayfire.wfpanel",
            "/org/wayfire/wfpanel",
            "org.wayfire.wfpanel",
            "command",
            "ss",
            panel_cmd,
            &n.text,
        ])
        .output();
    if panel.as_ref().is_ok_and(|o| o.status.success()) {
        return Ok("the Pi panel");
    }

    let urgency = match n.urgency {
        Urgency::Normal => "normal",
        Urgency::Critical => "critical",
    };
    let send = Command::new("notify-send")
        .args([
            "--urgency",
            urgency,
            "--app-name",
            "argon-utils",
            "UPS",
            &n.text,
        ])
        .output();
    if send.as_ref().is_ok_and(|o| o.status.success()) {
        return Ok("notify-send");
    }

    let why = panel.map_or_else(
        |e| e.to_string(),
        |o| String::from_utf8_lossy(&o.stderr).trim().to_owned(),
    );
    Err(format!(
        "no notification service accepted it (panel: {why})"
    ))
}
