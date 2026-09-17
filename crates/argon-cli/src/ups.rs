// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl ups` — UPS telemetry over hidraw.
//!
//! Read-only. Uses `hidraw` and Input reports only, so it claims no USB interface and
//! cannot disturb whatever holds the serial port.

use argon_hal::{discovery, foreign, hidraw, serial};
use argon_proto::hid::{ItemKind, ReportDescriptor, usage};
use argon_proto::ups::{BatteryStatus, Command, UpsTime};
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
        return run_serial(port);
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

/// Reads telemetry over the Argon serial protocol.
///
/// Read-only commands only: battery status, firmware version, clock and wake schedule. No
/// writes, and in particular no meter reset -- that discards the battery meter's baseline.
fn run_serial(port: &str) -> ExitCode {
    let usb = discovery::usb_devices();
    let path = if port == "auto" {
        let Some(ups) = discovery::find_argon_ups(&usb) else {
            eprintln!("argonctl: no Argon UPS found");
            return ExitCode::FAILURE;
        };
        let node = ups.nodes.iter().find(|n| {
            n.file_name()
                .is_some_and(|f| f.to_string_lossy().starts_with("ttyACM"))
        });
        let Some(node) = node else {
            eprintln!("argonctl: the UPS exposes no serial node");
            return ExitCode::FAILURE;
        };
        node.clone()
    } else {
        std::path::PathBuf::from(port)
    };

    // CDC-ACM has no arbitration: two readers split the stream and both desynchronise. Check
    // before opening, and say plainly when the check itself is inconclusive.
    let owners = foreign::port_owners(&path);
    if let Some(o) = owners.first() {
        eprintln!(
            "argonctl: {} is held by pid {} ({}).\n\n\
             Two readers on a CDC-ACM port corrupt each other's frames. Stop the owning\n\
             service first -- for the vendor stack that is:\n\n    \
             sudo systemctl stop argonupsrtcd\n",
            path.display(),
            o.pid,
            o.comm
        );
        return ExitCode::FAILURE;
    }
    if !foreign::can_see_all_processes() {
        eprintln!(
            "argonctl: note -- not privileged, so the ownership check only saw our own\n\
             processes. If the vendor daemon is running, this will read corrupt frames.\n"
        );
    }

    let mut link = match serial::SerialLink::open(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("argonctl: cannot open {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
    };

    println!("Serial: {}", path.display());
    println!("--------{}", "-".repeat(path.display().to_string().len()));

    let mut ask = |cmd: Command, label: &str| {
        let deadline = Instant::now() + Duration::from_secs(3);
        match link.request(cmd.as_byte(), &[], deadline) {
            Ok(f) => Some(f),
            Err(e) => {
                println!("  {label:<16} unavailable ({e})");
                None
            }
        }
    };

    if let Some(f) = ask(Command::BatteryStatus, "battery") {
        match BatteryStatus::decode(f.payload()) {
            Ok(s) => println!("  {:<16} {s}", "battery"),
            Err(e) => println!("  {:<16} undecodable ({e})", "battery"),
        }
    }
    if let Some(f) = ask(Command::FirmwareVersion, "firmware") {
        match f.payload() {
            [v] => println!("  {:<16} {v}", "firmware"),
            other => println!("  {:<16} unexpected payload {other:?}", "firmware"),
        }
    }
    if let Some(f) = ask(Command::GetRtc, "clock") {
        match UpsTime::decode_clock(f.payload()) {
            Ok(t) if t.is_plausible() => println!("  {:<16} {t}", "clock"),
            Ok(t) => println!("  {:<16} {t}  (implausible)", "clock"),
            Err(e) => println!("  {:<16} undecodable ({e})", "clock"),
        }
    }
    if let Some(f) = ask(Command::GetWake, "wake schedule") {
        // With nothing scheduled the device answers with an empty payload, not five zero
        // bytes -- so "none set" is a successful decode rather than a length error.
        match UpsTime::decode_optional_schedule(f.payload()) {
            Ok(None) => println!("  {:<16} none set", "wake schedule"),
            Ok(Some(t)) if t.is_plausible() => println!("  {:<16} {t}", "wake schedule"),
            Ok(Some(t)) => println!("  {:<16} {t}  (implausible)", "wake schedule"),
            Err(e) => println!("  {:<16} undecodable ({e})", "wake schedule"),
        }
    }

    println!(
        "\nFraming and these commands are `observed` in docs/protocol/FACTS.md, captured from\n\
         hardware on 2026-09-17. See crates/argon-proto/tests/tapes/ups-reads.json."
    );
    ExitCode::SUCCESS
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
