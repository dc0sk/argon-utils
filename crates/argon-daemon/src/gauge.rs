// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's battery thread for the Argon ONE UP, reading its CW2217 fuel gauge.
//!
//! The same cycle as the PWR UPS thread -- read, decide, act, publish -- through the same
//! policy, shutdown coordinator and status file, so the tray, the OLED and the notification
//! agent work unchanged. What the ONE UP lacks is the UPS's clock and scheduled wake, so this
//! thread has no housekeeping and refuses wake requests.

use crate::ups::{self, Requests};
use argon_device::config::Config;
use argon_device::control::Response;
use argon_device::gauge::{Cw2217, GaugeMonitor};
use argon_device::power::{Logind, ShutdownCoordinator};
use argon_device::ups_service::{oneup_contention, step};
use argon_hal::i2c::LinuxI2c;
use argon_hal::mode::Mode;
use argon_proto::cw2217::{self, Flow};
use argon_proto::ups::policy::BatteryPolicy;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

/// Starts ONE UP battery monitoring.
///
/// Returns `None` -- after saying why -- when the policy is invalid or no I2C bus can be named.
pub fn spawn(
    config: &Config,
    active_units: &[String],
    stopping: Arc<AtomicBool>,
    heartbeat: Arc<AtomicU64>,
    requests: Requests,
) -> Option<JoinHandle<()>> {
    // Reading alongside the vendor daemon is safe -- the I2C bus arbitrates, unlike the UPS's
    // serial port -- so it only decides whether argond may act.
    let contended = oneup_contention(active_units);
    let requested = config.mode().unwrap_or_default();
    let enforce = requested == Mode::Full && contended.is_empty();
    if enforce {
        eprintln!(
            "argond: ups: ONE UP battery: shutdown ENABLED: poweroff {} min after the battery \
             is confirmed critical, cancelled once it stops discharging",
            config.ups.shutdown_delay_min
        );
    } else if requested == Mode::Full {
        eprintln!(
            "argond: ups: ONE UP battery: shutdown in dry run: {} is running and responsible \
             for this battery",
            contended.join(", ")
        );
    } else {
        eprintln!(
            "argond: ups: ONE UP battery: shutdown in dry run: mode is {requested}, and only \
             mode \"full\" may power the machine off"
        );
    }

    let policy = match BatteryPolicy::new(config.ups.policy()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argond: ups: battery policy: {e}");
            return None;
        }
    };

    let bus = if config.mcu.bus == "auto" {
        argon_hal::discovery::header_i2c_bus().map(|p| p.display().to_string())
    } else {
        Some(config.mcu.bus.clone())
    };
    let Some(bus) = bus else {
        eprintln!(
            "argond: ups: no I2C bus found for the ONE UP's fuel gauge; is dtparam=i2c_arm=on \
             set in config.txt?"
        );
        return None;
    };

    let interval = Duration::from_secs(config.ups.poll_interval_s.max(1));
    let delay = Duration::from_secs(config.ups.shutdown_delay_min * 60);
    let state_file = PathBuf::from(&config.ups.state_file);

    Some(std::thread::spawn(move || {
        run(
            &bus,
            &policy,
            interval,
            delay,
            !enforce,
            &state_file,
            &stopping,
            &heartbeat,
            &requests,
        );
    }))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one thread body; bundling these into a struct would only move the list"
)]
fn run(
    bus: &str,
    policy: &BatteryPolicy,
    interval: Duration,
    delay: Duration,
    dry_run: bool,
    state_file: &std::path::Path,
    stopping: &AtomicBool,
    heartbeat: &AtomicU64,
    requests: &Requests,
) {
    let mut coordinator = ShutdownCoordinator::new(Logind, delay, dry_run);
    let mut monitor: Option<GaugeMonitor<LinuxI2c>> = None;
    let mut open_error_logged = false;
    let mut uptime_error_logged = false;
    let mut state_error_logged = false;
    let mut held_logged = false;
    let mut last_flow: Option<Flow> = None;

    while !stopping.load(Ordering::Relaxed) {
        heartbeat.store(ups::unix_now(), Ordering::Relaxed);

        if monitor.is_none() {
            // A fresh policy on reconnect: readings after a gap must confirm critical again.
            monitor = open_gauge(bus, policy, &mut open_error_logged);
        }

        if let Some(m) = monitor.as_mut() {
            let uptime = ups::uptime_or_zero(&mut uptime_error_logged);
            let cycle = step(m, &mut coordinator, uptime, SystemTime::now());
            ups::report_cycle(&cycle, &mut held_logged);
            if let Some(r) = m.last() {
                // The level only changes on battery; charging starting and stopping is worth
                // a line too, since on a laptop that is the charger being plugged in.
                if last_flow != Some(r.flow) {
                    eprintln!(
                        "argond: ups: ONE UP battery {} at {}% ({:.3} V, current {:+})",
                        flow_name(r.flow),
                        r.percent,
                        f64::from(r.vcell_uv) / 1e6,
                        r.current
                    );
                    last_flow = Some(r.flow);
                }
            }
            if cycle.poll.consecutive_failures >= ups::REOPEN_AFTER {
                eprintln!("argond: ups: reopening the fuel gauge");
                monitor = None;
                last_flow = None;
            }
            ups::publish(&cycle.status, state_file, &mut state_error_logged);
        }

        refuse_requests(requests);
        ups::sleep_unless(interval, stopping, &requests.waiting);
    }

    if !dry_run && coordinator.scheduled_at().is_some() {
        eprintln!("argond: ups: stopping with a poweroff scheduled; leaving it in place");
    }
}

/// Opens the gauge and confirms what it is, logging a failure once per episode.
fn open_gauge(
    bus: &str,
    policy: &BatteryPolicy,
    error_logged: &mut bool,
) -> Option<GaugeMonitor<LinuxI2c>> {
    let opened = LinuxI2c::open(bus, u16::from(cw2217::ADDR)).and_then(Cw2217::identify);
    match opened {
        Ok(mut gauge) => {
            let health = gauge.health().map_or_else(
                |e| format!("cycle count unreadable ({e})"),
                |h| format!("{} charge cycles, health {}%", h.cycles, h.soh),
            );
            eprintln!(
                "argond: ups: ONE UP fuel gauge: CW2217 at {}; {health}",
                gauge.describe()
            );
            *error_logged = false;
            Some(GaugeMonitor::new(gauge, policy.clone()))
        }
        Err(e) => {
            if !*error_logged {
                eprintln!(
                    "argond: ups: no CW2217 fuel gauge at 0x{:02x} on {bus} ({e}). Is this an \
                     Argon ONE UP? Will keep trying once per poll without repeating this.",
                    cw2217::ADDR
                );
                *error_logged = true;
            }
            None
        }
    }
}

/// Answers every waiting request: the ONE UP has nothing to set a wake on.
fn refuse_requests(requests: &Requests) {
    requests.waiting.store(false, Ordering::Relaxed);
    while let Ok((_, reply)) = requests.rx.try_recv() {
        let _ = reply.send(Response::error(
            "this machine's battery is the ONE UP's, which has no scheduled wake; nothing was done",
        ));
    }
}

const fn flow_name(flow: Flow) -> &'static str {
    match flow {
        Flow::Charging => "charging",
        Flow::Discharging => "discharging",
        Flow::Idle => "resting on the charger",
    }
}
