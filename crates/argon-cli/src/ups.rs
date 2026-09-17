// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl ups` — UPS telemetry over hidraw.
//!
//! Read-only. Uses `hidraw` and Input reports only, so it claims no USB interface and
//! cannot disturb whatever holds the serial port.

use argon_device::config::Config;
use argon_device::ups::{QueryOnly, Ups, UpsMonitor};
use argon_hal::{discovery, foreign, hidraw, platform, serial};
use argon_proto::hid::{ItemKind, ReportDescriptor, usage};
use argon_proto::ups::PowerSource;
use argon_proto::ups::policy::{Advice, BatteryPolicy};
use std::process::ExitCode;
use std::time::{Duration, Instant};

#[derive(clap::Args)]
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

    /// Read over the Argon serial protocol instead of HID.
    ///
    /// Pass a port path, or `auto` to use the discovered UPS. The vendor's daemon holds the
    /// port continuously and CDC-ACM has no arbitration, so this refuses to run while
    /// another process owns it.
    #[arg(long, value_name = "PATH_OR_AUTO")]
    pub serial: Option<String>,
}

/// A decoded telemetry value.
struct Reading {
    label: &'static str,
    value: String,
}

pub fn run(args: &Args) -> ExitCode {
    if let Some(port) = &args.serial {
        return run_serial(port, args);
    }
    let (mut dev, desc, raw_len, serial) = match open_ups() {
        Ok(v) => v,
        Err(code) => return code,
    };

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

/// Reads the UPS over the Argon serial protocol, once or continuously.
///
/// Every request goes through `QueryOnly`, so nothing that changes the UPS's state can be
/// sent from here, whatever this function does.
fn run_serial(port: &str, args: &Args) -> ExitCode {
    let path = if port == "auto" {
        let Some(p) = discovery::argon_ups_serial_path() else {
            eprintln!("argonctl: no Argon UPS serial port found");
            return ExitCode::FAILURE;
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
        return ExitCode::FAILURE;
    }
    if let Some(o) = foreign::port_owners(&path).first() {
        eprintln!(
            "argonctl: {} is held by pid {} ({}). Two readers corrupt each other's frames.",
            path.display(),
            o.pid,
            o.comm
        );
        return ExitCode::FAILURE;
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
             cat /run/argon-utils/ups.state\n\n\
             To talk to the device from here instead, stop the service first:\n\n    \
             sudo systemctl stop argond\n\n\
             and start it again when you are done.",
            path.display()
        );
        return ExitCode::FAILURE;
    }

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

    println!("Serial: {}", path.display());
    report_identity(monitor.ups_mut());

    if !args.watch {
        let poll = monitor.poll(uptime());
        match poll.battery {
            Some(b) => println!("  {:<14} {b}", "battery"),
            None => println!(
                "  {:<14} unavailable ({})",
                "battery",
                poll.error.map_or_else(String::new, |e| e.to_string())
            ),
        }
        return ExitCode::SUCCESS;
    }

    watch(&mut monitor, &config, args.count);
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

fn report_identity<L: argon_device::ups::UpsLink>(ups: &mut Ups<L>) {
    match ups.firmware() {
        Ok(v) => println!("  {:<14} {v}", "firmware"),
        Err(e) => println!("  {:<14} unavailable ({e})", "firmware"),
    }
    match ups.clock() {
        Ok(t) if t.is_plausible() => println!("  {:<14} {t}", "clock"),
        Ok(t) => println!("  {:<14} {t}  (implausible)", "clock"),
        Err(e) => println!("  {:<14} unavailable ({e})", "clock"),
    }
    match ups.wake() {
        Ok(None) => println!("  {:<14} none set", "wake schedule"),
        Ok(Some(t)) => println!("  {:<14} {t}", "wake schedule"),
        Err(e) => println!("  {:<14} unavailable ({e})", "wake schedule"),
    }
}

/// Polls until interrupted or `count` polls have run.
fn watch<L: argon_device::ups::UpsLink>(monitor: &mut UpsMonitor<L>, config: &Config, count: u64) {
    let interval = Duration::from_secs(config.ups.poll_interval_s.max(1));
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
            "\nhidraw nodes are root:root 0600 by default. Either run this with sudo, or\n\
             install the udev rule from packaging/udev/60-argon-utils.rules."
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
