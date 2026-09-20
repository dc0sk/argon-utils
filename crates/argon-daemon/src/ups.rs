// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's UPS thread.

use argon_device::clock_sync::{self, Verdict};
use argon_device::config::Config;
use argon_device::control::{Request, Response};
use argon_device::drift;
use argon_device::power::PowerControl;
use argon_device::power::{Action, Logind, ShutdownCoordinator};
use argon_device::ups::{Gate, Ups, UpsMonitor, Writes};
use argon_device::ups_service::{contention, step};
use argon_device::wake::{self, Assessment};
use argon_hal::mode::Mode;
use argon_hal::serial::SerialLink;
use argon_hal::{discovery, platform};
use argon_proto::ups::policy::{Advice, BatteryPolicy};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
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
pub(crate) const REOPEN_AFTER: u32 = 3;

/// Requests from the control socket, and the flag that says one is waiting.
pub struct Requests {
    /// Where requests arrive.
    pub rx: Receiver<crate::control::Message>,
    /// Set by the control thread when it sends one, so the UPS thread cuts its sleep short.
    pub waiting: Arc<AtomicBool>,
}

/// Starts UPS monitoring, if it is configured and possible.
///
/// Returns `None` -- after saying why -- when the source is not serial, the vendor daemon
/// holds the port, or the policy is invalid.
pub fn spawn(
    config: &Config,
    active_units: &[String],
    stopping: Arc<AtomicBool>,
    heartbeat: Arc<AtomicU64>,
    requests: Requests,
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

    // Setting a wake schedule -- and parking one that would come due on a running machine --
    // is a full-mode write like the rest.
    let writes = Writes {
        clock: clock_writes,
        wake: requested == Mode::Full,
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
            &heartbeat,
            writes,
            &requests,
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
    writes: Writes,
    requests: &Requests,
) {
    let clock_writes = writes.clock;
    let drift_record = drift_record_path();
    let drift_record = drift_record.as_deref();
    let mut coordinator = ShutdownCoordinator::new(Logind, delay, dry_run);
    let mut monitor: Option<UpsMonitor<Gate<SerialLink>>> = None;
    // Due immediately: the first check runs right after the first successful poll, so a
    // clock left wrong by a deep discharge is found at boot rather than six hours in.
    let mut next_clock_check = Instant::now();
    // Also due at once: a schedule left close to due -- or past, by a wake that did not clear
    // itself -- must be found at boot, not five minutes in.
    let mut next_wake_check = Instant::now();
    let mut wake_logged = Seen::Unknown;
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
                    let gate = Gate::with_writes(link, writes);
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
            let uptime = uptime_or_zero(&mut uptime_error_logged);
            let cycle = step(m, &mut coordinator, uptime, SystemTime::now());
            report_cycle(&cycle, &mut held_logged);
            if cycle.poll.consecutive_failures >= REOPEN_AFTER {
                eprintln!("argond: ups: reopening the port");
                monitor = None;
            } else if cycle.poll.error.is_none() {
                // Only on a healthy link, after the poll: housekeeping must never delay or
                // displace a battery reading.
                if Instant::now() >= next_clock_check {
                    let again = check_and_record_clock(m, clock_writes, drift_record);
                    next_clock_check = Instant::now() + again;
                }
                if Instant::now() >= next_wake_check {
                    check_wake(m, writes.wake, unix_now(), &mut wake_logged);
                    next_wake_check = Instant::now() + wake::CHECK_EVERY;
                }
            }

            publish(&cycle.status, state_file, &mut state_error_logged);
        }

        serve_requests(requests, monitor.as_mut(), writes.wake);

        sleep_unless(interval, stopping, &requests.waiting);
    }

    if !dry_run && coordinator.scheduled_at().is_some() {
        // The battery is still critical; stopping this daemon does not change that.
        eprintln!("argond: ups: stopping with a poweroff scheduled; leaving it in place");
    }
}

/// Uptime, or zero -- "just booted" -- if it cannot be read.
///
/// Zero holds any shutdown back. That is the safe direction, but it is indistinguishable from a
/// healthy daemon unless it is said out loud, so the failure is logged once per episode.
pub(crate) fn uptime_or_zero(error_logged: &mut bool) -> Duration {
    match platform::uptime() {
        Ok(u) => {
            *error_logged = false;
            u
        }
        Err(e) => {
            if !*error_logged {
                eprintln!(
                    "argond: ups: cannot read uptime ({e}); treating the machine as just \
                     booted, which holds any shutdown back"
                );
                *error_logged = true;
            }
            Duration::ZERO
        }
    }
}

/// Logs what one monitoring cycle found and did.
pub(crate) fn report_cycle(cycle: &argon_device::ups_service::Cycle, held_logged: &mut bool) {
    // Logged once per episode: a daemon permanently holding a shutdown must not look like a
    // daemon with nothing to do.
    if let Advice::HeldForUptime { remaining } = cycle.poll.decision.advice {
        if !*held_logged {
            eprintln!(
                "argond: ups: battery critical, but holding the shutdown for another {}s after \
                 boot",
                remaining.as_secs()
            );
            *held_logged = true;
        }
    } else {
        *held_logged = false;
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

/// Runs a clock check and adds what it did to the drift record, if there is one.
/// Checks the UPS clock, records the result, and says how long until the next check.
///
/// Shorter while the system clock is unsynchronised: nothing can be corrected until it is, and
/// on a fresh boot that is a matter of minutes, not of the usual six hours.
fn check_and_record_clock(
    m: &mut UpsMonitor<Gate<SerialLink>>,
    clock_writes: bool,
    record: Option<&std::path::Path>,
) -> Duration {
    let synced = clock_sync::system_clock_synced();
    let entry = check_clock(m, clock_writes, synced);
    if let (Some(entry), Some(path)) = (entry, record) {
        if let Err(e) = drift::append(path, entry) {
            eprintln!(
                "argond: ups: cannot add to the drift record {}: {e}",
                path.display()
            );
        }
    }
    clock_sync::next_check_after(synced)
}

/// Where the drift record lives: `clock.log` in the state directory systemd gives the unit.
///
/// `None` outside systemd -- a hand-started argond keeps no record, rather than writing one into
/// whatever directory it was started from.
fn drift_record_path() -> Option<PathBuf> {
    let dirs = std::env::var_os("STATE_DIRECTORY")?;
    // systemd separates several state directories with colons; this unit declares one.
    let first = dirs.to_string_lossy().split(':').next()?.to_owned();
    (!first.is_empty()).then(|| PathBuf::from(first).join("clock.log"))
}

/// Writes the status file, logging a failure once rather than on every poll.
pub(crate) fn publish(
    status: &argon_device::status::UpsStatus,
    path: &std::path::Path,
    error_logged: &mut bool,
) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match status.write_atomic(path) {
        Ok(()) => *error_logged = false,
        Err(e) if !*error_logged => {
            eprintln!("argond: ups: cannot write {}: {e}", path.display());
            *error_logged = true;
        }
        Err(_) => {}
    }
}

/// Answers every waiting control request.
fn serve_requests(
    requests: &Requests,
    mut monitor: Option<&mut UpsMonitor<Gate<SerialLink>>>,
    wake_writes: bool,
) {
    // Cleared before draining, so a request sent while draining sets it again and is not left
    // waiting a whole interval.
    requests.waiting.store(false, Ordering::Relaxed);
    while let Ok((req, reply)) = requests.rx.try_recv() {
        let answer = match monitor.as_deref_mut() {
            Some(m) => handle_request(m, &req, &mut Logind, wake_writes, unix_now()),
            None => Response::error("the UPS is not reachable right now; nothing was done"),
        };
        let _ = reply.send(answer);
    }
}

/// What the wake safety net last reported, so it logs changes rather than every check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// Nothing reported yet.
    Unknown,
    /// No schedule.
    Nothing,
    /// A schedule at this time.
    At(argon_proto::ups::UpsTime),
}

/// Handles one control request. Runs on the UPS thread, which owns the port.
///
/// For a wake: set it, read it back, and only then schedule the poweroff. If anything fails
/// after the wake was written, it is parked again before answering -- the one outcome this must
/// never produce is a live schedule on a machine that is not going to power off.
fn handle_request<P: PowerControl>(
    m: &mut UpsMonitor<Gate<SerialLink>>,
    req: &Request,
    power: &mut P,
    wake_writes: bool,
    now_unix: u64,
) -> Response {
    let Request::PoweroffWithWake { at_unix } = *req;
    if !wake_writes {
        return Response::error(
            "a scheduled wake is a full-mode operation, and argond is not in mode \"full\"",
        );
    }
    let t = match wake::schedule_for(at_unix, now_unix) {
        Ok(t) => t,
        Err(e) => return Response::error(e.to_string()),
    };
    if let Err(e) = m.ups_mut().set_wake(t) {
        return Response::error(format!(
            "setting the wake failed ({e}); nothing was scheduled and the machine stays on"
        ));
    }
    match m.ups_mut().wake() {
        Ok(Some(got)) if wake::same_minute(got, t) => {}
        other => {
            let parked = park(m);
            return Response::error(format!(
                "the wake did not read back as set ({other:?}); {parked}; the machine stays on"
            ));
        }
    }
    match power.schedule_poweroff(wake::POWEROFF_DELAY, &wake::poweroff_message(t)) {
        Ok(at) => {
            let wake_unix = t.to_unix_seconds().unwrap_or(0);
            let poweroff_unix = at
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            eprintln!(
                "argond: ups: wake set for unix {wake_unix}, poweroff scheduled for unix \
                 {poweroff_unix}, on request"
            );
            Response::PoweroffScheduled {
                wake_unix,
                poweroff_unix,
            }
        }
        Err(e) => {
            let parked = park(m);
            Response::error(format!(
                "the poweroff could not be scheduled ({e}); {parked}, so it cannot come due on a \
                 running machine"
            ))
        }
    }
}

/// Moves the wake schedule to [`wake::PARK_AT`], and says how that went.
fn park(m: &mut UpsMonitor<Gate<SerialLink>>) -> String {
    match m.ups_mut().set_wake(wake::PARK_AT) {
        Ok(()) => "the wake has been parked (moved to 2097)".into(),
        Err(e) => format!("PARKING THE WAKE ALSO FAILED ({e}) -- check it with `argonctl rtc`"),
    }
}

/// The safety net: parks a schedule that would come due, or has passed, on this running machine.
///
/// Logs only on a change, since it runs every few minutes.
fn check_wake(
    m: &mut UpsMonitor<Gate<SerialLink>>,
    park_allowed: bool,
    now_unix: u64,
    logged: &mut Seen,
) {
    let found = match m.ups_mut().wake() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("argond: ups: wake check: reading the schedule failed: {e}");
            return;
        }
    };
    match wake::assess(found, now_unix) {
        Assessment::Nothing | Assessment::Leave { .. } => {
            let now_seen = found.map_or(Seen::Nothing, Seen::At);
            if *logged != now_seen {
                match found {
                    None => eprintln!("argond: ups: no wake schedule set"),
                    Some(t) => eprintln!(
                        "argond: ups: wake schedule {:04}-{:02}-{:02} {:02}:{:02} UTC, far enough \
                         away to leave",
                        t.year, t.month, t.day, t.hour, t.minute
                    ),
                }
                *logged = now_seen;
            }
        }
        Assessment::Park { found, why } if park_allowed => {
            let result = park(m);
            eprintln!(
                "argond: ups: a wake schedule for {:04}-{:02}-{:02} {:02}:{:02} UTC was {why:?} on a \
                 running machine; {result}",
                found.year, found.month, found.day, found.hour, found.minute
            );
            *logged = Seen::Unknown;
        }
        Assessment::Park { found, why } => eprintln!(
            "argond: ups: WARNING: a wake schedule for {:04}-{:02}-{:02} {:02}:{:02} UTC is {why:?} \
             on a running machine, and parking it needs mode \"full\"",
            found.year, found.month, found.day, found.hour, found.minute
        ),
    }
}

/// Compares the UPS clock with the system clock, and sets it if allowed and needed.
///
/// Every outcome is logged with the offset found, in sync or not, which is the only drift
/// measurement there is. It lasts only as long as the journal does -- until reboot on
/// Raspberry Pi OS, whose journal is volatile. A failure is logged and left for the next check.
fn check_clock(
    m: &mut UpsMonitor<Gate<SerialLink>>,
    clock_writes: bool,
    system_synced: bool,
) -> Option<drift::Entry> {
    let ups_clock = match m.ups_mut().clock() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("argond: ups: clock check: reading the clock failed: {e}");
            return None;
        }
    };
    let checked_at = unix_now();
    let entry = |offset_s, action| {
        Some(drift::Entry {
            unix: checked_at,
            offset_s,
            action,
        })
    };
    match clock_sync::judge(ups_clock, checked_at, system_synced) {
        Verdict::InSync { offset_s } => {
            eprintln!("argond: ups: clock offset {offset_s:+} s, within tolerance");
            entry(offset_s, drift::Action::InSync)
        }
        Verdict::Untrusted { offset_s } => {
            eprintln!(
                "argond: ups: clock offset {offset_s:+} s; not correcting it, because the system \
                 clock is not NTP-synchronised"
            );
            entry(offset_s, drift::Action::LeftAlone)
        }
        Verdict::Implausible => {
            eprintln!("argond: ups: the clock read back as an impossible date; not acting on it");
            None
        }
        Verdict::Correct { offset_s } if !clock_writes => {
            eprintln!(
                "argond: ups: clock offset {offset_s:+} s; not correcting it (clock sync is off)"
            );
            entry(offset_s, drift::Action::LeftAlone)
        }
        Verdict::Correct { offset_s } => {
            clock_sync::sleep_to_next_second();
            let now = argon_proto::ups::UpsTime::from_unix_seconds(unix_now())?;
            if let Err(e) = m.ups_mut().set_clock(now) {
                eprintln!("argond: ups: clock was {offset_s:+} s off; setting it failed: {e}");
                return entry(offset_s, drift::Action::LeftAlone);
            }
            let after = m
                .ups_mut()
                .clock()
                .ok()
                .map(|t| clock_sync::judge(t, unix_now(), true));
            if let Some(Verdict::InSync { offset_s: after_s }) = after {
                eprintln!(
                    "argond: ups: clock was {offset_s:+} s off; set from the system clock, now \
                     {after_s:+} s"
                );
                entry(offset_s, drift::Action::Corrected { after_s })
            } else {
                eprintln!(
                    "argond: ups: clock was {offset_s:+} s off; set it, but the read-back is \
                     {after:?}"
                );
                // Not recorded as a correction: without a trusted read-back there is no known
                // starting offset for the next free-running stretch.
                entry(offset_s, drift::Action::LeftAlone)
            }
        }
    }
}

/// Seconds since the unix epoch, or 0 if the clock is before it.
pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Sleeps in short slices so a stop request is honoured promptly.
pub(crate) fn sleep_unless_stopping(total: Duration, stopping: &AtomicBool) {
    sleep_unless(total, stopping, &AtomicBool::new(false));
}

/// Sleeps in short slices, ending early on a stop or when a request is waiting.
pub(crate) fn sleep_unless(total: Duration, stopping: &AtomicBool, request_waiting: &AtomicBool) {
    let slice = Duration::from_millis(250);
    let mut left = total;
    while !left.is_zero()
        && !stopping.load(Ordering::Relaxed)
        && !request_waiting.load(Ordering::Relaxed)
    {
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

        let _ = check_clock(&mut m, clock_writes, synced);

        let after = m.ups_mut().clock().expect("read the clock back");
        stop.store(true, Ordering::Relaxed);
        drop(m);
        let _ = sim.join();
        i64::try_from(after.to_unix_seconds().unwrap()).unwrap()
            - i64::try_from(unix_now()).unwrap()
    }

    /// A fake logind: records schedules, can be made to refuse. Nothing here powers off.
    #[derive(Default)]
    struct FakePower {
        schedules: Vec<(Duration, String)>,
        refuse: bool,
    }

    impl PowerControl for FakePower {
        fn schedule_poweroff(
            &mut self,
            delay: Duration,
            message: &str,
        ) -> argon_hal::Result<SystemTime> {
            if self.refuse {
                return Err(argon_hal::Error::Io(std::io::Error::other(
                    "polkit: not authorised",
                )));
            }
            self.schedules.push((delay, message.to_owned()));
            Ok(SystemTime::now() + delay)
        }
        fn cancel(&mut self) -> argon_hal::Result<()> {
            Ok(())
        }
        fn pending(&mut self) -> argon_hal::Result<Option<SystemTime>> {
            Ok(None)
        }
        fn wall_message(&mut self) -> argon_hal::Result<Option<String>> {
            Ok(None)
        }
    }

    /// Runs `body` against a simulated UPS with the given wake schedule, and returns what the
    /// schedule is afterwards.
    fn with_ups<T>(
        wake: Option<UpsTime>,
        writes: Writes,
        body: impl FnOnce(&mut UpsMonitor<Gate<SerialLink>>) -> T,
    ) -> (T, Option<UpsTime>) {
        let state = UpsState {
            wake,
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
        let policy = BatteryPolicy::new(argon_proto::ups::policy::PolicyConfig::default()).unwrap();
        let mut m = UpsMonitor::new(Ups::new(Gate::with_writes(link, writes)), policy);
        let out = body(&mut m);
        let after = m.ups_mut().wake().expect("read the schedule back");
        stop.store(true, Ordering::Relaxed);
        drop(m);
        let _ = sim.join();
        (out, after)
    }

    const ALL_WRITES: Writes = Writes {
        clock: true,
        wake: true,
    };

    fn minutes_from_now(min: u64) -> (u64, UpsTime) {
        let at = unix_now() + min * 60;
        let floored = at - at % 60;
        let mut t = UpsTime::from_unix_seconds(floored).unwrap();
        t.second = None;
        (at, t)
    }

    #[test]
    fn a_wake_request_sets_the_wake_then_schedules_the_poweroff() {
        let (at, expected) = minutes_from_now(30);
        let mut power = FakePower::default();
        let (resp, after) = with_ups(None, ALL_WRITES, |m| {
            handle_request(
                m,
                &Request::PoweroffWithWake { at_unix: at },
                &mut power,
                true,
                unix_now(),
            )
        });
        assert!(
            matches!(resp, Response::PoweroffScheduled { .. }),
            "{resp:?}"
        );
        assert!(
            after.is_some_and(|t| wake::same_minute(t, expected)),
            "wake is {after:?}"
        );
        assert_eq!(power.schedules.len(), 1);
        assert_eq!(power.schedules[0].0, wake::POWEROFF_DELAY);
    }

    #[test]
    fn a_wake_too_soon_is_refused_and_nothing_is_written() {
        let (at, _) = minutes_from_now(8);
        let mut power = FakePower::default();
        let (resp, after) = with_ups(None, ALL_WRITES, |m| {
            handle_request(
                m,
                &Request::PoweroffWithWake { at_unix: at },
                &mut power,
                true,
                unix_now(),
            )
        });
        assert!(matches!(resp, Response::Error { .. }));
        assert_eq!(after, None, "a refused request wrote a schedule");
        assert!(power.schedules.is_empty());
    }

    #[test]
    fn if_the_poweroff_fails_the_wake_is_parked_again() {
        // The one outcome that must never happen: a live schedule on a machine that is not
        // going to power off.
        let (at, _) = minutes_from_now(30);
        let mut power = FakePower {
            refuse: true,
            ..FakePower::default()
        };
        let (resp, after) = with_ups(None, ALL_WRITES, |m| {
            handle_request(
                m,
                &Request::PoweroffWithWake { at_unix: at },
                &mut power,
                true,
                unix_now(),
            )
        });
        assert!(matches!(resp, Response::Error { .. }));
        assert_eq!(
            after,
            Some(wake::PARK_AT),
            "left a live schedule behind: {after:?}"
        );
    }

    #[test]
    fn without_full_mode_a_wake_is_refused_before_anything_is_sent() {
        let (at, _) = minutes_from_now(30);
        let mut power = FakePower::default();
        let (resp, after) = with_ups(None, Writes::default(), |m| {
            handle_request(
                m,
                &Request::PoweroffWithWake { at_unix: at },
                &mut power,
                false,
                unix_now(),
            )
        });
        assert!(matches!(resp, Response::Error { .. }));
        assert_eq!(after, None);
        assert!(power.schedules.is_empty());
    }

    #[test]
    fn the_safety_net_parks_a_schedule_close_to_due() {
        let (_, soon) = minutes_from_now(6);
        let ((), after) = with_ups(Some(soon), ALL_WRITES, |m| {
            check_wake(m, true, unix_now(), &mut Seen::Unknown);
        });
        assert_eq!(after, Some(wake::PARK_AT));
    }

    #[test]
    fn the_safety_net_parks_a_past_schedule() {
        let past = UpsTime::from_unix_seconds(unix_now() - 3_600).map(|mut t| {
            t.second = None;
            t
        });
        let ((), after) = with_ups(past, ALL_WRITES, |m| {
            check_wake(m, true, unix_now(), &mut Seen::Unknown);
        });
        assert_eq!(after, Some(wake::PARK_AT));
    }

    #[test]
    fn the_safety_net_leaves_a_distant_schedule_and_cannot_park_without_permission() {
        let (_, later) = minutes_from_now(60);
        let ((), after) = with_ups(Some(later), ALL_WRITES, |m| {
            check_wake(m, true, unix_now(), &mut Seen::Unknown);
        });
        assert_eq!(
            after,
            Some(later),
            "moved a schedule that was far enough away"
        );

        let (_, soon) = minutes_from_now(6);
        let ((), after) = with_ups(Some(soon), Writes::default(), |m| {
            check_wake(m, false, unix_now(), &mut Seen::Unknown);
        });
        assert_eq!(after, Some(soon), "wrote without permission");
    }

    #[test]
    fn the_drift_record_gets_what_the_check_did() {
        // The simulator's clock starts days away from now, so a check always has to act.
        let (entry, _) = with_ups(None, ALL_WRITES, |m| check_clock(m, true, true));
        let entry = entry.expect("a check that corrected recorded nothing");
        match entry.action {
            drift::Action::Corrected { after_s } => {
                assert!(
                    after_s.abs() <= clock_sync::THRESHOLD_S,
                    "read back {after_s:+} s"
                );
                assert!(
                    entry.offset_s.abs() > 3_600,
                    "recorded the wrong offset: {entry:?}"
                );
            }
            other => panic!("expected a correction, recorded {other:?}"),
        }

        let (entry, _) = with_ups(None, Writes::default(), |m| check_clock(m, false, true));
        assert_eq!(
            entry.map(|e| e.action),
            Some(drift::Action::LeftAlone),
            "a check without clock writes must not be recorded as a correction"
        );
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
