// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl rtc` — the UPS real-time clock, and the T15 experiment that confirms how to set it.
//!
//! Reading the clock is `observed` (`ARGON-UPS-CMD5`). Setting it is only `inferred`
//! (`ARGON-UPS-CMD3`), and the clean-room rule is that an inferred fact may not back a write
//! path. This module therefore has exactly one write, and it is the experiment that would
//! promote the fact: `--t15 --write`, run once, by an operator, with its whole exchange printed
//! as evidence. Nothing else here sends anything but queries.
//!
//! # The experiment
//!
//! Setting the clock to the current time proves nothing: if it was already close, a correct
//! read-back is indistinguishable from a write the device ignored. So the clock is first set
//! to a deliberately *wrong*, distinctive time and read back; only then is it set to the real
//! time. That is a controlled perturbation with its own negative control built in -- the
//! baseline reads beforehand show what the clock does when nobody writes to it.

use argon_device::config::{Config, DEFAULT_PATH};
use argon_hal::mode::Mode;
use argon_hal::serial::SerialLink;
use argon_hal::{discovery, foreign};
use argon_proto::ups::{Command, Frame, FrameReader, UpsTime};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How far from the truth the distinctive target is set: 1 h 17 min 29 s behind.
///
/// Distinctive rather than round, so a read-back matching it cannot be a coincidence, a
/// time-zone confusion (whole hours) or a stale value.
const T15_OFFSET_S: u64 = 3_600 + 17 * 60 + 29;

/// How closely a read-back must match its expected value, in seconds. The device counts in
/// whole seconds and the write lands at an unknown phase within one.
const TOLERANCE_S: i64 = 2;

/// How long to listen for whatever the device sends after a set.
const LISTEN: Duration = Duration::from_millis(1_500);

#[derive(clap::Args)]
pub struct Args {
    /// Run task T15: set the clock to a deliberately wrong time, read it back, then set it to
    /// the correct time and read that back too. Requires --write.
    #[arg(long)]
    pub t15: bool,

    /// Run task T17: set the wake schedule to one far-future time and read it back, then to a
    /// different far-future time and read that back. Requires --write.
    #[arg(long, conflicts_with = "t15")]
    pub t17: bool,

    /// Permit the writes of T15 or T17. Without it this command only queries.
    #[arg(long)]
    pub write: bool,

    /// Serial port, or "auto" to find the UPS by its USB identity.
    #[arg(long, default_value = "auto")]
    pub port: String,
}

pub fn run(args: &Args) -> ExitCode {
    let experiment = args.t15 || args.t17;
    if experiment && !args.write {
        eprintln!(
            "argonctl: --t15 and --t17 write to the UPS and need --write as well. Read \
             docs/testing/HUMAN-TASKS.md first."
        );
        return ExitCode::FAILURE;
    }
    if args.write && !experiment {
        eprintln!("argonctl: --write only means something with --t15 or --t17");
        return ExitCode::FAILURE;
    }

    let Some(path) = resolve_port(&args.port) else {
        eprintln!("argonctl: no Argon UPS serial port found");
        return ExitCode::FAILURE;
    };
    if let Err(why) = port_is_free(&path) {
        eprintln!("argonctl: {why}");
        return ExitCode::FAILURE;
    }
    let mut link = match SerialLink::open(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("argonctl: cannot open {}: {e}", path.display());
            if matches!(&e, argon_hal::Error::Io(io) if io.to_string().contains("ermission")) {
                eprintln!(
                    "argonctl: the packaged service gives this port to group `argon`; run this \
                     under sudo."
                );
            }
            return ExitCode::FAILURE;
        }
    };

    println!("UPS on {}", path.display());
    println!();
    println!("Baseline: three reads, nothing written");
    let baseline = match read_offsets(&mut link, 3) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("argonctl: reading the clock failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let wake = read_wake(&mut link);
    match &wake {
        Ok(None) => println!("  wake schedule    none"),
        Ok(Some(t)) => println!("  wake schedule    {}", fmt(*t)),
        Err(e) => println!("  wake schedule    unreadable: {e}"),
    }

    if args.t17 {
        return run_t17(&mut link, &wake);
    }
    if !args.t15 {
        return ExitCode::SUCCESS;
    }
    if let Err(why) = t15_preconditions(&wake, &baseline) {
        eprintln!("\nargonctl: T15 refused: {why}");
        return ExitCode::FAILURE;
    }
    let outcome = t15(&mut link);
    report(outcome);
    if outcome.confirmed && outcome.restored {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// What the experiment established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Outcome {
    /// A distinctive wrong time was written and read back.
    confirmed: bool,
    /// The correct time was written afterwards and read back.
    restored: bool,
}

fn resolve_port(port: &str) -> Option<PathBuf> {
    if port == "auto" {
        discovery::argon_ups_serial_path()
    } else {
        Some(PathBuf::from(port))
    }
}

/// Refuses while anything else could be reading the port.
fn port_is_free(path: &std::path::Path) -> Result<(), String> {
    for u in foreign::vendor_units() {
        if u.unit == "argonupsrtcd.service" && u.contends() {
            return Err("argonupsrtcd is running and holds the UPS port. Stop it first.".into());
        }
    }
    let argond = std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "argond.service"])
        .status()
        .is_ok_and(|s| s.success());
    if argond {
        return Err(
            "argond is running and owns the UPS port. For the few seconds this \
             takes, stop it:\n\n    sudo systemctl stop argond\n\nand start it again \
             afterwards. Do this on mains: while it is stopped, nothing watches the battery."
                .into(),
        );
    }
    if let Some(o) = foreign::port_owners(path).first() {
        return Err(format!(
            "{} is held by pid {} ({}). Two readers corrupt each other's frames.",
            path.display(),
            o.pid,
            o.comm
        ));
    }
    if !foreign::can_see_all_processes() {
        eprintln!(
            "argonctl: note: cannot check for other readers as an unprivileged user; run \
             under sudo for that check to mean anything."
        );
    }
    Ok(())
}

/// Reads the clock `n` times, returning each offset from the system clock in seconds (UPS
/// minus system).
fn read_offsets(link: &mut SerialLink, n: usize) -> Result<Vec<i64>, String> {
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ups = read_clock(link)?;
        let sys = now_secs();
        let offset_s = signed_diff(ups.to_unix_seconds().ok_or("implausible clock")?, sys);
        println!(
            "  UPS clock        {}   system {}   offset {offset_s:+} s",
            fmt(ups),
            UpsTime::from_unix_seconds(sys).map_or_else(|| "?".into(), fmt),
        );
        out.push(offset_s);
        if i + 1 < n {
            std::thread::sleep(Duration::from_millis(1_100));
        }
    }
    Ok(out)
}

fn read_clock(link: &mut SerialLink) -> Result<UpsTime, String> {
    let f = link
        .request(Command::GetRtc.as_byte(), &[], deadline())
        .map_err(|e| e.to_string())?;
    let t = UpsTime::decode_clock(f.payload()).map_err(|e| format!("{e:?}"))?;
    if t.is_plausible() {
        Ok(t)
    } else {
        Err(format!(
            "the clock read back as an impossible date: {}",
            fmt(t)
        ))
    }
}

fn read_wake(link: &mut SerialLink) -> Result<Option<UpsTime>, String> {
    let f = link
        .request(Command::GetWake.as_byte(), &[], deadline())
        .map_err(|e| e.to_string())?;
    UpsTime::decode_optional_schedule(f.payload()).map_err(|e| format!("{e:?}"))
}

fn t15_preconditions(
    wake: &Result<Option<UpsTime>, String>,
    baseline: &[i64],
) -> Result<(), String> {
    // RTC writes are a `full`-mode operation, and this is the operator's own statement of
    // what the machine may do -- honour it here as the daemon would.
    let config = Config::load(std::path::Path::new(DEFAULT_PATH))
        .map_err(|e| format!("cannot read {DEFAULT_PATH} to check the mode: {e}"))?;
    if config.mode().unwrap_or_default() != Mode::Full {
        return Err(format!(
            "mode is {:?} in {DEFAULT_PATH}; setting the UPS clock is a full-mode operation",
            config.mode
        ));
    }
    // A wrong clock could move or fire a wake schedule. With none set there is nothing to
    // move; with one set, or unknown, do not risk it.
    match wake {
        Ok(None) => {}
        Ok(Some(t)) => {
            return Err(format!(
                "a wake schedule is set ({}); a wrong clock could move or fire it",
                fmt(*t)
            ));
        }
        Err(e) => return Err(format!("the wake schedule could not be read ({e})")),
    }
    // The restore step sets the UPS from the system clock, so that clock must be right.
    if !argon_device::clock_sync::system_clock_synced() {
        return Err(
            "the system clock is not NTP-synchronised, so restoring the UPS clock \
             from it would make it worse"
                .into(),
        );
    }
    // The baseline must be self-consistent, or the read-back comparison means nothing.
    let spread = baseline.iter().max().unwrap_or(&0) - baseline.iter().min().unwrap_or(&0);
    if spread > TOLERANCE_S {
        return Err(format!(
            "the baseline reads disagree by {spread} s; the clock is not ticking steadily"
        ));
    }
    Ok(())
}

fn t15(link: &mut SerialLink) -> Outcome {
    println!();
    println!(
        "Step 1: set the clock {T15_OFFSET_S} s BEHIND (a deliberately wrong, distinctive time)"
    );
    let wrong_sent_at = now_secs();
    let Some(target) = UpsTime::from_unix_seconds(wrong_sent_at - T15_OFFSET_S) else {
        eprintln!("argonctl: target time out of range");
        return Outcome {
            confirmed: false,
            restored: false,
        };
    };
    let confirmed = match send_set(link, target) {
        Ok(()) => {
            println!();
            println!("Step 2: read it back");
            let offset = -i64::try_from(T15_OFFSET_S).unwrap_or(0);
            check_readback(link, wrong_sent_at, offset)
        }
        Err(e) => {
            eprintln!("argonctl: sending the set failed: {e}");
            false
        }
    };

    // Always restore, whatever step 2 found: if the set did take effect, the clock is now
    // wrong by over an hour and must not be left that way.
    println!();
    println!("Step 3: set the clock to the correct time");
    argon_device::clock_sync::sleep_to_next_second();
    let right_sent_at = now_secs();
    let restored = match UpsTime::from_unix_seconds(right_sent_at) {
        Some(now) => match send_set(link, now) {
            Ok(()) => {
                println!();
                println!("Step 4: read it back");
                check_readback(link, right_sent_at, 0)
            }
            Err(e) => {
                eprintln!("argonctl: sending the restore failed: {e}");
                false
            }
        },
        None => false,
    };
    Outcome {
        confirmed,
        restored,
    }
}

fn report(o: Outcome) {
    println!();
    println!("T15 RESULT");
    if o.confirmed {
        println!("  CONFIRMED: command 3 sets the UPS clock. A distinctive wrong time was written");
        println!("  and read back within {TOLERANCE_S} s of what was sent.");
    } else {
        println!("  NOT CONFIRMED: the read-back did not match the distinctive time sent.");
        println!("  ARGON-UPS-CMD3 stays `inferred`, and no clock write path may be built on it.");
    }
    if o.restored {
        println!("  Clock restored to the correct time.");
    } else if o.confirmed {
        println!("  !! RESTORE NOT CONFIRMED: the UPS clock may still be {T15_OFFSET_S} s behind.");
        println!("  !! Run this again, or report it before anything relies on the UPS clock.");
    }
}

/// Sets the clock and prints everything that comes back, raw and decoded.
fn send_set(link: &mut SerialLink, t: UpsTime) -> Result<(), String> {
    let payload = t.encode_clock().map_err(|e| format!("{e:?}"))?;
    send_frame(link, Command::SetRtc, &payload, &fmt(t))
}

/// Sends one command and prints everything that comes back, raw and decoded.
fn send_frame(
    link: &mut SerialLink,
    cmd: Command,
    payload: &[u8],
    target: &str,
) -> Result<(), String> {
    let frame = Frame::new(cmd.as_byte(), payload).map_err(|e| e.to_string())?;
    let mut out = [0u8; 16];
    let n = frame.encode_into(&mut out).map_err(|e| e.to_string())?;
    println!("  target           {target}");
    println!("  sent             {}", hex(&out[..n]));

    let got = link
        .write_and_collect(&out[..n], LISTEN)
        .map_err(|e| e.to_string())?;
    println!(
        "  received         {} ({} bytes in {} ms)",
        if got.is_empty() {
            "nothing".into()
        } else {
            hex(&got)
        },
        got.len(),
        LISTEN.as_millis()
    );
    let mut reader = FrameReader::new();
    for &b in &got {
        match reader.push(b) {
            Some(Ok(f)) => println!(
                "  decoded          frame: command {}, payload {}",
                f.cmd(),
                if f.payload().is_empty() {
                    "empty".into()
                } else {
                    hex(f.payload())
                }
            ),
            Some(Err(e)) => println!("  decoded          rejected bytes: {e}"),
            None => {}
        }
    }
    Ok(())
}

/// The two wake times T17 writes: distinctive, different from each other in every field, and
/// decades away, so a schedule the UPS might act on cannot come due. If waking works by cutting
/// and restoring the Pi's power -- the likely mechanism for a Pi 5 set to power off on halt --
/// a schedule that fired while the machine was running would be an abrupt power cut.
const T17_FIRST: UpsTime = UpsTime {
    year: 2098,
    month: 7,
    day: 13,
    hour: 6,
    minute: 29,
    second: None,
};
const T17_SECOND: UpsTime = UpsTime {
    year: 2097,
    month: 3,
    day: 21,
    hour: 17,
    minute: 42,
    second: None,
};

// Checked at compile time, so an edit that brings a T17 time within reach -- or makes the two
// times share a field, so the second read-back could be a stale copy of the first -- does not
// build.
const _: () = {
    assert!(
        T17_FIRST.year >= 2090 && T17_SECOND.year >= 2090,
        "T17 times must be decades away"
    );
    assert!(T17_FIRST.is_plausible() && T17_SECOND.is_plausible());
    assert!(T17_FIRST.year != T17_SECOND.year && T17_FIRST.month != T17_SECOND.month);
    assert!(T17_FIRST.day != T17_SECOND.day && T17_FIRST.hour != T17_SECOND.hour);
    assert!(T17_FIRST.minute != T17_SECOND.minute);
};

/// What T17 established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WakeOutcome {
    /// The first time was written and read back exactly.
    first: bool,
    /// The second, different, time was written and read back exactly.
    second: bool,
}

fn run_t17(link: &mut SerialLink, wake: &Result<Option<UpsTime>, String>) -> ExitCode {
    if let Err(why) = t17_preconditions(wake) {
        eprintln!("\nargonctl: T17 refused: {why}");
        return ExitCode::FAILURE;
    }
    let outcome = t17(link);
    println!();
    println!("T17 RESULT");
    if outcome.first && outcome.second {
        println!("  CONFIRMED: command 6 sets the wake schedule. Two different far-future times");
        println!("  were written and each read back exactly.");
    } else {
        println!(
            "  NOT CONFIRMED (first {}, second {}).",
            outcome.first, outcome.second
        );
        println!("  ARGON-UPS-CMD6 stays `inferred`, and no wake write path may be built on it.");
    }
    match read_wake(link) {
        Ok(Some(t)) => println!(
            "  The schedule is LEFT at {} -- decades away. No command to clear a schedule is\n  \
             known, and guessing one could leave a near-term time behind.",
            fmt(t)
        ),
        Ok(None) => println!("  The schedule reads as none."),
        Err(e) => println!("  The final schedule could not be read: {e}"),
    }
    if outcome.first && outcome.second {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn t17_preconditions(wake: &Result<Option<UpsTime>, String>) -> Result<(), String> {
    let config = Config::load(std::path::Path::new(DEFAULT_PATH))
        .map_err(|e| format!("cannot read {DEFAULT_PATH} to check the mode: {e}"))?;
    if config.mode().unwrap_or_default() != Mode::Full {
        return Err(format!(
            "mode is {:?} in {DEFAULT_PATH}; setting a wake schedule is a full-mode operation",
            config.mode
        ));
    }
    // Never overwrite a schedule someone set: there is no known way to put it back.
    match wake {
        Ok(None) => Ok(()),
        Ok(Some(t)) => Err(format!(
            "a wake schedule is already set ({}); it would be overwritten and cannot be restored",
            fmt(*t)
        )),
        Err(e) => Err(format!("the current wake schedule could not be read ({e})")),
    }
}

fn t17(link: &mut SerialLink) -> WakeOutcome {
    let mut results = [false; 2];
    for (i, target) in [T17_FIRST, T17_SECOND].into_iter().enumerate() {
        println!();
        println!("Step {}: set the wake schedule to {}", i + 1, fmt(target));
        let Ok(payload) = target.encode_schedule() else {
            eprintln!("argonctl: cannot encode {}", fmt(target));
            break;
        };
        if let Err(e) = send_frame(link, Command::SetWake, &payload, &fmt(target)) {
            eprintln!("argonctl: sending failed: {e}");
            break;
        }
        results[i] = (0..2).all(|_| {
            std::thread::sleep(Duration::from_millis(500));
            match read_wake(link) {
                Ok(Some(got)) => {
                    let pass = same_minute(got, target);
                    println!(
                        "  read back        {}   {}",
                        fmt(got),
                        if pass { "ok" } else { "MISMATCH" }
                    );
                    pass
                }
                Ok(None) => {
                    println!("  read back        none   MISMATCH");
                    false
                }
                Err(e) => {
                    println!("  read back        failed: {e}");
                    false
                }
            }
        });
    }
    WakeOutcome {
        first: results[0],
        second: results[1],
    }
}

/// Whether two schedule times name the same minute. A schedule has no seconds field.
const fn same_minute(a: UpsTime, b: UpsTime) -> bool {
    a.year == b.year
        && a.month == b.month
        && a.day == b.day
        && a.hour == b.hour
        && a.minute == b.minute
}

/// Reads the clock twice and checks it against what was set, `offset_s` from the system
/// clock at `sent_at`.
fn check_readback(link: &mut SerialLink, sent_at: u64, offset_s: i64) -> bool {
    let mut ok = true;
    for _ in 0..2 {
        std::thread::sleep(Duration::from_millis(1_100));
        let expected = as_i64(sent_at) + offset_s + signed_diff(now_secs(), sent_at);
        match read_clock(link) {
            Ok(t) => {
                let got = t.to_unix_seconds().map_or(0, as_i64);
                let error = got - expected;
                let pass = error.abs() <= TOLERANCE_S;
                println!(
                    "  read back        {}   expected {}   error {error:+} s   {}",
                    fmt(t),
                    u64::try_from(expected)
                        .ok()
                        .and_then(UpsTime::from_unix_seconds)
                        .map_or_else(|| "?".into(), fmt),
                    if pass { "ok" } else { "MISMATCH" }
                );
                ok &= pass;
            }
            Err(e) => {
                println!("  read back        failed: {e}");
                ok = false;
            }
        }
    }
    ok
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `a - b` as a signed number. Unix seconds fit an `i64` for the next 292 billion years.
fn signed_diff(a: u64, b: u64) -> i64 {
    as_i64(a) - as_i64(b)
}

fn as_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

fn fmt(t: UpsTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        t.year,
        t.month,
        t.day,
        t.hour,
        t.minute,
        t.second.unwrap_or(0)
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    //! The experiment against a simulated UPS over a real PTY, including one that ignores the
    //! write. An experiment is only worth running on hardware if it has been seen to answer
    //! "no" as well as "yes".

    use super::*;
    use argon_sim::ups::{Faults, UpsSim, UpsState};
    use serialport::SerialPort;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn with_sim(faults: Faults) -> Outcome {
        with_sim_state(UpsState::default(), faults)
    }

    fn with_sim_state(state: UpsState, faults: Faults) -> Outcome {
        let (master, slave) = serialport::TTYPort::pair().expect("PTY pair");
        let slave_name = slave.name().expect("slave path");
        let stop = Arc::new(AtomicBool::new(false));
        let sim_stop = Arc::clone(&stop);
        let sim = std::thread::spawn(move || {
            let mut master = master;
            let mut sim = UpsSim::with_faults(state, faults);
            let _ = sim.serve_until(
                &mut master,
                Instant::now() + Duration::from_secs(60),
                &sim_stop,
            );
        });
        let _slave = slave;
        let mut link = SerialLink::open(&slave_name).expect("open the simulated port");
        let outcome = t15(&mut link);
        stop.store(true, Ordering::Relaxed);
        drop(link);
        let _ = sim.join();
        outcome
    }

    fn t17_with_sim(faults: Faults) -> WakeOutcome {
        let (master, slave) = serialport::TTYPort::pair().expect("PTY pair");
        let slave_name = slave.name().expect("slave path");
        let stop = Arc::new(AtomicBool::new(false));
        let sim_stop = Arc::clone(&stop);
        let sim = std::thread::spawn(move || {
            let mut master = master;
            let mut sim = UpsSim::with_faults(UpsState::default(), faults);
            let _ = sim.serve_until(
                &mut master,
                Instant::now() + Duration::from_secs(60),
                &sim_stop,
            );
        });
        let _slave = slave;
        let mut link = SerialLink::open(&slave_name).expect("open the simulated port");
        let outcome = t17(&mut link);
        stop.store(true, Ordering::Relaxed);
        drop(link);
        let _ = sim.join();
        outcome
    }

    #[test]
    fn t17_confirms_a_device_that_sets_its_schedule() {
        assert_eq!(
            t17_with_sim(Faults::default()),
            WakeOutcome {
                first: true,
                second: true
            }
        );
    }

    #[test]
    fn t17_does_not_confirm_a_device_that_ignores_the_set() {
        let faults = Faults {
            ignore_wake_set: true,
            ..Faults::default()
        };
        assert_eq!(
            t17_with_sim(faults),
            WakeOutcome {
                first: false,
                second: false
            }
        );
    }

    #[test]
    fn a_device_that_sets_its_clock_is_confirmed_and_restored() {
        assert_eq!(
            with_sim(Faults::default()),
            Outcome {
                confirmed: true,
                restored: true
            }
        );
    }

    #[test]
    fn a_device_that_ignores_the_set_is_not_confirmed() {
        // It still acknowledges the command, as a real device might. Only the read-back can
        // tell the difference, which is the whole point of writing a wrong time first.
        let faults = Faults {
            ignore_clock_set: true,
            ..Faults::default()
        };
        assert_eq!(
            with_sim(faults),
            Outcome {
                confirmed: false,
                restored: false
            }
        );
    }

    #[test]
    fn an_ignored_set_on_an_already_correct_clock_is_still_not_confirmed() {
        // The case a naive "set it to now, read it back" test gets wrong: the clock was right
        // already, so the read-back matches and the ignored write looks like a working one.
        // Writing a distinctive WRONG time first is what catches it. The restore step passes
        // here -- the clock is correct -- and that must not be mistaken for confirmation.
        let state = UpsState {
            clock: UpsTime::from_unix_seconds(now_secs()).expect("now is in range"),
            ..UpsState::default()
        };
        let faults = Faults {
            ignore_clock_set: true,
            ..Faults::default()
        };
        assert_eq!(
            with_sim_state(state, faults),
            Outcome {
                confirmed: false,
                restored: true
            }
        );
    }
}
