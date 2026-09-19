// SPDX-License-Identifier: GPL-3.0-or-later
//! `argon-tray` — a panel icon for the UPS, the CPU temperature and the fan.
//!
//! Reads what `argond` publishes in `/run/argon-utils/ups.state`; it never opens the UPS
//! serial port. That port has exactly one owner, the daemon, because CDC-ACM has no
//! arbitration and two readers desynchronise each other.
//!
//! The icon is a `StatusNotifierItem`, which is what the Raspberry Pi desktop's panel
//! (wf-panel-pi) hosts natively, and what KDE, waybar and others host too.

use argon_device::status::{self, UpsStatus};
use argon_hal::fan_hwmon::PwmFan;
use argon_hal::thermal::{TemperatureSource, ThermalZone};
use clap::Parser;
use ksni::blocking::TrayMethods;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

mod tray;
mod view;
mod wake;

#[derive(Parser)]
#[command(
    name = "argon-tray",
    version,
    about = "Panel icon for argon-utils",
    long_about = "Panel icon for argon-utils: the UPS charge and state, the CPU temperature and \
        the fan, as a StatusNotifierItem. Started at login from /etc/xdg/autostart.\n\n\
        Reads the status argond publishes in /run/argon-utils/ups.state and never opens the UPS \
        itself. A status argond has stopped updating is shown as stale, never as current. It shows \
        any shutdown logind has scheduled and offers to cancel it -- behind a two-step menu for \
        argond's low-battery poweroff, since cancelling that one risks the battery running out. \
        Where polkit allows, it offers to power off with a UPS wake."
)]
struct Cli {
    /// Status file argond publishes.
    #[arg(long, value_name = "PATH", default_value = status::DEFAULT_PATH)]
    state: PathBuf,

    /// Seconds between reads.
    #[arg(long, default_value = "2")]
    interval: u64,

    /// Print what the icon would show, once, and exit. Needs no desktop.
    #[arg(long)]
    once: bool,

    /// Print this program's manual page (roff) and exit. Used by the package build.
    #[arg(long, hide = true)]
    man: bool,
}

/// The things the tray reads, found once at startup.
struct Sources {
    state: PathBuf,
    sensor: Option<ThermalZone>,
    fan: Option<PwmFan>,
}

impl Sources {
    fn read(&mut self) -> view::Snapshot {
        view::Snapshot {
            ups: read_status(&self.state),
            now: SystemTime::now(),
            cpu_decicelsius: self.sensor.as_mut().and_then(|s| s.read_decicelsius().ok()),
            fan: self.fan.as_ref().and_then(PwmFan::read),
        }
    }
}

fn read_status(path: &Path) -> Option<UpsStatus> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| UpsStatus::parse(&t).ok())
}

fn main() -> ExitCode {
    argon_hal::platform::exit_quietly_on_broken_stdout();
    let cli = Cli::parse();
    if cli.man {
        let man = clap_mangen::Man::new(<Cli as clap::CommandFactory>::command()).section("1");
        return match man.render(&mut std::io::stdout()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("argon-tray: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let mut sources = Sources {
        state: cli.state,
        sensor: ThermalZone::find_cpu().ok(),
        fan: PwmFan::find(),
    };
    let render = |s: &view::Snapshot| view::render(s, &status::local_hhmm);

    let first = render(&sources.read());
    if cli.once {
        println!("icon      {}", first.icon);
        println!("urgency   {:?}", first.urgency);
        println!("headline  {}", first.headline);
        for d in &first.details {
            println!("          {d}");
        }
        if first.shutdown_pending {
            println!("menu      offers: Cancel the scheduled poweroff");
        }
        let scheduled = zbus::blocking::Connection::system()
            .ok()
            .and_then(|c| wake::logind_scheduled(&c));
        match (
            &scheduled,
            wake::pending(scheduled.as_ref().map(|(k, u)| (k.as_str(), *u)), None),
        ) {
            (None, _) => println!("logind    could not read ScheduledShutdown"),
            (Some(_), None) => println!("logind    no shutdown scheduled"),
            (Some(_), Some(p)) => println!(
                "logind    {} at {}",
                p.kind,
                status::local_hhmm(SystemTime::UNIX_EPOCH + Duration::from_secs(p.at_unix))
            ),
        }
        return ExitCode::SUCCESS;
    }

    let tray = tray::ArgonTray::new(first.clone());
    let ours = std::sync::Arc::clone(&tray.ours);
    // Autostart can run before the panel is up: on the ONE UP the tray was started, found no
    // StatusNotifierWatcher, and exited for good. Treat a missing panel as "not yet" -- ksni
    // then registers the icon when the panel appears (see ArgonTray::watcher_offline).
    let handle = match tray.assume_sni_available(true).spawn() {
        Ok(h) => h,
        Err(e) => {
            // No StatusNotifierWatcher on the session bus: a panel without tray support, or
            // no desktop session at all. Say which is likely rather than just the error.
            eprintln!(
                "argon-tray: cannot show a tray icon: {e}\n\
                 This needs a panel that hosts StatusNotifierItem icons (the Raspberry Pi \
                 desktop's panel does). For a one-off reading without a desktop, use --once."
            );
            return ExitCode::FAILURE;
        }
    };

    let interval = Duration::from_secs(cli.interval.max(1));
    let mut shown = first;
    // Kept open: logind is asked on every poll, so a shutdown scheduled or cancelled anywhere
    // shows within one interval.
    let system_bus = zbus::blocking::Connection::system().ok();
    let mut shown_pending: Option<wake::Pending> = None;
    let mut next_wake_refresh = std::time::Instant::now();
    while !handle.is_closed() {
        // Once a minute, off the menu's path: whether "power off and wake" can be offered, and
        // the times to offer. Opening the menu never waits on the bus or on `date`.
        if std::time::Instant::now() >= next_wake_refresh {
            let can = wake::can();
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let presets = wake::presets(now, wake::next_seven(now));
            handle.update(|t| {
                t.can_wake = can;
                t.presets = presets;
            });
            next_wake_refresh = std::time::Instant::now() + Duration::from_secs(60);
        }
        std::thread::sleep(interval);
        if let Some(conn) = &system_bus {
            let scheduled = wake::logind_scheduled(conn);
            let ours = ours.lock().ok().and_then(|o| *o);
            let now_pending =
                wake::pending(scheduled.as_ref().map(|(k, u)| (k.as_str(), *u)), ours);
            if now_pending != shown_pending {
                handle.update(|t| t.pending.clone_from(&now_pending));
                shown_pending = now_pending;
            }
        }
        let next = render(&sources.read());
        // Only push a change. Every update is a D-Bus signal the panel acts on, and most
        // polls change nothing.
        if next != shown {
            handle.update(|t| t.view = next.clone());
            shown = next;
        }
    }
    ExitCode::SUCCESS
}
