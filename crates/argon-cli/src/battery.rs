// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl battery` — the Argon ONE UP's battery, from its CW2217 fuel gauge.
//!
//! Read-only: only the documented read-only registers, through a bus type that has no write
//! method. Safe alongside the vendor's daemon and argond, since the I2C bus arbitrates
//! between readers.

use argon_device::gauge::Cw2217;
use argon_hal::discovery;
use argon_hal::i2c::LinuxI2c;
use argon_proto::cw2217::{self, Flow};
use argon_proto::ups::PowerSource;
use std::process::ExitCode;
use std::time::Duration;

#[derive(clap::Args)]
pub struct Args {
    /// I2C bus device path, or `auto` for the header bus.
    #[arg(long, default_value = "auto", value_name = "PATH")]
    pub bus: String,

    /// Read this many times, `--interval` apart. 1 reads once.
    #[arg(long, default_value = "1")]
    pub count: u32,

    /// Seconds between reads.
    #[arg(long, default_value = "2")]
    pub interval: u64,

    /// Machine-readable output: one JSON object per read.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &Args) -> ExitCode {
    let bus = if args.bus == "auto" {
        let Some(p) = discovery::header_i2c_bus() else {
            eprintln!("argonctl: no I2C bus found; is dtparam=i2c_arm=on set in config.txt?");
            return ExitCode::FAILURE;
        };
        p.display().to_string()
    } else {
        args.bus.clone()
    };
    let mut gauge = match LinuxI2c::open(&bus, u16::from(cw2217::ADDR)).and_then(Cw2217::identify) {
        Ok(g) => g,
        Err(e) => {
            eprintln!(
                "argonctl: no CW2217 fuel gauge at 0x{:02x} on {bus}: {e}\n\
                 This command is for the Argon ONE UP; the PWR UPS is `argonctl ups --serial auto`.",
                cw2217::ADDR
            );
            return ExitCode::FAILURE;
        }
    };

    let health = gauge.health();
    if !args.json {
        println!("CW2217 fuel gauge at {}", gauge.describe());
        match &health {
            Ok(h) => println!("  {} charge cycles, state of health {} %", h.cycles, h.soh),
            Err(e) => println!("  cycles and health unreadable: {e}"),
        }
        println!();
        println!("  charge   voltage    current  flow          policy sees");
    }

    let count = args.count.max(1);
    for n in 0..count {
        if n > 0 {
            std::thread::sleep(Duration::from_secs(args.interval));
        }
        if args.json {
            use std::io::Write as _;
            println!(
                "{}",
                json_reading(
                    &gauge.describe(),
                    health.as_ref().ok(),
                    gauge.read_now().as_ref()
                )
            );
            let _ = std::io::stdout().flush();
            continue;
        }
        match gauge.read_now() {
            Ok(r) => println!(
                "  {:>5} %  {:.4} V  {:>+7}  {:<12}  {}",
                r.percent,
                f64::from(r.vcell_uv) / 1e6,
                r.current,
                flow_name(r.flow),
                match r.battery().source {
                    PowerSource::Battery => "on battery",
                    PowerSource::Mains => "on mains",
                }
            ),
            Err(e) => println!("  read failed: {e}"),
        }
    }
    if !args.json {
        println!(
            "\nCurrent is the raw register: positive charging, negative discharging. It is not\n\
             in amperes, because the ONE UP's sense resistor is not known."
        );
    }
    ExitCode::SUCCESS
}

/// One gauge reading as JSON.
///
/// `current_raw` is named for what it is: the register value, positive charging, negative
/// discharging, in no unit, because the ONE UP's sense resistor is unknown (`ONEUP-RSENSE`).
/// A failed read gives `null` readings and an `error`, never the previous numbers again.
fn json_reading(
    bus: &str,
    health: Option<&argon_device::gauge::Health>,
    reading: Result<&argon_device::gauge::Reading, &argon_hal::Error>,
) -> String {
    let r = reading.ok();
    serde_json::json!({
        "route": "cw2217",
        "bus": bus,
        "cycles": health.map(|h| h.cycles),
        "state_of_health_percent": health.map(|h| h.soh),
        "percent": r.map(|r| r.percent),
        "vcell_uv": r.map(|r| r.vcell_uv),
        "current_raw": r.map(|r| r.current),
        "flow": r.map(|r| flow_name(r.flow)),
        "source": r.map(|r| match r.battery().source {
            PowerSource::Battery => "battery",
            PowerSource::Mains => "mains",
        }),
        "error": reading.err().map(std::string::ToString::to_string),
    })
    .to_string()
}

const fn flow_name(flow: Flow) -> &'static str {
    match flow {
        Flow::Charging => "charging",
        Flow::Discharging => "discharging",
        Flow::Idle => "idle",
    }
}

#[cfg(test)]
mod tests {
    use super::json_reading;
    use argon_device::gauge::{Health, Reading};
    use argon_proto::cw2217::Flow;

    fn reading(percent: u8, current: i16, flow: Flow) -> Reading {
        Reading {
            percent,
            vcell_uv: 3_912_000,
            current,
            flow,
        }
    }

    #[test]
    fn a_reading_carries_the_raw_current_and_the_source_the_policy_sees() {
        let r = reading(64, -2256, Flow::Discharging);
        let out = json_reading("i2c-1 @ 0x64", Some(&Health { cycles: 7, soh: 98 }), Ok(&r));
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["percent"], 64);
        assert_eq!(
            v["current_raw"], -2256,
            "the sign is the direction of charge"
        );
        assert_eq!(v["flow"], "discharging");
        assert_eq!(v["source"], "battery");
        assert_eq!(v["cycles"], 7);
        assert_eq!(v["state_of_health_percent"], 98);
    }

    #[test]
    fn a_charger_too_weak_for_the_load_still_reads_as_battery() {
        // The ONE UP finding: "on mains" is not a pin, it is which way charge is moving.
        let r = reading(100, -50, Flow::Discharging);
        let out = json_reading("bus", None, Ok(&r));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["source"], "battery");
        assert_eq!(v["percent"], 100);
        assert!(v["cycles"].is_null(), "invented health");
    }

    #[test]
    fn a_failed_read_reports_the_error_and_no_numbers() {
        let e = argon_hal::Error::Timeout;
        let out = json_reading("bus", None, Err(&e));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["percent"].is_null(), "{out}");
        assert!(v["flow"].is_null());
        assert!(v["error"].is_string());
    }
}
