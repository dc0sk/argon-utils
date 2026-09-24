// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl ups` — what the UPS is doing.
//!
//! Read-only, by three routes, in this order:
//!
//! 1. **argond over D-Bus** (the default when it is running). argond holds the UPS port, so it
//!    is the one process that can read it; asking it keeps a login out of the `argon` group,
//!    which owns both the serial node and the hidraw node and is what keeps a second writer off
//!    a link that has no arbitration.
//! 2. **hidraw** (`--device`), Input reports only: it claims no USB interface and cannot
//!    disturb whatever holds the serial port. Needs access to the node.
//! 3. **the serial protocol** (`--serial`), which only works when nothing else holds the port.

use argon_device::config::Config;
use argon_device::control::{BUS_NAME, INTERFACE, OBJECT_PATH};
use argon_device::status;
use argon_device::ups::{QueryOnly, Ups, UpsMonitor};
use argon_device::ups_seen::{self, SeenUps};
use argon_hal::{discovery, foreign, hidraw, platform, serial};
use argon_proto::hid::{ItemKind, ReportDescriptor, usage};
use argon_proto::ups::PowerSource;
use argon_proto::ups::policy::{Advice, BatteryPolicy};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags: clap derives one field per flag"
)]
pub struct Args {
    /// How long to listen for reports. The device sends on change, so a short window may
    /// see only some fields.
    #[arg(long, default_value = "3", value_name = "SECONDS")]
    pub wait: u64,

    /// Show every field the descriptor declares, not just the summary.
    #[arg(long)]
    pub all: bool,

    /// Keep polling over the serial protocol, showing the battery policy's decisions.
    ///
    /// Monitoring only: when the policy advises shutdown this prints what it would do and
    /// does nothing. Requires `--serial`.
    #[arg(long, requires = "serial")]
    pub watch: bool,

    /// With `--watch`, stop after this many polls. 0 means until interrupted.
    #[arg(long, default_value = "0")]
    pub count: u64,

    /// Configuration file for the poll interval and battery thresholds. Defaults are used if
    /// it does not exist.
    #[arg(long, value_name = "PATH")]
    pub config: Option<std::path::PathBuf>,

    /// Machine-readable output: one JSON object, or one per poll with `--watch`.
    #[arg(long)]
    pub json: bool,

    /// Read the device directly instead of asking argond.
    ///
    /// Uses hidraw, which needs access to the node -- so this normally wants `sudo`, because
    /// the node belongs to the `argon` group that argond runs as.
    #[arg(long, conflicts_with = "serial")]
    pub device: bool,

    /// Read over the Argon serial protocol instead of HID.
    ///
    /// Pass a port path, or `auto` to use the discovered UPS. The vendor's daemon holds the
    /// port continuously and CDC-ACM has no arbitration, so this refuses to run while
    /// another process owns it.
    #[arg(long, value_name = "PATH_OR_AUTO")]
    pub serial: Option<String>,

    /// Forget the UPS argond saw before, so its absence stops being reported.
    ///
    /// For a UPS removed on purpose. argond remembers the last UPS it read so that one coming
    /// unplugged is noticed; this deletes that record. Needs root, since the record belongs to
    /// argond.
    #[arg(long, conflicts_with_all = ["serial", "device", "watch"])]
    pub forget: bool,
}

/// A decoded telemetry value.
struct Reading {
    label: &'static str,
    value: String,
}

pub fn run(args: &Args) -> ExitCode {
    if args.forget {
        return forget(std::path::Path::new(ups_seen::DEFAULT_PATH));
    }
    if let Some(port) = &args.serial {
        return run_serial(port, args);
    }
    if !args.device {
        if let Some(fields) = from_daemon() {
            if args.json {
                println!("{}", json_from_daemon(&fields, now_unix()));
            } else {
                print!("{}", render(&fields, now_unix()));
            }
            return ExitCode::SUCCESS;
        }
        eprintln!("argonctl: argond is not on the system bus; reading the device directly\n");
    }
    let (mut dev, desc, raw_len, serial) = match open_ups() {
        Ok(v) => v,
        Err(code) => return code,
    };

    if args.json {
        return device_json(&mut dev, &desc, serial.as_deref(), args.wait);
    }

    println!("Device");
    println!("------");
    println!("  node        {}", dev.path().display());
    println!(
        "  hid name    {}",
        dev.hid_name().unwrap_or_else(|| "-".into())
    );
    println!("  serial      {}", serial.as_deref().unwrap_or("-"));
    println!(
        "  descriptor  {raw_len} bytes, {} fields",
        desc.fields().len()
    );

    let deadline = Instant::now() + Duration::from_secs(args.wait);
    println!("\nListening {}s for reports...", args.wait);
    let reports = match dev.collect_until(deadline) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("argonctl: read failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if reports.is_empty() {
        println!(
            "\n  No Input reports arrived.\n\n  \
             On firmware 113 this is expected: the HID interface publishes a complete Power\n  \
             Device descriptor but serves no data through it. Feature reads return only the\n  \
             echoed report ID, and nothing arrives on the Input stream while idle. See\n  \
             docs/protocol/captures/OBS-2026-09-15-ups-hid-is-dormant.md.\n\n  \
             Live telemetry comes from the serial protocol instead. The one untested case is\n  \
             a state change: removing mains power may force a report. If you can do that\n  \
             while this is running, the result settles it."
        );
        return ExitCode::SUCCESS;
    }

    let (readings, status) = decode(&desc, &reports);

    println!("\nTelemetry");
    println!("---------");
    if readings.is_empty() && status.is_empty() {
        println!("  (reports arrived but carried no recognised fields)");
    }
    for r in &readings {
        println!("  {:<13} {}", r.label, r.value);
    }
    if !status.is_empty() {
        println!("  {:<13} {}", "status", status.join(", "));
    }

    println!("\nReports seen: {}", fmt_ids(&reports));

    if args.all {
        println!("\nAll declared fields");
        println!("-------------------");
        for f in desc.fields() {
            println!(
                "  rpt {:#04x} {:<8} page {:#04x} usage {:#04x} @bit {:>3} x{:<2} range {}..={}{}{}",
                f.report_id,
                format!("{:?}", f.kind),
                f.usage_page,
                f.usage,
                f.bit_offset,
                f.bit_size,
                f.logical_min,
                f.logical_max,
                if f.is_constant() { " const" } else { "" },
                if f.is_volatile() { " volatile" } else { "" },
            );
        }
    }

    ExitCode::SUCCESS
}

/// Deletes argond's record of the UPS it saw last.
fn forget(path: &std::path::Path) -> ExitCode {
    let name = SeenUps::load(path).map(|u| u.name);
    match std::fs::remove_file(path) {
        Ok(()) => {
            println!(
                "Forgot {}. argond reports no UPS now, and records the next one it reads.",
                name.as_deref().unwrap_or("the UPS")
            );
            ExitCode::SUCCESS
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!("No UPS is remembered; nothing to forget.");
            ExitCode::SUCCESS
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!(
                "argonctl: {} belongs to argond; run this with sudo.",
                path.display()
            );
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("argonctl: cannot remove {}: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// Asks argond what it last read, or `None` when it is not on the bus.
///
/// Anything short of an answer is a `None`: no system bus, no service, an older argond without
/// the method. The caller then falls back to the device, so a missing daemon is not an error.
fn from_daemon() -> Option<std::collections::HashMap<String, String>> {
    let conn = zbus::blocking::Connection::system().ok()?;
    let proxy = zbus::blocking::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).ok()?;
    proxy
        .call::<_, _, std::collections::HashMap<String, String>>("UpsStatus", &())
        .ok()
}

/// Formats argond's answer.
///
/// Unknown keys are ignored and missing ones read as "-", so a newer daemon reporting more, or
/// an older one reporting less, still prints something truthful.
fn render(fields: &std::collections::HashMap<String, String>, now_unix: u64) -> String {
    use std::fmt::Write as _;
    let get = |k: &str| fields.get(k).map_or("", String::as_str);
    let mut out = String::new();
    let _ = writeln!(out, "From argond\n-----------");
    if get("available") != "yes" {
        // "Not monitoring" and "no reading yet" look identical from here unless the daemon
        // says which. The first is a configuration choice and perfectly healthy; reporting it
        // as an absence reads like a broken daemon, which is how this line was first written.
        let _ = match get("source_config") {
            "none" => writeln!(
                out,
                "  argond is running, and UPS monitoring is off by configuration\n  \
                 ([ups] source = \"none\" in /etc/argon-utils/config.toml). Nothing is wrong."
            ),
            "" => writeln!(
                out,
                "  argond is running but has no reading yet: it has not completed its first\n  \
                 poll, or the battery is unreachable."
            ),
            source => writeln!(
                out,
                "  argond is running and configured to monitor {source:?}, but has no reading\n  \
                 yet: either its first poll has not finished, or that source is unreachable.\n  \
                 `journalctl -u argond -n 20` says which."
            ),
        };
        return out;
    }
    match get("level") {
        status::ABSENT => {
            let _ = writeln!(
                out,
                "  No UPS connected. argond keeps looking and picks one up when it is plugged in."
            );
            return out;
        }
        status::MISSING => {
            let seen = get("last_seen_unix")
                .parse::<u64>()
                .ok()
                .map_or_else(String::new, |t| {
                    format!(", last read {} min ago", now_unix.saturating_sub(t) / 60)
                });
            let _ = writeln!(
                out,
                "  The UPS connected before ({}{seen}) cannot be found.\n  \
                 No battery is being watched. If it was removed on purpose:\n  \
                 sudo argonctl ups --forget",
                dash(get("missing_name"))
            );
            return out;
        }
        _ => {}
    }
    let percent = match get("percent") {
        "" => "unknown".to_owned(),
        p => format!("{p}%"),
    };
    let _ = writeln!(out, "  {:<14} {percent}", "charge");
    let _ = writeln!(out, "  {:<14} {}", "source", dash(get("source")));
    let _ = writeln!(out, "  {:<14} {}", "level", dash(get("level")));
    let _ = writeln!(out, "  {:<14} {}", "last read", age(get("age_s")));
    let _ = writeln!(
        out,
        "  {:<14} {}",
        "poweroff",
        poweroff(get("shutdown_at_unix"), now_unix)
    );
    out
}

/// How old a reading may be before it is called stale, in seconds.
///
/// Two poll intervals and a bit: long enough that a slow poll is not an alarm, short enough
/// that a thread which stopped is noticed.
const STALE_AFTER_S: u64 = 120;

/// argond's answer as JSON.
///
/// Numbers are numbers and an absent value is `null`, never `0` or `""` -- a monitor that
/// cannot tell "no reading" from "0 %" is worse than no monitor. `stale` applies the same
/// threshold as the text output, so the two cannot disagree.
fn json_from_daemon(fields: &std::collections::HashMap<String, String>, now_unix: u64) -> String {
    let get = |k: &str| fields.get(k).map_or("", String::as_str);
    let num = |k: &str| get(k).parse::<u64>().ok();
    let available = get("available") == "yes";
    let age = num("age_s");
    let at = num("shutdown_at_unix");
    let value = serde_json::json!({
        "route": "argond",
        "available": available,
        "level": opt(get("level")),
        "source": opt(get("source")),
        "percent": get("percent").parse::<u8>().ok(),
        "updated_unix": num("updated_unix"),
        "age_s": age,
        "stale": age.is_some_and(|a| a >= STALE_AFTER_S),
        "shutdown_at_unix": at,
        "shutdown_in_s": at.map(|t| t.saturating_sub(now_unix)),
        "missing_name": opt(get("missing_name")),
        "last_seen_unix": num("last_seen_unix"),
    });
    value.to_string()
}

/// The hidraw route, reported as one JSON object.
fn device_json(
    dev: &mut hidraw::HidRaw,
    desc: &ReportDescriptor,
    serial: Option<&str>,
    wait: u64,
) -> ExitCode {
    let deadline = Instant::now() + Duration::from_secs(wait);
    let reports = match dev.collect_until(deadline) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("argonctl: read failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (readings, status) = decode(desc, &reports);
    println!(
        "{}",
        json_from_device(
            &dev.path().display().to_string(),
            serial,
            &readings,
            &status
        )
    );
    ExitCode::SUCCESS
}

/// The hidraw route as JSON.
///
/// `readings` is an object of what the descriptor actually yielded, so a firmware that serves
/// nothing -- which is what firmware 113 does -- produces an empty object rather than invented
/// zeroes.
fn json_from_device(
    node: &str,
    serial: Option<&str>,
    readings: &[Reading],
    status: &[&'static str],
) -> String {
    let mut map = serde_json::Map::new();
    for r in readings {
        map.insert(r.label.to_owned(), serde_json::Value::from(r.value.clone()));
    }
    serde_json::json!({
        "route": "hidraw",
        "node": node,
        "serial": serial,
        "readings": map,
        "status": status,
    })
    .to_string()
}

/// `None` for an empty string, so a missing field is `null` rather than `""`.
fn opt(v: &str) -> Option<&str> {
    (!v.is_empty()).then_some(v)
}

fn dash(v: &str) -> &str {
    if v.is_empty() { "-" } else { v }
}

/// How long ago the reading was taken, called out when it is old enough to distrust.
fn age(age_s: &str) -> String {
    let Ok(secs) = age_s.parse::<u64>() else {
        return "-".to_owned();
    };
    let when = if secs < 60 {
        format!("{secs}s ago")
    } else {
        format!("{}m {}s ago", secs / 60, secs % 60)
    };
    // A status that stopped being updated looks exactly like a healthy one otherwise.
    if secs >= 120 {
        format!("{when}  (stale -- is argond still polling?)")
    } else {
        when
    }
}

/// A pending poweroff, as the time left rather than a timestamp to subtract in your head.
fn poweroff(at_unix: &str, now_unix: u64) -> String {
    let Ok(at) = at_unix.parse::<u64>() else {
        return "none scheduled".to_owned();
    };
    let left = at.saturating_sub(now_unix);
    format!("scheduled in {}m {}s", left / 60, left % 60)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads the UPS over the Argon serial protocol, once or continuously.
///
/// Every request goes through `QueryOnly`, so nothing that changes the UPS's state can be
/// sent from here, whatever this function does.
/// Resolves the port to use, and refuses when something else holds it.
///
/// Read as one question -- may this process talk to the UPS over serial? -- because every
/// answer has the same consequence: two readers on a CDC-ACM port corrupt each other's frames.
fn serial_path(port: &str) -> Result<std::path::PathBuf, ExitCode> {
    let path = if port == "auto" {
        let Some(p) = discovery::argon_ups_serial_path() else {
            eprintln!("argonctl: no Argon UPS serial port found");
            return Err(ExitCode::FAILURE);
        };
        p
    } else {
        std::path::PathBuf::from(port)
    };

    // CDC-ACM has no arbitration: two readers split the byte stream and both desynchronise.
    // The vendor daemon runs as root, so an unprivileged /proc scan cannot see it holding the
    // port -- check the unit as well, which works for anyone.
    if foreign::vendor_units()
        .iter()
        .any(|u| u.unit == "argonupsrtcd.service" && u.is_active())
    {
        eprintln!(
            "argonctl: argonupsrtcd is running and holds the UPS serial port.\n\
             Two readers on a CDC-ACM port corrupt each other's frames. Stop it first:\n\n    \
             sudo systemctl stop argonupsrtcd\n\n\
             and start it again when you are done."
        );
        return Err(ExitCode::FAILURE);
    }
    if let Some(o) = foreign::port_owners(&path).first() {
        eprintln!(
            "argonctl: {} is held by pid {} ({}). Two readers corrupt each other's frames.",
            path.display(),
            o.pid,
            o.comm
        );
        return Err(ExitCode::FAILURE);
    }
    // That scan only sees processes we are allowed to see. As an ordinary user it cannot see
    // argond's file descriptors at all, so "nobody holds it" is not a finding -- say so,
    // rather than letting silence read as an all-clear.
    if !foreign::can_see_all_processes() {
        eprintln!(
            "argonctl: note: cannot check for other readers as an unprivileged user; run under \
             sudo for that check to mean anything."
        );
    }

    // The packaged daemon owns this node: the udev rule gives it to group `argon`, which is
    // also how two readers are kept apart -- file permissions do that reliably, where the
    // process scan above cannot.
    if permission_denied(&path) && is_argond_running() {
        eprintln!(
            "argonctl: {} belongs to the argond service (group `argon`), which is holding it \
             now.\n\n\
             argond publishes what it reads, so for a quick look:\n\n    \
             argonctl ups\n\n\
             To talk to the device from here instead, stop the service first:\n\n    \
             sudo systemctl stop argond\n\n\
             and start it again when you are done.",
            path.display()
        );
        return Err(ExitCode::FAILURE);
    }

    Ok(path)
}

fn run_serial(port: &str, args: &Args) -> ExitCode {
    let path = match serial_path(port) {
        Ok(p) => p,
        Err(code) => return code,
    };

    let config = match load_config(args.config.as_deref()) {
        Ok(c) => c,
        Err(code) => return code,
    };
    let policy = match BatteryPolicy::new(config.ups.policy()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argonctl: battery policy: {e}");
            return ExitCode::FAILURE;
        }
    };

    let link = match serial::SerialLink::open(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("argonctl: cannot open {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let mut monitor = UpsMonitor::new(Ups::new(QueryOnly(link)), policy);

    let id = identity(monitor.ups_mut());
    if !args.json {
        println!("Serial: {}", path.display());
        print_identity(&id);
    }

    if !args.watch {
        let poll = monitor.poll(uptime());
        if args.json {
            println!(
                "{}",
                json_from_serial(&path.display().to_string(), &id, &poll, &clock_hms())
            );
        } else {
            match poll.battery {
                Some(b) => println!("  {:<14} {b}", "battery"),
                None => println!(
                    "  {:<14} unavailable ({})",
                    "battery",
                    poll.error.map_or_else(String::new, |e| e.to_string())
                ),
            }
        }
        return ExitCode::SUCCESS;
    }

    let port = path.display().to_string();
    watch(&mut monitor, &config, args.count, args.json, &port, &id);
    let discarded = monitor.ups_mut().link().0.discarded_frames();
    if discarded > 0 {
        println!(
            "\nNote: {discarded} unsolicited or rejected frame(s) were discarded while waiting \
             for replies."
        );
    }
    ExitCode::SUCCESS
}

/// Firmware, clock and wake schedule: read once, not every poll.
/// Whether opening this path fails purely for lack of permission.
fn permission_denied(path: &std::path::Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .open(path)
        .err()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}

/// Whether the packaged daemon is running, and therefore owns the UPS port.
fn is_argond_running() -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", "argond.service"])
        .status()
        .is_ok_and(|s| s.success())
}

/// What the UPS says about itself: read once, not every poll.
///
/// Kept as values rather than printed on the spot, so the same read serves the table and the
/// JSON and the two cannot drift apart.
struct Identity {
    firmware: Result<String, String>,
    /// An implausible clock is carried as such: it is a reading, not an error.
    clock: Result<(String, bool), String>,
    /// `Ok(None)`: no schedule set.
    wake: Result<Option<String>, String>,
}

fn identity<L: argon_device::ups::UpsLink>(ups: &mut Ups<L>) -> Identity {
    Identity {
        firmware: ups
            .firmware()
            .map(|v| v.to_string())
            .map_err(|e| e.to_string()),
        clock: ups
            .clock()
            .map(|t| (t.to_string(), t.is_plausible()))
            .map_err(|e| e.to_string()),
        wake: ups
            .wake()
            .map(|w| w.map(|t| t.to_string()))
            .map_err(|e| e.to_string()),
    }
}

fn print_identity(id: &Identity) {
    match &id.firmware {
        Ok(v) => println!("  {:<14} {v}", "firmware"),
        Err(e) => println!("  {:<14} unavailable ({e})", "firmware"),
    }
    match &id.clock {
        Ok((t, true)) => println!("  {:<14} {t}", "clock"),
        Ok((t, false)) => println!("  {:<14} {t}  (implausible)", "clock"),
        Err(e) => println!("  {:<14} unavailable ({e})", "clock"),
    }
    match &id.wake {
        Ok(None) => println!("  {:<14} none set", "wake schedule"),
        Ok(Some(t)) => println!("  {:<14} {t}", "wake schedule"),
        Err(e) => println!("  {:<14} unavailable ({e})", "wake schedule"),
    }
}

/// One poll on the serial route as JSON.
///
/// The same object for a single read and for each line of `--watch`, so a consumer parses one
/// shape either way. A failed read gives `percent: null` and an `error`, never a stale number.
fn json_from_serial(
    port: &str,
    id: &Identity,
    poll: &argon_device::ups::Poll,
    time: &str,
) -> String {
    let source = poll.battery.map(|b| match b.source {
        PowerSource::Mains => "mains",
        PowerSource::Battery => "battery",
    });
    let advice = match poll.decision.advice {
        Advice::None => "none",
        Advice::Shutdown => "shutdown",
        Advice::HeldForUptime { .. } => "held-for-uptime",
    };
    serde_json::json!({
        "route": "serial",
        "port": port,
        "time": time,
        "firmware": id.firmware.as_deref().ok(),
        "clock": id.clock.as_ref().ok().map(|(t, _)| t.as_str()),
        "clock_plausible": id.clock.as_ref().ok().map(|&(_, ok)| ok),
        "wake_at": id.wake.as_ref().ok().and_then(|w| w.as_deref()),
        "percent": poll.battery.map(|b| b.percent),
        "source": source,
        "level": argon_device::status::level_name(poll.decision.level),
        "advice": advice,
        "error": poll.error.as_ref().map(std::string::ToString::to_string),
        "consecutive_failures": poll.consecutive_failures,
    })
    .to_string()
}

/// Polls until interrupted or `count` polls have run.
fn watch<L: argon_device::ups::UpsLink>(
    monitor: &mut UpsMonitor<L>,
    config: &Config,
    count: u64,
    json: bool,
    port: &str,
    id: &Identity,
) {
    let interval = Duration::from_secs(config.ups.poll_interval_s.max(1));
    if json {
        watch_json(monitor, interval, count, port, id);
        return;
    }
    let u = &config.ups;
    println!(
        "\nPolicy: low {}%, critical {}% (confirmed {}x), margin {}%, no advice before {}s uptime",
        u.low_percent, u.critical_percent, u.confirmations, u.recover_margin, u.min_uptime_s
    );
    println!(
        "Watching every {}s. Monitoring only: nothing is shut down. Ctrl-C to stop.\n",
        interval.as_secs()
    );
    println!(
        "  {:<8}  {:>4}  {:<11}  {:<16}  advice",
        "time", "pct", "source", "level"
    );

    let mut polls = 0u64;
    loop {
        let poll = monitor.poll(uptime());
        let time = clock_hms();
        let (pct, source) = poll.battery.map_or_else(
            || ("-".to_owned(), "-".to_owned()),
            |b| {
                let src = match b.source {
                    PowerSource::Mains => "mains",
                    PowerSource::Battery => "battery",
                };
                (format!("{}%", b.percent), src.to_owned())
            },
        );
        let advice = match poll.decision.advice {
            Advice::None => String::new(),
            Advice::Shutdown => "WOULD SHUT DOWN (not enabled)".to_owned(),
            Advice::HeldForUptime { remaining } => {
                format!("critical, held {}s after boot", remaining.as_secs())
            }
        };
        let change = poll
            .decision
            .changed_from
            .map_or_else(String::new, |from| format!("   <- was {from}"));
        println!(
            "  {time:<8}  {pct:>4}  {source:<11}  {:<16}  {advice}{change}",
            poll.decision.level.to_string()
        );
        if let Some(e) = &poll.error {
            println!(
                "            read failed ({} in a row): {e}",
                poll.consecutive_failures
            );
        }

        polls += 1;
        if count > 0 && polls >= count {
            break;
        }
        std::thread::sleep(interval);
    }
}

/// `--watch --json`: one object per poll, flushed as it goes, so a pipe sees each line when it
/// happens rather than when the process ends.
fn watch_json<L: argon_device::ups::UpsLink>(
    monitor: &mut UpsMonitor<L>,
    interval: Duration,
    count: u64,
    port: &str,
    id: &Identity,
) {
    use std::io::Write as _;
    let mut polls = 0u64;
    loop {
        let poll = monitor.poll(uptime());
        println!("{}", json_from_serial(port, id, &poll, &clock_hms()));
        let _ = std::io::stdout().flush();
        polls += 1;
        if count > 0 && polls >= count {
            break;
        }
        std::thread::sleep(interval);
    }
}

fn load_config(path: Option<&std::path::Path>) -> Result<Config, ExitCode> {
    let path = path.map_or_else(
        || std::path::PathBuf::from(argon_device::config::DEFAULT_PATH),
        std::path::Path::to_path_buf,
    );
    if !path.exists() {
        return Ok(Config::default());
    }
    Config::load(&path).map_err(|e| {
        eprintln!("argonctl: {}: {e}", path.display());
        ExitCode::FAILURE
    })
}

fn uptime() -> Duration {
    // If uptime cannot be read, report zero: that holds any shutdown advice rather than
    // releasing it early, which is the safe direction to be wrong in.
    platform::uptime().unwrap_or(Duration::ZERO)
}

/// Local wall-clock time as HH:MM:SS, without pulling in a date library for one column.
fn clock_hms() -> String {
    std::process::Command::new("date")
        .arg("+%H:%M:%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "?".to_owned(), |s| s.trim().to_owned())
}

/// Finds the UPS, opens its hidraw node and parses its report descriptor.
///
/// On failure, prints the reason and returns the exit code to propagate.
fn open_ups() -> Result<(hidraw::HidRaw, ReportDescriptor, usize, Option<String>), ExitCode> {
    let usb = discovery::usb_devices();
    let Some(ups) = discovery::find_argon_ups(&usb) else {
        eprintln!("argonctl: no Argon UPS found");
        return Err(ExitCode::FAILURE);
    };

    let Some(node) = ups.nodes.iter().find(|n| {
        n.file_name()
            .is_some_and(|f| f.to_string_lossy().starts_with("hidraw"))
    }) else {
        eprintln!("argonctl: the UPS exposes no hidraw node");
        return Err(ExitCode::FAILURE);
    };

    let dev = hidraw::HidRaw::open(node).map_err(|e| {
        eprintln!("argonctl: cannot open {}: {e}", node.display());
        eprintln!(
            "\nThe UPS's hidraw node belongs to the `argon` group -- the user argond runs as --\n\
             so that one process holds a link that has no arbitration, and because that group\n\
             carries writes: the UPS clock, the wake schedule, the battery meter, and HID's\n\
             ShutdownImminent. Adding your login to it is not the answer.\n\n\
             Either let argond read it and ask argond instead:\n\n    \
             sudo systemctl start argond && argonctl ups\n\n\
             or read the device yourself, just this once:\n\n    \
             sudo argonctl ups --device\n\n\
             Without the packaged udev rule the node is root:root 0600 and even the group is\n\
             absent; it is in packaging/udev/60-argon-utils.rules."
        );
        ExitCode::FAILURE
    })?;

    let raw = dev.descriptor().map_err(|e| {
        eprintln!("argonctl: cannot read the report descriptor: {e}");
        ExitCode::FAILURE
    })?;
    let desc = ReportDescriptor::parse(&raw).map_err(|e| {
        eprintln!("argonctl: cannot parse the report descriptor: {e}");
        ExitCode::FAILURE
    })?;

    Ok((dev, desc, raw.len(), ups.serial.clone()))
}

/// Decodes recognised telemetry fields out of whatever reports arrived.
///
/// Field positions come from the descriptor, so a firmware that moves or drops a report
/// yields a missing reading rather than a number assembled from the wrong bits.
fn decode(
    desc: &ReportDescriptor,
    reports: &[hidraw::Report],
) -> (Vec<Reading>, Vec<&'static str>) {
    let mut readings: Vec<Reading> = Vec::new();
    let mut status: Vec<&'static str> = Vec::new();

    for r in reports {
        let fields = desc
            .fields()
            .iter()
            .filter(|f| f.kind == ItemKind::Input && f.report_id == r.id);
        for f in fields {
            let Some(v) = f.extract(&r.payload) else {
                continue;
            };
            match (f.usage_page, f.usage) {
                (usage::PAGE_BATTERY_SYSTEM, usage::RELATIVE_STATE_OF_CHARGE) => {
                    readings.push(Reading {
                        label: "charge",
                        value: format!("{v}%"),
                    });
                }
                (usage::PAGE_BATTERY_SYSTEM, usage::RUN_TIME_TO_EMPTY) => {
                    readings.push(Reading {
                        label: "runtime left",
                        value: format_seconds(v),
                    });
                }
                (usage::PAGE_BATTERY_SYSTEM, usage::FULL_CHARGE_CAPACITY) => {
                    readings.push(Reading {
                        label: "full charge",
                        value: format!("{v}"),
                    });
                }
                (usage::PAGE_POWER_DEVICE, usage::CONFIG_VOLTAGE) => {
                    readings.push(Reading {
                        label: "voltage",
                        value: format!("{v}"),
                    });
                }
                // Flags: report only what is set. A list of every false bit is noise.
                (usage::PAGE_BATTERY_SYSTEM, u) if v != 0 => {
                    if let Some(label) = battery_flag(u) {
                        status.push(label);
                    }
                }
                (usage::PAGE_POWER_DEVICE, usage::SHUTDOWN_IMMINENT) if v != 0 => {
                    status.push("SHUTDOWN IMMINENT");
                }
                (usage::PAGE_POWER_DEVICE, usage::SHUTDOWN_REQUESTED) if v != 0 => {
                    status.push("shutdown requested");
                }
                _ => {}
            }
        }
    }
    (readings, status)
}

/// Human label for a Battery System status flag, if it is one we report.
const fn battery_flag(usage_id: u16) -> Option<&'static str> {
    Some(match usage_id {
        usage::CHARGING => "charging",
        usage::DISCHARGING => "discharging",
        usage::AC_PRESENT => "on mains",
        usage::BATTERY_PRESENT => "battery present",
        usage::FULLY_CHARGED => "fully charged",
        usage::FULLY_DISCHARGED => "fully discharged",
        usage::NEED_REPLACEMENT => "NEEDS REPLACEMENT",
        _ => return None,
    })
}

fn fmt_ids(reports: &[hidraw::Report]) -> String {
    let mut s: Vec<String> = reports.iter().map(|r| format!("{:#04x}", r.id)).collect();
    s.dedup();
    s.join(" ")
}

/// Formats a duration in seconds as something a human reads at a glance.
fn format_seconds(v: i64) -> String {
    if v < 0 {
        return format!("{v} (negative — unexpected)");
    }
    let (h, m, s) = (v / 3600, (v % 3600) / 60, v % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::{age, json_from_daemon, poweroff, render};
    use std::collections::HashMap;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_reading_is_reported_with_its_source_and_age() {
        let out = render(
            &fields(&[
                ("available", "yes"),
                ("level", "on-battery"),
                ("source", "battery"),
                ("percent", "64"),
                ("age_s", "3"),
                ("shutdown_at_unix", ""),
            ]),
            1_000,
        );
        assert!(out.contains("64%"), "{out}");
        assert!(out.contains("battery"), "{out}");
        assert!(out.contains("3s ago"), "{out}");
        assert!(out.contains("none scheduled"), "{out}");
    }

    #[test]
    fn a_daemon_with_nothing_to_report_says_so_rather_than_printing_blanks() {
        let out = render(&fields(&[("available", "no")]), 1_000);
        assert!(out.contains("no reading"), "{out}");
        assert!(!out.contains("charge"), "{out}");
    }

    #[test]
    fn monitoring_switched_off_reads_as_a_choice_not_a_fault() {
        // On a machine with no UPS, [ups] source = "none" is the correct configuration. The
        // first version of this line reported it as an absence, and it read like a broken
        // daemon -- which is exactly how it was read on a Pi 4.
        let out = render(
            &fields(&[("available", "no"), ("source_config", "none")]),
            1_000,
        );
        assert!(out.contains("off by configuration"), "{out}");
        assert!(out.contains("Nothing is wrong"), "{out}");
    }

    #[test]
    fn a_configured_source_with_no_reading_names_the_source_and_where_to_look() {
        let out = render(
            &fields(&[("available", "no"), ("source_config", "serial")]),
            1_000,
        );
        assert!(out.contains("serial"), "{out}");
        assert!(out.contains("journalctl"), "{out}");
    }

    #[test]
    fn a_missing_field_never_becomes_a_wrong_number() {
        // An older daemon, or one that failed its last read: the percentage is simply absent.
        let out = render(
            &fields(&[("available", "yes"), ("level", "unknown")]),
            1_000,
        );
        assert!(out.contains("unknown"), "{out}");
        assert!(!out.contains("0%"), "{out}");
    }

    #[test]
    fn a_stale_reading_is_called_stale() {
        // A thread that died leaves a status that otherwise looks perfectly healthy.
        assert!(age("3").contains("3s ago"));
        assert!(!age("59").contains("stale"));
        assert!(age("3600").contains("stale"), "{}", age("3600"));
        assert_eq!(age("not a number"), "-");
    }

    #[test]
    fn the_json_route_reports_numbers_as_numbers_and_absence_as_null() {
        let out = json_from_daemon(
            &fields(&[
                ("available", "yes"),
                ("level", "critical"),
                ("source", "battery"),
                ("percent", "7"),
                ("updated_unix", "1000"),
                ("age_s", "4"),
                ("shutdown_at_unix", "1300"),
            ]),
            1_000,
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["percent"], 7);
        assert_eq!(v["level"], "critical");
        assert_eq!(v["source"], "battery");
        assert_eq!(v["age_s"], 4);
        assert_eq!(v["stale"], false);
        assert_eq!(v["shutdown_in_s"], 300);
        assert_eq!(v["route"], "argond");
    }

    #[test]
    fn json_never_turns_a_missing_reading_into_a_zero() {
        // A monitor that cannot tell "no reading" from "0 %" would shut a machine down.
        let out = json_from_daemon(
            &fields(&[("available", "yes"), ("level", "unknown"), ("percent", "")]),
            1_000,
        );
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(v["percent"].is_null(), "{out}");
        assert!(v["shutdown_at_unix"].is_null(), "{out}");
        assert!(v["shutdown_in_s"].is_null(), "{out}");

        let none = json_from_daemon(&fields(&[("available", "no")]), 1_000);
        let v: serde_json::Value = serde_json::from_str(&none).expect("valid JSON");
        assert_eq!(v["available"], false);
        assert!(v["level"].is_null());
    }

    #[test]
    fn json_marks_a_stale_reading_at_the_same_threshold_as_the_text() {
        let stale = |age: &str| {
            let out = json_from_daemon(&fields(&[("available", "yes"), ("age_s", age)]), 0);
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            v["stale"].as_bool().unwrap()
        };
        assert!(!stale("119"));
        assert!(stale("120"));
        // The text output must agree, or a script and a person reading the same daemon
        // disagree about whether to trust it.
        assert!(!age("119").contains("stale"));
        assert!(age("120").contains("stale"));
    }

    #[test]
    fn a_pending_poweroff_is_shown_as_time_left() {
        assert_eq!(poweroff("1300", 1_000), "scheduled in 5m 0s");
        assert_eq!(poweroff("", 1_000), "none scheduled");
        // Already due: no underflow, and no negative time printed.
        assert_eq!(poweroff("900", 1_000), "scheduled in 0m 0s");
    }
}
