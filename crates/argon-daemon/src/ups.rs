// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's UPS thread.

use argon_device::config::Config;
use argon_device::power::{Action, Logind, ShutdownCoordinator};
use argon_device::ups::{QueryOnly, Ups, UpsMonitor};
use argon_device::ups_service::{contention, step};
use argon_hal::mode::Mode;
use argon_hal::serial::SerialLink;
use argon_hal::{discovery, platform};
use argon_proto::ups::policy::BatteryPolicy;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// Consecutive failed reads before the port is closed and reopened.
///
/// The device has been seen to re-enumerate under a new node name; a link held open across
/// that points at nothing and never recovers on its own.
const REOPEN_AFTER: u32 = 3;

/// Starts UPS monitoring, if it is configured and possible.
///
/// Returns `None` -- after saying why -- when the source is not serial, the vendor daemon
/// holds the port, or the policy is invalid.
pub fn spawn(
    config: &Config,
    active_units: &[String],
    stopping: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    if config.ups.source != "serial" {
        eprintln!(
            "argond: ups: source is {:?}, not monitoring",
            config.ups.source
        );
        return None;
    }

    let contended = contention(active_units);
    if contended.iter().any(|u| u == "argonupsrtcd.service") {
        eprintln!(
            "argond: ups: argonupsrtcd holds the serial port, not monitoring. Two readers on a \
             CDC-ACM port corrupt each other's frames."
        );
        return None;
    }

    let requested = config.mode().unwrap_or_default();
    let enforce = requested == Mode::Full && contended.is_empty();
    if enforce {
        eprintln!(
            "argond: ups: shutdown ENABLED: poweroff {} min after the battery is confirmed \
             critical, cancelled if mains returns",
            config.ups.shutdown_delay_min
        );
    } else if requested == Mode::Full {
        eprintln!(
            "argond: ups: shutdown in dry run: {} still running and has its own shutdown logic",
            contended.join(", ")
        );
    } else {
        eprintln!(
            "argond: ups: shutdown in dry run: mode is {requested}, and only mode \"full\" may \
             power the machine off"
        );
    }

    let policy = match BatteryPolicy::new(config.ups.policy()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argond: ups: battery policy: {e}");
            return None;
        }
    };

    let port = if config.ups.port == "auto" {
        None
    } else {
        Some(PathBuf::from(&config.ups.port))
    };
    let interval = Duration::from_secs(config.ups.poll_interval_s.max(1));
    let delay = Duration::from_secs(config.ups.shutdown_delay_min * 60);
    let state_file = PathBuf::from(&config.ups.state_file);

    Some(std::thread::spawn(move || {
        run(
            port.as_deref(),
            &policy,
            interval,
            delay,
            !enforce,
            &state_file,
            &stopping,
        );
    }))
}

fn run(
    port: Option<&std::path::Path>,
    policy: &BatteryPolicy,
    interval: Duration,
    delay: Duration,
    dry_run: bool,
    state_file: &std::path::Path,
    stopping: &AtomicBool,
) {
    let mut coordinator = ShutdownCoordinator::new(Logind, delay, dry_run);
    let mut monitor: Option<UpsMonitor<QueryOnly<SerialLink>>> = None;
    let mut state_error_logged = false;

    while !stopping.load(Ordering::Relaxed) {
        if monitor.is_none() {
            let path = port
                .map(std::path::Path::to_path_buf)
                .or_else(discovery::argon_ups_serial_path);
            match path.as_ref().map(SerialLink::open) {
                Some(Ok(link)) => {
                    eprintln!(
                        "argond: ups: reading {}",
                        path.as_ref()
                            .map_or_else(String::new, |p| p.display().to_string())
                    );
                    // A fresh policy on reconnect: readings after a gap must confirm critical
                    // again, which is the conservative direction.
                    monitor = Some(UpsMonitor::new(Ups::new(QueryOnly(link)), policy.clone()));
                }
                Some(Err(e)) => eprintln!("argond: ups: cannot open port: {e}"),
                None => eprintln!("argond: ups: no UPS serial port found"),
            }
        }

        if let Some(m) = monitor.as_mut() {
            let uptime = platform::uptime().unwrap_or(Duration::ZERO);
            let cycle = step(m, &mut coordinator, uptime, SystemTime::now());

            if let Some(from) = cycle.poll.decision.changed_from {
                let pct = cycle
                    .status
                    .percent
                    .map_or_else(String::new, |p| format!(" at {p}%"));
                eprintln!("argond: ups: {from} -> {}{pct}", cycle.poll.decision.level);
            }
            log_action(&cycle.action);
            if let Some(e) = &cycle.poll.error {
                eprintln!(
                    "argond: ups: read failed ({} in a row): {e}",
                    cycle.poll.consecutive_failures
                );
            }
            if cycle.poll.consecutive_failures >= REOPEN_AFTER {
                eprintln!("argond: ups: reopening the port");
                monitor = None;
            }

            if let Some(dir) = state_file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match cycle.status.write_atomic(state_file) {
                Ok(()) => state_error_logged = false,
                Err(e) if !state_error_logged => {
                    eprintln!("argond: ups: cannot write {}: {e}", state_file.display());
                    state_error_logged = true;
                }
                Err(_) => {}
            }
        }

        sleep_unless_stopping(interval, stopping);
    }

    if coordinator.scheduled_at().is_some() {
        // The battery is still critical; stopping this daemon does not change that.
        eprintln!("argond: ups: stopping with a poweroff scheduled; leaving it in place");
    }
}

fn log_action(action: &Action) {
    match action {
        Action::None => {}
        Action::Scheduled { at } => {
            // T12 logged this as a raw SystemTime debug struct, which nobody reading a
            // journal at 3 a.m. can parse. The delay is what an operator needs: how long
            // they have to plug the mains back in.
            let left = at
                .duration_since(SystemTime::now())
                .map_or(0, |d| d.as_secs());
            let epoch = at
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            eprintln!(
                "argond: ups: poweroff SCHEDULED in {}m{:02}s (unix time {epoch})",
                left / 60,
                left % 60
            );
        }
        Action::AlreadyPending { .. } => {
            eprintln!("argond: ups: a shutdown is already pending; leaving it alone");
        }
        Action::Cancelled => eprintln!("argond: ups: poweroff cancelled"),
        Action::OverriddenByOperator => {
            eprintln!("argond: ups: our poweroff was cancelled by someone else; standing down");
        }
        Action::WouldSchedule => eprintln!("argond: ups: would schedule a poweroff (dry run)"),
        Action::WouldCancel => eprintln!("argond: ups: would cancel the poweroff (dry run)"),
        Action::Failed(e) => eprintln!("argond: ups: shutdown action failed, will retry: {e}"),
    }
}

/// Sleeps in short slices so a stop request is honoured promptly.
fn sleep_unless_stopping(total: Duration, stopping: &AtomicBool) {
    let slice = Duration::from_millis(250);
    let mut left = total;
    while !left.is_zero() && !stopping.load(Ordering::Relaxed) {
        let d = left.min(slice);
        std::thread::sleep(d);
        left = left.saturating_sub(d);
    }
}
