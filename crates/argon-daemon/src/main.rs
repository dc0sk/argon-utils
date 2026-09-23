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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};

mod control;
mod cpu_cap;
mod dbus;
mod exporter;
mod gauge;
mod oled;
mod startup;
mod ups;

#[derive(Parser)]
#[command(
    name = "argond",
    version,
    about = "UPS, OLED and fan daemon for Argon40 cases",
    long_about = "UPS, OLED and fan daemon for Argon40 cases.\n\n\
        Monitors the Argon PWR UPS over its USB serial link -- or, with [ups] source = \
        \"oneup\", the Argon ONE UP laptop's battery through its fuel gauge -- and, in mode \
        \"full\", schedules a delayed poweroff when the battery is confirmed critical, \
        cancelled if mains returns. \
        Keeps the UPS clock set from the system clock. Publishes UPS status to \
        /run/argon-utils/ups.state for the tray icon and the notification agent. Optionally \
        draws a status page on the case OLED. Drives the case fan on models with an Argon MCU \
        and reports the kernel-controlled fan on models without one.\n\n\
        Configuration: /etc/argon-utils/config.toml. Nothing that changes device state happens \
        in mode \"read-only\", the default.\n\n\
        Kill switch: if /etc/argon-utils/disabled exists, the systemd unit does not start."
)]
struct Cli {
    /// Configuration file.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Run a single control iteration and exit. For checking a configuration.
    #[arg(long)]
    once: bool,

    /// Print this program's manual page (roff) and exit. Used by the package build.
    #[arg(long, hide = true)]
    man: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.man {
        return print_man(
            &clap_mangen::Man::new(<Cli as clap::CommandFactory>::command()).section("8"),
        );
    }
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
        // An upgrade keeps the administrator's file, so a config written before a feature
        // existed silently runs that feature's defaults. Say which sections are missing.
        if let Ok(text) = std::fs::read_to_string(&path) {
            let missing = Config::missing_sections(&text);
            if !missing.is_empty() {
                eprintln!(
                    "argond: {} has no [{}] section; those settings are at their defaults. The \
                     package's current config is /etc/argon-utils/config.toml.dpkg-dist.",
                    path.display(),
                    missing.join("], [")
                );
            }
        }
        Config::load(&path)?
    } else {
        eprintln!("argond: {} not present, using defaults", path.display());
        Config::default()
    };

    // Find out what drives the fan before deciding anything about it. A bus that cannot be
    // resolved is not fatal: it means no MCU, and the UPS does not need one.
    let bus_path = match resolve_bus(&config) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("argond: {e}");
            None
        }
    };
    let fan = describe_fan_plan(bus_path.as_deref());
    let decision = announce_mode(&config, fan);

    let stopping = Arc::new(AtomicBool::new(false));
    // Created here rather than in the control loop: the D-Bus service starts first and needs
    // to read the same state, so `argonctl fan` can report what the daemon has the fan doing.
    let metrics = Arc::new(Mutex::new(exporter::State::default()));
    let watch = start_workers(cli, &config, &stopping, Arc::clone(&metrics));

    let (curve, source) = match prepare_fan(&config) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("argond: fan control unavailable: {e}");
            eprintln!("argond: continuing without it; UPS monitoring is unaffected");
            return ups_only_loop(cli, &stopping, watch);
        }
    };

    if let (true, startup::FanPlan::Mcu, Some(bus_path)) =
        (decision.mode.allows_writes(), fan, bus_path)
    {
        eprintln!("argond: bus {bus_path}");
        match LinuxI2c::open(&bus_path, u16::from(argon_device::mcu::ADDR)) {
            Ok(dev) => {
                let bus = RateLimited::new(dev, config.min_write_interval());
                control_loop(
                    cli,
                    &config,
                    (curve, source),
                    bus,
                    decision.mode,
                    true,
                    &stopping,
                    watch,
                    &metrics,
                )
            }
            Err(e) => {
                eprintln!("argond: cannot open {bus_path}: {e}");
                eprintln!("argond: continuing without fan control");
                ups_only_loop(cli, &stopping, watch)
            }
        }
    } else {
        // A null-write transport: the fan is observed, never driven. Constructed here rather
        // than branched on later, so no code path below can write by accident.
        let bus = DryRun::new(ReadOnly(NullBus));
        control_loop(
            cli,
            &config,
            (curve, source),
            bus,
            decision.mode,
            false,
            &stopping,
            watch,
            &metrics,
        )
    }
}

/// Starts the UPS, control and OLED threads.
///
/// UPS monitoring starts before anything to do with the fan. Fan setup can fail for reasons the
/// UPS does not care about -- a renamed thermal zone, one transient sensor read error, an
/// unopenable bus -- and each of those used to be a `?` that exited the process, taking battery
/// protection with it and leaving Restart=always to loop on it.
fn start_workers(
    cli: &Cli,
    config: &Config,
    stopping: &Arc<AtomicBool>,
    metrics: Arc<Mutex<exporter::State>>,
) -> Workers {
    let heartbeat = Arc::new(AtomicU64::new(unix_now()));
    // Requests from `argonctl` reach the UPS thread over this channel: that thread owns the UPS
    // port, so it is the only one that can act on them.
    let (to_ups, from_control) = std::sync::mpsc::channel();
    let request_waiting = Arc::new(AtomicBool::new(false));
    let requests = ups::Requests {
        rx: from_control,
        waiting: Arc::clone(&request_waiting),
    };
    // The case display's switch: the OLED thread obeys it, the D-Bus service flips it.
    let oled_control = Arc::new(oled::OledControl::from_state_directory());
    // The last reading, shared the same way: the monitoring thread fills it, the D-Bus service
    // reports it to callers who cannot open the device themselves.
    let latest = ups::no_status();
    // Only the PWR UPS has a clock to wake the machine by.
    let oneup = config.ups.source == "oneup";
    let ups_thread = if cli.once {
        None
    } else if oneup {
        gauge::spawn(
            config,
            &startup::active_vendor_units(),
            Arc::clone(stopping),
            Arc::clone(&heartbeat),
            requests,
            Arc::clone(&latest),
        )
    } else {
        ups::spawn(
            config,
            &startup::active_vendor_units(),
            Arc::clone(stopping),
            Arc::clone(&heartbeat),
            requests,
            Arc::clone(&latest),
        )
    };
    // The same channel serves the D-Bus service, so both routes reach the UPS thread the same
    // way. Its connection is kept in Workers: dropping it takes the service off the bus.
    let bus = if cli.once {
        None
    } else {
        dbus::serve(
            to_ups.clone(),
            Arc::clone(&request_waiting),
            !oneup,
            Arc::clone(&oled_control),
            latest,
            metrics,
            config.ups.source.clone(),
        )
    };
    let control_thread = if cli.once {
        None
    } else {
        control::spawn(
            &control::socket_path(),
            to_ups,
            request_waiting,
            Arc::clone(stopping),
        )
    };
    // The OLED thread starts here too, for the same reason: nothing about the fan should be
    // able to keep the display from showing the battery.
    let oled_thread = if cli.once {
        None
    } else {
        oled::spawn(
            config,
            &startup::active_vendor_units(),
            Arc::clone(stopping),
            Arc::clone(&oled_control),
        )
    };
    let others = [oled_thread, control_thread]
        .into_iter()
        .flatten()
        .collect();
    let mut workers = Workers::new(ups_thread, heartbeat, others, config);
    workers.bus = bus;
    workers
}

/// Works out what drives the fan, and says so.
fn describe_fan_plan(bus_path: Option<&str>) -> startup::FanPlan {
    let fan = startup::fan_plan(
        bus_path.is_some_and(mcu_answers),
        argon_hal::fan_hwmon::PwmFan::find().is_some(),
    );
    eprintln!(
        "argond: fan: {}",
        match fan {
            startup::FanPlan::Mcu => "Argon MCU at 0x1a",
            startup::FanPlan::KernelReportOnly =>
                "no Argon MCU; the kernel's pwm-fan drives it, reported only",
            startup::FanPlan::NoFan => "no Argon MCU and no kernel fan; nothing to drive",
        }
    );
    fan
}

/// Decides the mode, and reports it plus any refusal.
fn announce_mode(config: &Config, fan: startup::FanPlan) -> startup::Startup {
    let decision = startup::decide(config, &startup::active_vendor_units(), fan);
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
    decision
}

/// Builds the fan curve and opens the temperature sensor.
///
/// Every failure here is reported as a string rather than propagated: none of them is a
/// reason to stop monitoring the battery, which is what returning `?` from the caller used
/// to do.
fn prepare_fan(config: &Config) -> Result<(argon_proto::fan::FanCurve, ThermalZone), String> {
    let curve = config.fan_curve().map_err(|e| format!("fan curve: {e}"))?;
    let mut source = ThermalZone::find_cpu().map_err(|e| format!("sensor: {e}"))?;
    let first = source
        .read_decicelsius()
        .map_err(|e| format!("sensor read: {e}"))?;
    eprintln!("argond: sensor {}", source.describe());
    eprintln!("argond: {}C at startup", first / 10);
    Ok((curve, source))
}

/// Seconds since the unix epoch, or 0 if the clock is before it.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The background threads, and the UPS thread's liveness.
///
/// The fan loop feeds systemd's watchdog, so a UPS thread that panicked or wedged inside a
/// device read would leave the service `active` and apparently healthy with no battery
/// protection at all. This makes that state loud and fatal instead.
///
/// The other threads are held here so that stopping joins them: the OLED thread blanks the
/// panel on its way out and the control thread removes its socket, and the process must not
/// exit before either has happened.
struct Workers {
    thread: Option<JoinHandle<()>>,
    heartbeat: Arc<AtomicU64>,
    stale_after: Duration,
    /// The OLED and control threads: joined on the way out, so the panel is blanked and the
    /// control socket removed before the process exits.
    others: Vec<JoinHandle<()>>,
    /// The D-Bus service's connection, held so the service stays on the bus.
    bus: Option<zbus::blocking::Connection>,
}

impl Workers {
    fn new(
        thread: Option<JoinHandle<()>>,
        heartbeat: Arc<AtomicU64>,
        others: Vec<JoinHandle<()>>,
        config: &Config,
    ) -> Self {
        // Several poll intervals, and never less than a minute: a reopen after a device
        // re-enumeration legitimately takes a while.
        let interval = config.ups.poll_interval_s.max(1) * u64::from(ups::STALL_INTERVALS);
        Self {
            thread,
            heartbeat,
            stale_after: Duration::from_secs(interval.max(60)),
            others,
            bus: None,
        }
    }

    /// How long since the UPS thread last started a poll, if it has gone quiet.
    fn stalled_for(&self) -> Option<Duration> {
        self.thread.as_ref()?;
        let since =
            Duration::from_secs(unix_now().saturating_sub(self.heartbeat.load(Ordering::Relaxed)));
        (since > self.stale_after).then_some(since)
    }

    fn join(self) {
        // Off the bus first, so nothing can ask for an action while the rest shuts down.
        drop(self.bus);
        for t in self.thread.into_iter().chain(self.others) {
            let _ = t.join();
        }
    }
}

/// Runs until stopped, with no fan control: UPS monitoring only.
fn ups_only_loop(
    cli: &Cli,
    stopping: &Arc<AtomicBool>,
    watch: Workers,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    install_signal_handlers(stopping, || ())?;
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

    let mut code = ExitCode::SUCCESS;
    while !stopping.load(Ordering::Relaxed) && !cli.once {
        let _ = sd_notify::notify(&[sd_notify::NotifyState::Watchdog]);
        if let Some(since) = watch.stalled_for() {
            report_stalled_ups(since);
            code = ExitCode::FAILURE;
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    stopping.store(true, Ordering::Relaxed);
    watch.join();
    eprintln!("argond: stopping");
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    Ok(code)
}

/// Says a stalled UPS thread out loud, in the terms an operator needs.
fn report_stalled_ups(since: Duration) {
    eprintln!(
        "argond: ups: the monitoring thread has not polled for {}s. Exiting so systemd \
         restarts us: a daemon that looks healthy while nothing watches the battery is worse \
         than one that is visibly down.",
        since.as_secs()
    );
}

/// Whether an Argon MCU acknowledges its address.
///
/// `SMBus` quick-write only: no data byte, so nothing for either firmware dialect to act on
/// (ADR-0002). Without this the first fan write went to an address nothing answers on a
/// ONE V5, and its `EREMOTEIO` stopped the daemon.
fn mcu_answers(bus_path: &str) -> bool {
    let addr = argon_device::mcu::ADDR;
    LinuxI2c::open(bus_path, u16::from(addr)).is_ok_and(|mut bus| bus.probe(addr).unwrap_or(false))
}

/// Writes a manual page to stdout.
fn print_man(man: &clap_mangen::Man) -> ExitCode {
    match man.render(&mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("argond: {e}");
            ExitCode::FAILURE
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

#[expect(
    clippy::too_many_arguments,
    reason = "the daemon's one control loop; a parameter struct would only move the list"
)]
fn control_loop<B: I2cBus + Send + 'static, T: TemperatureSource>(
    cli: &Cli,
    config: &Config,
    fan_stack: (argon_proto::fan::FanCurve, T),
    bus: B,
    mode: Mode,
    drive_fan: bool,
    stopping: &Arc<AtomicBool>,
    watch: Workers,
    metrics: &Arc<Mutex<exporter::State>>,
) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let (curve, source) = fan_stack;
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

    {
        // Restores a running duty on the usual termination signals, before the loop unwinds.
        let guard = Arc::clone(&guard);
        install_signal_handlers(stopping, move || {
            let _ = guard.restore_now();
        })?;
    }

    if drive_fan {
        // Assert a known duty before anything else. If a previous instance died leaving the
        // fan stopped, this is the first thing that fixes it. Not fatal: exiting would stop
        // UPS monitoring too, and the loop below keeps retrying the fan anyway.
        if let Err(e) = mcu
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_fan(safe_duty)
        {
            eprintln!("argond: initial fan write failed: {e}");
        }
    }

    // Shared state the exporter reads. Updated by the control loop; never written by the
    // exporter, so a scrape cannot perturb what it measures.
    if config.telemetry.enabled {
        match exporter::spawn(&config.telemetry.listen, mode, Arc::clone(metrics)) {
            Ok(addr) => eprintln!("argond: metrics on http://{addr}/metrics"),
            Err(e) => eprintln!("argond: metrics disabled: {e}"),
        }
    }

    let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
    let interval = config.fan_poll_interval();
    let mut code = ExitCode::SUCCESS;

    while !stopping.load(Ordering::Relaxed) {
        match task.step() {
            Ok(Step::Applied {
                decicelsius,
                duty,
                wrote,
            }) => {
                if wrote && drive_fan {
                    eprintln!("argond: {}C -> {duty}", decicelsius / 10);
                }
                exporter::record(
                    metrics,
                    Some(decicelsius),
                    0,
                    Some(duty.percent()),
                    drive_fan,
                );
            }
            Ok(Step::SensorFailed {
                fallback,
                consecutive,
            }) => {
                eprintln!(
                    "argond: temperature read failed ({consecutive} in a row), forcing {fallback}"
                );
                exporter::record(
                    metrics,
                    None,
                    consecutive,
                    Some(fallback.percent()),
                    drive_fan,
                );
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

        // Checked after the ping, not instead of it: the fan loop is doing its job, and this
        // is about the other thread. Exiting hands the restart to systemd.
        if let Some(since) = watch.stalled_for() {
            report_stalled_ups(since);
            code = ExitCode::FAILURE;
            break;
        }

        if cli.once {
            break;
        }
        std::thread::sleep(interval);
    }

    stopping.store(true, Ordering::Relaxed);
    watch.join();
    if drive_fan {
        eprintln!("argond: stopping, restoring {safe_duty}");
    } else {
        eprintln!("argond: stopping");
    }
    let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
    Ok(code)
}

/// Sets the stop flag on the usual termination signals, after running `on_signal`.
///
/// `on_signal` is where the fan's safe duty is restored. It runs before the main loop
/// unwinds: the Drop guard would also fire, but only once the loop notices the flag, and on a
/// hot machine that delay is the whole problem it exists to avoid. The UPS-only path passes a
/// no-op, because there is no fan to restore.
fn install_signal_handlers(
    stopping: &Arc<AtomicBool>,
    on_signal: impl Fn() + Send + 'static,
) -> Result<(), Box<dyn std::error::Error>> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])?;
    let stopping = Arc::clone(stopping);

    std::thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            eprintln!("argond: signal {sig}, stopping");
            on_signal();
            stopping.store(true, Ordering::Relaxed);
        }
    });
    Ok(())
}
