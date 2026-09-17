// SPDX-License-Identifier: GPL-3.0-or-later
//! `argond` — the argon-utils daemon.
//!
//! # What this does when things go wrong
//!
//! The worst outcome is a stopped fan on a heating machine, so the shutdown path gets more
//! care than the happy path. Three layers, none sufficient alone:
//!
//! 1. A signal handler restores a safe duty on `SIGTERM`/`SIGINT`/`SIGHUP`, before the
//!    process unwinds.
//! 2. A `Drop` guard covers a panic or any other unwind. This works only because the release
//!    profile uses `panic = "unwind"` — under `abort` a `Drop` impl does not run, and
//!    `scripts/check-panic-strategy.sh` keeps it that way.
//! 3. `ExecStopPost=` in the systemd unit covers `SIGKILL`, where no in-process mechanism
//!    can help because the process has stopped executing.

use argon_device::config::Config;
use argon_device::fan::{FanTask, Step};
use argon_device::mcu::{Dialect, Mcu};
use argon_device::safety::FanSafeGuard;
use argon_hal::i2c::{DryRun, I2cBus, LinuxI2c, RateLimited, ReadOnly};
use argon_hal::mode::Mode;
use argon_hal::thermal::{TemperatureSource, ThermalZone};
use argon_proto::fan::{FanController, FanDuty};
use clap::Parser;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

mod exporter;
mod startup;
mod ups;

#[derive(Parser)]
#[command(
    name = "argond",
    version,
    about = "Fan, button and UPS daemon for Argon40 cases"
)]
struct Cli {
    /// Configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Run a single control iteration and exit. For checking a configuration.
    #[arg(long)]
    once: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("argond: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let path = cli
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(argon_device::config::DEFAULT_PATH));
    let config = if path.exists() {
        Config::load(&path)?
    } else {
        eprintln!("argond: {} not present, using defaults", path.display());
        Config::default()
    };

    let decision = startup::decide(&config, &startup::active_vendor_units());
    if let Some(refusal) = &decision.refusal {
        // WARN, not an error: this is a supported state, and the daemon keeps running.
        eprintln!("argond: {refusal}\n");
        eprintln!("argond: continuing in read-only mode.");
    }
    eprintln!(
        "argond: mode {} (configured {}){}",
        decision.mode,
        decision.requested,
        if decision.degraded() {
            ", degraded"
        } else {
            ""
        }
    );

    let curve = config.fan_curve()?;
    let mut source = ThermalZone::find_cpu()?;
    eprintln!("argond: sensor {}", source.describe());
    let first = source.read_decicelsius()?;
    eprintln!("argond: {}C at startup", first / 10);

    match decision.mode {
        Mode::ReadOnly => {
            // A null-write transport: the fan is observed, never driven. Constructed here
            // rather than branched on later, so no code path below can write by accident.
            let bus = DryRun::new(ReadOnly(NullBus));
            control_loop(cli, &config, curve, source, bus, decision.mode)
        }
        Mode::Managed | Mode::Full => {
            let bus_path = resolve_bus(&config)?;
            eprintln!("argond: bus {bus_path}");
            let bus = RateLimited::new(
                LinuxI2c::open(&bus_path, u16::from(argon_device::mcu::ADDR))?,
                config.min_write_interval(),
            );
            control_loop(cli, &config, curve, source, bus, decision.mode)
        }
    }
}

/// Resolves the I2C bus path from configuration, by adapter name rather than by number.
fn resolve_bus(config: &Config) -> Result<String, Box<dyn std::error::Error>> {
    if config.mcu.bus != "auto" {
        return Ok(config.mcu.bus.clone());
    }
    argon_hal::discovery::header_i2c_bus()
        .map(|p| p.display().to_string())
        .ok_or_else(|| "no I2C bus found; is dtparam=i2c_arm=on set in config.txt?".into())
}

/// A bus that accepts writes and discards them, for read-only mode.
struct NullBus;

impl I2cBus for NullBus {
    fn write(&mut self, _addr: u8, _data: &[u8]) -> argon_hal::Result<()> {
        Ok(())
    }
    fn probe(&mut self, _addr: u8) -> argon_hal::Result<bool> {
        Ok(false)
    }
    fn describe(&self) -> String {
        "none (read-only)".into()
    }
}

fn control_loop<B: I2cBus + Send + 'static, T: TemperatureSource>(
    cli: &Cli,
    config: &Config,
    curve: argon_proto::fan::FanCurve,
    source: T,
    bus: B,
    mode: Mode,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mcu = Arc::new(Mutex::new(Mcu::new(bus, Dialect::default())));
    let controller = FanController::new(
        curve,
        config.fan.hysteresis_c,
        config.fan.min_duty,
        config.fan.allow_stop,
    );
    let safe_duty = FanDuty::clamped(config.fan.safe_duty);
    let mut task = FanTask::new(Arc::clone(&mcu), source, controller, safe_duty);

    // Armed for the whole loop. Its Drop restores a running duty on any unwind.
    let guard = Arc::new(FanSafeGuard::new(Arc::clone(&mcu), safe_duty));

    let stopping = Arc::new(AtomicBool::new(false));
    install_signal_handlers(&stopping, &guard)?;

    if mode.allows_writes() {
        // Assert a known duty before anything else. If a previous instance died leaving the
        // fan stopped, this is the first thing that fixes it.
        mcu.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_fan(safe_duty)?;
    }

    // Shared state the exporter reads. Updated by the control loop; never written by the
    // exporter, so a scrape cannot perturb what it measures.
    let metrics = Arc::new(Mutex::new(exporter::State::default()));
    if config.telemetry.enabled {
        match exporter::spawn(&config.telemetry.listen, mode, Arc::clone(&metrics)) {
            Ok(addr) => eprintln!("argond: metrics on http://{addr}/metrics"),
            Err(e) => eprintln!("argond: metrics disabled: {e}"),
        }
    }

    // UPS monitoring runs on its own thread with its own interval, and decides its own
    // contention: it only needs the vendor's UPS daemons out of the way, not the fan daemon.
    let ups_thread = if cli.once {
        None
    } else {
        ups::spawn(
            config,
            &startup::active_vendor_units(),
            Arc::clone(&stopping),
        )
    };

    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
    let interval = config.fan_poll_interval();

    while !stopping.load(Ordering::Relaxed) {
        match task.step() {
            Ok(Step::Applied {
                decicelsius,
                duty,
                wrote,
            }) => {
                if wrote {
                    eprintln!("argond: {}C -> {duty}", decicelsius / 10);
                }
                exporter::record(&metrics, Some(decicelsius), 0);
            }
            Ok(Step::SensorFailed {
                fallback,
                consecutive,
            }) => {
                eprintln!(
                    "argond: temperature read failed ({consecutive} in a row), forcing {fallback}"
                );
                exporter::record(&metrics, None, consecutive);
            }
            Err(e) => {
                // A failed write means we are no longer in control. Report and keep trying:
                // exiting here would drop the guard and hand the fan to nobody.
                eprintln!("argond: write failed: {e}");
            }
        }

        // Ping the watchdog from the control loop itself, not a separate timer thread. A
        // watchdog fed by a thread that is not doing the work will happily keep a wedged
        // control loop alive.
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);

        if cli.once {
            break;
        }
        std::thread::sleep(interval);
    }

    stopping.store(true, Ordering::Relaxed);
    if let Some(t) = ups_thread {
        let _ = t.join();
    }
    eprintln!("argond: stopping, restoring {safe_duty}");
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    Ok(ExitCode::SUCCESS)
}

/// Restores a safe duty on the usual termination signals.
fn install_signal_handlers<B: I2cBus + Send + 'static>(
    stopping: &Arc<AtomicBool>,
    guard: &Arc<FanSafeGuard<B>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    let stopping = Arc::clone(stopping);
    let guard = Arc::clone(guard);

    std::thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            eprintln!("argond: signal {sig}, restoring a safe fan duty");
            // Act before the main loop unwinds. The Drop guard would also fire, but only
            // once the loop notices the flag, and on a hot machine that delay is the whole
            // problem this exists to avoid.
            let _ = guard.restore_now();
            stopping.store(true, Ordering::Relaxed);
        }
    });
    Ok(())
}
