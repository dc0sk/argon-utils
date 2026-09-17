// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl fan` — show what the fan controller would do.
//!
//! Dry run by default. It reads the real temperature and runs the real controller, but the
//! transport records writes instead of performing them, so it is safe to run alongside the
//! vendor's daemon and tells you exactly what would go on the bus.

use argon_device::config::Config;
use argon_device::fan::{FanTask, Step};
use argon_device::mcu::{Dialect, Mcu};
use argon_hal::Result;
use argon_hal::i2c::{DryRun, I2cBus, LinuxI2c, ReadOnly};
use argon_hal::mode::Mode;
use argon_hal::thermal::{TemperatureSource, ThermalZone};
use argon_proto::fan::{FanController, FanCurve, FanDuty};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};

/// A bus that goes nowhere, for showing the curve without touching hardware.
struct NullBus;

impl I2cBus for NullBus {
    fn write(&mut self, _addr: u8, _data: &[u8]) -> Result<()> {
        Ok(())
    }
    fn probe(&mut self, _addr: u8) -> Result<bool> {
        Ok(false)
    }
    fn describe(&self) -> String {
        "no device (dry run)".into()
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// Configuration file to read. Defaults are used if it does not exist.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Keep running, showing each control iteration.
    #[arg(long)]
    pub watch: bool,

    /// How many iterations to run with `--watch`. 0 means until interrupted.
    #[arg(long, default_value = "0")]
    pub count: usize,

    /// Write the configured safe duty to the fan and exit.
    ///
    /// This is the one path in this subcommand that writes. It exists for the systemd unit's
    /// `ExecStopPost=`, which runs after the daemon is gone -- including after a `SIGKILL`,
    /// where no in-process guard can help because the process stopped executing.
    #[arg(long, conflicts_with = "watch")]
    pub safe: bool,
}

pub fn run(args: &Args) -> ExitCode {
    let (config, mode, curve) = match load(args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if args.safe {
        return set_safe(&config, mode);
    }

    report_curve(&config, mode, &curve);

    let mut source = match ThermalZone::find_cpu() {
        Ok(z) => z,
        Err(e) => {
            eprintln!("\nargonctl: cannot find a CPU thermal zone: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("\nSensor: {}", source.describe());

    let current = match source.read_decicelsius() {
        Ok(dc) => dc,
        Err(e) => {
            eprintln!("argonctl: cannot read the temperature: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!(
        "Now:    {}C -> curve says {}",
        fmt_c(current),
        curve.duty_for(current)
    );

    if args.watch {
        watch(args, &config, curve, source)
    } else {
        println!("\n(dry run: nothing was written. Use --watch to follow the controller.)");
        ExitCode::SUCCESS
    }
}

/// Loads configuration and derives the mode and curve.
fn load(args: &Args) -> std::result::Result<(Config, Mode, FanCurve), ExitCode> {
    let path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(argon_device::config::DEFAULT_PATH));

    let config = if path.exists() {
        match Config::load(&path) {
            Ok(c) => {
                println!("Config: {}", path.display());
                c
            }
            Err(e) => {
                eprintln!("argonctl: {}: {e}", path.display());
                return Err(ExitCode::FAILURE);
            }
        }
    } else {
        println!("Config: {} not present, using defaults", path.display());
        Config::default()
    };

    let mode = config.mode().map_err(|e| {
        eprintln!("argonctl: {e}");
        ExitCode::FAILURE
    })?;
    let curve = config.fan_curve().map_err(|e| {
        eprintln!("argonctl: {e}");
        ExitCode::FAILURE
    })?;
    Ok((config, mode, curve))
}

/// Writes the configured safe duty to the fan.
///
/// Refuses in read-only mode. A safety fallback that ignores the mode gate would be a way to
/// write to the hardware from a configuration that says not to, and "but it is for safety"
/// is exactly the argument that erodes such gates.
fn set_safe(config: &Config, mode: Mode) -> ExitCode {
    let duty = FanDuty::clamped(config.fan.safe_duty);

    if !mode.allows_writes() {
        eprintln!(
            "argonctl: mode is {mode}, so nothing was written.\n\
             In read-only mode this daemon never drove the fan, so there is nothing to restore."
        );
        return ExitCode::SUCCESS;
    }

    let bus_path = match resolve_bus(config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argonctl: {e}");
            return ExitCode::FAILURE;
        }
    };

    let bus = match LinuxI2c::open(&bus_path, u16::from(argon_device::mcu::ADDR)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("argonctl: cannot open {bus_path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut mcu = Mcu::new(bus, Dialect::default());
    match mcu.set_fan(duty) {
        Ok(()) => {
            println!("fan set to {duty} on {bus_path}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("argonctl: could not set the fan to {duty}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolves the I2C bus path from configuration, by adapter name rather than by number.
fn resolve_bus(config: &Config) -> std::result::Result<String, String> {
    if config.mcu.bus != "auto" {
        return Ok(config.mcu.bus.clone());
    }
    argon_hal::discovery::header_i2c_bus()
        .map(|p| p.display().to_string())
        .ok_or_else(|| "no I2C bus found; is dtparam=i2c_arm=on set?".to_owned())
}

/// Prints the curve that would be applied.
fn report_curve(config: &Config, mode: Mode, curve: &FanCurve) {
    println!("Mode:   {mode}");
    println!("\nCurve");
    println!("-----");
    for p in curve.points() {
        println!("  >= {:>3}C   {}", p.celsius(), p.duty);
    }
    println!(
        "  hysteresis {}C, floor {}%, safe {}%, stopping {}",
        config.fan.hysteresis_c,
        config.fan.min_duty,
        config.fan.safe_duty,
        if config.fan.allow_stop {
            "allowed"
        } else {
            "not allowed"
        }
    );
}

/// Runs the controller, printing each iteration. Never writes to a device.
fn watch(args: &Args, config: &Config, curve: FanCurve, source: ThermalZone) -> ExitCode {
    // Always a dry run: this subcommand exists to show intent, and the transport records
    // writes rather than performing them, so it cannot contend with the vendor daemon.
    let bus = DryRun::new(ReadOnly(NullBus));
    let mcu = Arc::new(Mutex::new(Mcu::new(bus, Dialect::default())));
    let controller = FanController::new(
        curve,
        config.fan.hysteresis_c,
        config.fan.min_duty,
        config.fan.allow_stop,
    );
    let mut task = FanTask::new(
        Arc::clone(&mcu),
        source,
        controller,
        FanDuty::clamped(config.fan.safe_duty),
    );

    let interval = config.fan_poll_interval();
    println!(
        "\nWatching every {}s. Nothing is written to any device. Ctrl-C to stop.\n",
        interval.as_secs()
    );
    println!("  {:>8}  {:>6}  {:>6}  note", "temp", "duty", "write");

    let mut iterations = 0usize;
    loop {
        match task.step() {
            Ok(Step::Applied {
                decicelsius,
                duty,
                wrote,
            }) => {
                println!(
                    "  {:>7}C  {:>6}  {:>6}",
                    fmt_c(decicelsius),
                    duty.to_string(),
                    if wrote { "yes" } else { "-" }
                );
            }
            Ok(Step::SensorFailed {
                fallback,
                consecutive,
            }) => {
                println!(
                    "  {:>8}  {:>6}  {:>6}  sensor read failed ({consecutive} in a row)",
                    "-",
                    fallback.to_string(),
                    "yes"
                );
            }
            Err(e) => {
                eprintln!("argonctl: {e}");
                return ExitCode::FAILURE;
            }
        }

        iterations += 1;
        if args.count > 0 && iterations >= args.count {
            break;
        }
        std::thread::sleep(interval);
    }

    let mcu = mcu.lock().unwrap_or_else(PoisonError::into_inner);
    let writes = mcu.bus().writes();
    println!("\nWould have written {} transaction(s):", writes.len());
    for (addr, data) in writes {
        let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
        println!("  i2c 0x{addr:02x} <- {}", hex.join(" "));
    }

    ExitCode::SUCCESS
}

/// Formats tenths of a degree as `NN.N`.
fn fmt_c(decicelsius: i32) -> String {
    format!("{}.{}", decicelsius / 10, (decicelsius % 10).abs())
}
