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

    println!("CW2217 fuel gauge at {}", gauge.describe());
    match gauge.health() {
        Ok(h) => println!("  {} charge cycles, state of health {} %", h.cycles, h.soh),
        Err(e) => println!("  cycles and health unreadable: {e}"),
    }
    println!();
    println!("  charge   voltage    current  flow          policy sees");

    let count = args.count.max(1);
    for n in 0..count {
        if n > 0 {
            std::thread::sleep(Duration::from_secs(args.interval));
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
    println!(
        "\nCurrent is the raw register: positive charging, negative discharging. It is not in\n\
         amperes, because the ONE UP's sense resistor is not known."
    );
    ExitCode::SUCCESS
}

const fn flow_name(flow: Flow) -> &'static str {
    match flow {
        Flow::Charging => "charging",
        Flow::Discharging => "discharging",
        Flow::Idle => "idle",
    }
}
