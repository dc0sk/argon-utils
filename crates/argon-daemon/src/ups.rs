// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's UPS thread.

use argon_device::clock_sync::{self, Verdict};
use argon_device::config::Config;
use argon_device::power::{Action, Logind, ShutdownCoordinator};
use argon_device::ups::{Gate, Ups, UpsMonitor};
use argon_device::ups_service::{contention, step};
use argon_hal::mode::Mode;
use argon_hal::serial::SerialLink;
use argon_hal::{discovery, platform};
use argon_proto::ups::policy::{Advice, BatteryPolicy};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// How many poll intervals may pass with no heartbeat before the thread counts as stalled.
///
/// A thread that has panicked, or wedged inside a device read, stops updating its heartbeat.
/// Nothing else notices: the service stays `active`, the fan loop keeps feeding the watchdog,
/// and the machine simply has no battery protection any more.
pub const STALL_INTERVALS: u32 = 4;

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
    heartbeat: Arc<AtomicU64>,
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

    // Setting the UPS clock is the only write argond ever sends the UPS. Gated on full mode
    // like every other device write, and on the operator's switch.
    let clock_writes = requested == Mode::Full && config.ups.sync_clock;
    if clock_writes {
        eprintln!(
            "argond: ups: clock sync ON: compared every {} h, set when more than {} s off",
            clock_sync::CHECK_INTERVAL.as_secs() / 3_600,
            clock_sync::THRESHOLD_S
        );
    } else {
        eprintln!(
            "argond: ups: clock sync off (it needs mode \"full\" and sync_clock = true); \
             the offset is still checked and logged"
        );
    }

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
            &heartbeat,
            clock_writes,
        );
    }))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one thread body; bundling these into a struct would only move the list"
)]
fn run(
    port: Option<&std::path::Path>,
    policy: &BatteryPolicy,
    interval: Duration,
    delay: Duration,
    dry_run: bool,
    state_file: &std::path::Path,
    stopping: &AtomicBool,
    heartbeat: &AtomicU64,
    clock_writes: bool,
) {
    let mut coordinator = ShutdownCoordinator::new(Logind, delay, dry_run);
    let mut monitor: Option<UpsMonitor<Gate<SerialLink>>> = None;
    // Due immediately: the first check runs right after the first successful poll, so a
    // clock left wrong by a deep discharge is found at boot rather than six hours in.
    let mut next_clock_check = Instant::now();
    let mut state_error_logged = false;
    let mut uptime_error_logged = false;
    let mut open_error_logged = false;
    let mut held_logged = false;

    while !stopping.load(Ordering::Relaxed) {
        // Before the work, so a stall inside the work below is what the watcher sees.
        heartbeat.store(unix_now(), Ordering::Relaxed);

        if monitor.is_none() {
            match open_link(port, &mut open_error_logged) {
                Ok(Some(link)) => {
                    // A fresh policy on reconnect: readings after a gap must confirm critical
                    // again, which is the conservative direction.
                    let gate = if clock_writes {
                        Gate::with_clock_writes(link)
                    } else {
                        Gate::queries_only(link)
                    };
                    monitor = Some(UpsMonitor::new(Ups::new(gate), policy.clone()));
                    open_error_logged = false;
                }
                Ok(None) => {}
                // Held by someone else: wait rather than fighting over the port.
                Err(()) => {
                    sleep_unless_stopping(interval, stopping);
                    continue;
                }
            }
        }

        if let Some(m) = monitor.as_mut() {
            // A failed uptime read reads as "just booted", which holds the shutdown back.
            // That is the safe direction, but it is indistinguishable from a healthy daemon
            // unless it is said out loud.
            let uptime = match platform::uptime() {
                Ok(u) => {
                    uptime_error_logged = false;
                    u
                }
                Err(e) => {
                    if !uptime_error_logged {
                        eprintln!(
                            "argond: ups: cannot read uptime ({e}); treating the machine as \
                             just booted, which holds any shutdown back"
                        );
                        uptime_error_logged = true;
                    }
                    Duration::ZERO
                }
            };
            let cycle = step(m, &mut coordinator, uptime, SystemTime::now());

            // Logged once per episode: a daemon permanently holding a shutdown must not look
            // like a daemon with nothing to do.
            if let Advice::HeldForUptime { remaining } = cycle.poll.decision.advice {
                if !held_logged {
                    eprintln!(
                        "argond: ups: battery critical, but holding the shutdown for another \
                         {}s after boot",
                        remaining.as_secs()
                    );
                    held_logged = true;
                }
            } else {
                held_logged = false;
            }

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
            } else if cycle.poll.error.is_none() && Instant::now() >= next_clock_check {
                // Only on a healthy link, after the poll: the clock is housekeeping, and must
                // never delay or displace a battery reading.
                check_clock(m, clock_writes, clock_sync::system_clock_synced());
                next_clock_check = Instant::now() + clock_sync::CHECK_INTERVAL;
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

    if !dry_run && coordinator.scheduled_at().is_some() {
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
        Action::Adopted { at } => eprintln!(
            "argond: ups: took back over a poweroff we scheduled before restarting (unix time \
             {})",
            at.duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        ),
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

/// Finds and opens the UPS port.
///
/// `Err(())` means another process holds it, which the caller answers by waiting.
/// `Ok(None)` means there was nothing to open, or opening failed; either way the reason is
/// logged once rather than on every poll, because on an Argon case with no PWR UPS that
/// message would otherwise be thousands of journal lines a day.
fn open_link(
    port: Option<&std::path::Path>,
    error_logged: &mut bool,
) -> Result<Option<SerialLink>, ()> {
    let Some(path) = port
        .map(std::path::Path::to_path_buf)
        .or_else(discovery::argon_ups_serial_path)
    else {
        if !*error_logged {
            eprintln!(
                "argond: ups: no UPS serial port found; will keep looking once per poll \
                 without repeating this"
            );
            *error_logged = true;
        }
        return Ok(None);
    };

    // serial.rs promises an ownership check before every open, and the daemon was not keeping
    // it: the vendor-unit check happens once at spawn, from a snapshot, and says nothing
    // about a hand-started process or a CLI watch holding the port. Two readers on CDC-ACM
    // split the byte stream and both desynchronise.
    if let Some(o) = argon_hal::foreign::port_owners(&path).into_iter().next() {
        eprintln!(
            "argond: ups: {} is held by pid {} ({}); not opening it. Two readers corrupt \
             each other's frames.",
            path.display(),
            o.pid,
            o.comm
        );
        return Err(());
    }

    match SerialLink::open(&path) {
        Ok(link) => {
            eprintln!("argond: ups: reading {}", path.display());
            Ok(Some(link))
        }
        Err(e) => {
            if !*error_logged {
                eprintln!("argond: ups: cannot open {}: {e}", path.display());
                *error_logged = true;
            }
            Ok(None)
        }
    }
}

/// Compares the UPS clock with the system clock, and sets it if allowed and needed.
///
/// Every outcome is logged, in sync or not: over weeks the journal then records how fast the
/// UPS clock drifts, which nothing else measures. A failure is logged and left for the next
/// check.
fn check_clock(m: &mut UpsMonitor<Gate<SerialLink>>, clock_writes: bool, system_synced: bool) {
    let ups_clock = match m.ups_mut().clock() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("argond: ups: clock check: reading the clock failed: {e}");
            return;
        }
    };
    match clock_sync::judge(ups_clock, unix_now(), system_synced) {
        Verdict::InSync { offset_s } => {
            eprintln!("argond: ups: clock offset {offset_s:+} s, within tolerance");
        }
        Verdict::Untrusted { offset_s } => eprintln!(
            "argond: ups: clock offset {offset_s:+} s; not correcting it, because the system \
             clock is not NTP-synchronised"
        ),
        Verdict::Implausible => {
            eprintln!("argond: ups: the clock read back as an impossible date; not acting on it");
        }
        Verdict::Correct { offset_s } if !clock_writes => eprintln!(
            "argond: ups: clock offset {offset_s:+} s; not correcting it (clock sync is off)"
        ),
        Verdict::Correct { offset_s } => {
            clock_sync::sleep_to_next_second();
            let Some(now) = argon_proto::ups::UpsTime::from_unix_seconds(unix_now()) else {
                return;
            };
            if let Err(e) = m.ups_mut().set_clock(now) {
                eprintln!("argond: ups: clock was {offset_s:+} s off; setting it failed: {e}");
                return;
            }
            let after = m
                .ups_mut()
                .clock()
                .ok()
                .map(|t| clock_sync::judge(t, unix_now(), true));
            match after {
                Some(Verdict::InSync { offset_s: now_off }) => eprintln!(
                    "argond: ups: clock was {offset_s:+} s off; set from the system clock, now \
                     {now_off:+} s"
                ),
                other => eprintln!(
                    "argond: ups: clock was {offset_s:+} s off; set it, but the read-back is \
                     {other:?}"
                ),
            }
        }
    }
}

/// Seconds since the unix epoch, or 0 if the clock is before it.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

#[cfg(test)]
mod tests {
    //! The clock check against the simulated UPS over a real PTY. The decision itself is
    //! tested in `clock_sync`; this is the glue that actually writes, so it is tested in both
    //! directions -- that it corrects when allowed, and that it does not when not.

    use super::*;
    use argon_proto::ups::UpsTime;
    use argon_sim::ups::{Faults, UpsSim, UpsState};
    use serialport::SerialPort;

    /// Runs `body` against a simulated UPS whose clock starts `offset_s` from the system clock,
    /// and returns the clock offset afterwards.
    fn offset_after(offset_s: i64, clock_writes: bool, synced: bool) -> i64 {
        let start = i64::try_from(unix_now()).unwrap() + offset_s;
        let state = UpsState {
            clock: UpsTime::from_unix_seconds(u64::try_from(start).unwrap()).unwrap(),
            ..UpsState::default()
        };
        let (master, slave) = serialport::TTYPort::pair().expect("PTY pair");
        let slave_name = slave.name().expect("slave path");
        let stop = Arc::new(AtomicBool::new(false));
        let sim_stop = Arc::clone(&stop);
        let sim = std::thread::spawn(move || {
            let mut master = master;
            let mut sim = UpsSim::with_faults(state, Faults::default());
            let _ = sim.serve_until(
                &mut master,
                Instant::now() + Duration::from_secs(30),
                &sim_stop,
            );
        });
        let _slave = slave;
        let link = SerialLink::open(&slave_name).expect("open the simulated port");
        let gate = if clock_writes {
            Gate::with_clock_writes(link)
        } else {
            Gate::queries_only(link)
        };
        let policy = BatteryPolicy::new(argon_proto::ups::policy::PolicyConfig::default()).unwrap();
        let mut m = UpsMonitor::new(Ups::new(gate), policy);

        check_clock(&mut m, clock_writes, synced);

        let after = m.ups_mut().clock().expect("read the clock back");
        stop.store(true, Ordering::Relaxed);
        drop(m);
        let _ = sim.join();
        i64::try_from(after.to_unix_seconds().unwrap()).unwrap()
            - i64::try_from(unix_now()).unwrap()
    }

    #[test]
    fn a_slow_clock_is_corrected_when_allowed() {
        // The T15 finding: 21 s slow.
        let after = offset_after(-21, true, true);
        assert!(
            after.abs() <= clock_sync::THRESHOLD_S,
            "still {after:+} s off"
        );
    }

    #[test]
    fn a_slow_clock_is_left_alone_without_clock_writes() {
        let after = offset_after(-21, false, true);
        assert!(
            after <= -19,
            "was changed without permission: now {after:+} s"
        );
    }

    #[test]
    fn a_slow_clock_is_left_alone_when_the_system_clock_is_not_synced() {
        let after = offset_after(-21, true, false);
        assert!(
            after <= -19,
            "copied an unsynchronised clock: now {after:+} s"
        );
    }
}
