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
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags: clap derives one field per flag"
)]
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

    /// Machine-readable output: one JSON object, or one per iteration with `--watch`.
    #[arg(long, conflicts_with = "safe")]
    pub json: bool,

    /// Ask whether anything answers at the MCU address, and nothing else.
    ///
    /// Sends one `SMBus` quick-write: the address with the write bit and **no data byte**. That
    /// is the only transaction ADR-0002 permits against an unidentified device at 0x1a --
    /// a register read would put the register number on the bus first, which legacy firmware
    /// takes as a fan duty. Nothing is written, and no fan changes.
    #[arg(long, conflicts_with_all = ["safe", "watch", "json"])]
    pub probe: bool,
}

pub fn run(args: &Args) -> ExitCode {
    let (config, mode, curve) = match load(args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if args.safe {
        return set_safe(&config, mode);
    }
    if args.probe {
        return probe(&config);
    }

    if !args.json {
        report_curve(&config, mode, &curve);
    }

    let mut source = match ThermalZone::find_cpu() {
        Ok(z) => z,
        Err(e) => {
            eprintln!("\nargonctl: cannot find a CPU thermal zone: {e}");
            return ExitCode::FAILURE;
        }
    };
    if !args.json {
        println!("\nSensor: {}", source.describe());
    }

    let current = match source.read_decicelsius() {
        Ok(dc) => dc,
        Err(e) => {
            eprintln!("argonctl: cannot read the temperature: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.json {
        if args.watch {
            return watch(args, &config, curve, source);
        }
        println!(
            "{}",
            json_state(&config, mode, &curve, &source.describe(), current)
        );
        return ExitCode::SUCCESS;
    }
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

/// The fan's state and what the curve makes of it, as JSON.
///
/// `curve_duty_percent` is the curve's own answer, which is not necessarily what would be
/// written: the controller then applies the floor and the hysteresis, both reported here, so
/// a curve value of 0 below the first point still writes `floor_percent` unless stopping is
/// allowed. `--watch` reports the controller's actual decisions. Nothing here is a reading of
/// the fan: on a Pi 5 the kernel governor drives it and this subcommand is always a dry run
/// (T11).
fn json_state(
    config: &Config,
    mode: Mode,
    curve: &FanCurve,
    sensor: &str,
    decicelsius: i32,
) -> String {
    let points: Vec<_> = curve
        .points()
        .iter()
        .map(|p| serde_json::json!({ "celsius": p.celsius(), "duty_percent": p.duty.percent() }))
        .collect();
    serde_json::json!({
        "route": "fan-dry-run",
        "mode": mode.to_string(),
        "sensor": sensor,
        "temperature_c": f64::from(decicelsius) / 10.0,
        "curve_duty_percent": curve.duty_for(decicelsius).percent(),
        "curve": points,
        "hysteresis_c": config.fan.hysteresis_c,
        "floor_percent": config.fan.min_duty,
        "safe_percent": config.fan.safe_duty,
        "stopping_allowed": config.fan.allow_stop,
    })
    .to_string()
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
                if !args.json {
                    println!("Config: {}", path.display());
                }
                c
            }
            Err(e) => {
                eprintln!("argonctl: {}: {e}", path.display());
                return Err(ExitCode::FAILURE);
            }
        }
    } else {
        if !args.json {
            println!("Config: {} not present, using defaults", path.display());
        }
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

/// Asks whether anything answers at `0x1a`, with the one permitted transaction.
///
/// Through [`ReadOnly`], so the bus handed to the probe has no usable write: the guarantee is
/// structural, not a promise about this function's body.
fn probe(config: &Config) -> ExitCode {
    let bus_path = match resolve_bus(config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("argonctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    let addr = argon_device::mcu::ADDR;
    let bus = match LinuxI2c::open(&bus_path, u16::from(addr)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("argonctl: cannot open {bus_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut bus = ReadOnly(bus);

    println!("Probing {bus_path} at 0x{addr:02x}");
    println!("  sending: address + write bit, no data byte (SMBus quick-write), then stop");
    match bus.probe(addr) {
        Ok(true) => {
            println!("\n  ANSWERED. Something is at 0x{addr:02x} and acknowledged its address.");
            println!(
                "  That is all this says: not which firmware, not which dialect, and not that\n  \
                 it is a fan controller. Deciding the dialect is task T5, and there is no safe\n  \
                 probe for it -- see docs/design/adr/0002-legacy-mcu-dialect-by-default.md."
            );
            ExitCode::SUCCESS
        }
        Ok(false) => {
            println!("\n  NO ANSWER at 0x{addr:02x}: nothing acknowledged the address.");
            println!(
                "  On a Pi 5 in an Argon ONE V5 this is expected -- that case has no such\n  \
                 device, and the kernel drives the fan (T11)."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("\nargonctl: the probe itself failed: {e}");
            ExitCode::FAILURE
        }
    }
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

    // Probe before writing, exactly as argond does. This runs as the unit's ExecStopPost on
    // every stop, and on a ONE V5 there is no MCU at 0x1a at all: without this it puts an
    // unsolicited write on the header bus each time. ADR-0002 is explicit that a write to the
    // wrong firmware at that address is destructive, so "nothing answered" must mean "write
    // nothing", not "write anyway and log the error".
    match mcu.is_present() {
        Ok(true) => {}
        Ok(false) => {
            println!("no Argon MCU answers at 0x1a on {bus_path}; nothing to restore");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("argonctl: cannot probe {bus_path}: {e}");
            return ExitCode::FAILURE;
        }
    }

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
    if !args.json {
        println!(
            "\nWatching every {}s. Nothing is written to any device. Ctrl-C to stop.\n",
            interval.as_secs()
        );
        println!("  {:>8}  {:>6}  {:>6}  note", "temp", "duty", "write");
    }

    let mut iterations = 0usize;
    loop {
        match task.step() {
            Ok(step) if args.json => {
                use std::io::Write as _;
                println!("{}", json_step(step));
                let _ = std::io::stdout().flush();
            }
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
    if !args.json {
        println!("\nWould have written {} transaction(s):", writes.len());
        for (addr, data) in writes {
            let hex: Vec<String> = data.iter().map(|b| format!("{b:02x}")).collect();
            println!("  i2c 0x{addr:02x} <- {}", hex.join(" "));
        }
    }

    ExitCode::SUCCESS
}

/// One controller iteration as JSON.
///
/// A failed sensor read is its own shape: `temperature_c` is null and the duty is the
/// fallback, so nothing reads a fallback as a measurement.
fn json_step(step: Step) -> String {
    match step {
        Step::Applied {
            decicelsius,
            duty,
            wrote,
        } => serde_json::json!({
            "temperature_c": f64::from(decicelsius) / 10.0,
            "duty_percent": duty.percent(),
            "would_write": wrote,
            "sensor_ok": true,
        }),
        Step::SensorFailed {
            fallback,
            consecutive,
        } => serde_json::json!({
            "temperature_c": serde_json::Value::Null,
            "duty_percent": fallback.percent(),
            "would_write": true,
            "sensor_ok": false,
            "consecutive_failures": consecutive,
        }),
    }
    .to_string()
}

/// Formats tenths of a degree as `NN.N`.
fn fmt_c(decicelsius: i32) -> String {
    format!("{}.{}", decicelsius / 10, (decicelsius % 10).abs())
}

#[cfg(test)]
mod tests {
    use super::{json_state, json_step};
    use argon_device::config::Config;
    use argon_device::fan::Step;
    use argon_hal::mode::Mode;
    use argon_proto::fan::FanDuty;

    #[test]
    fn the_state_reports_the_curve_and_what_it_says_now() {
        let config = Config::default();
        let curve = config.fan_curve().expect("the default curve is valid");
        let out = json_state(&config, Mode::ReadOnly, &curve, "zone0", 620);
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(v["temperature_c"], 62.0);
        assert_eq!(v["sensor"], "zone0");
        assert!(v["curve"].as_array().is_some_and(|c| !c.is_empty()));
        // The floor is reported beside the curve, because the curve alone does not say what
        // would be written below its first point.
        assert!(v["floor_percent"].is_number());
        assert!(v["stopping_allowed"].is_boolean());
    }

    #[test]
    fn a_failed_sensor_read_is_not_reported_as_a_temperature() {
        let out = json_step(Step::SensorFailed {
            fallback: FanDuty::clamped(55),
            consecutive: 3,
        });
        let v: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        assert!(v["temperature_c"].is_null(), "a fallback read as a reading");
        assert_eq!(v["duty_percent"], 55);
        assert_eq!(v["sensor_ok"], false);
        assert_eq!(v["consecutive_failures"], 3);
    }

    #[test]
    fn an_applied_step_says_whether_it_would_have_written() {
        let out = json_step(Step::Applied {
            decicelsius: 475,
            duty: FanDuty::clamped(10),
            wrote: false,
        });
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["temperature_c"], 47.5);
        assert_eq!(v["would_write"], false);
        assert_eq!(v["sensor_ok"], true);
    }
}
