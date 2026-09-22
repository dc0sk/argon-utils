// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl` — command-line tool for Argon40 Raspberry Pi enclosures and UPS.

use clap::{Parser, Subcommand};

mod battery;
mod button;
mod doctor;
mod fan;
mod lid_agent;
mod notify;
mod oled;
mod poweroff;
mod rtc;
mod setup;
mod ups;
mod zigbee;

#[derive(Parser)]
#[command(
    name = "argonctl",
    version,
    about = "Control and inspect Argon40 Raspberry Pi enclosures and UPS",
    long_about = "Control and inspect Argon40 Raspberry Pi enclosures and UPS.\n\n\
                  This tool defaults to read-only operation. Commands that write to hardware \
                  are explicit and gated; `doctor` writes nothing at all and is safe to run \
                  alongside the vendor's own daemons."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Print this program's manual page (roff) and exit. Used by the package build.
    #[arg(long, hide = true)]
    man: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect the machine and report what Argon hardware is present.
    ///
    /// Strictly read-only: reads sysfs, queries systemd, and opens no serial port. Safe to
    /// run with the vendor daemons active.
    Doctor(doctor::Args),

    /// Read UPS telemetry.
    ///
    /// Asks argond over D-Bus, which is the process that holds the UPS port -- so this needs
    /// no access to the device and no membership of the `argon` group. Without argond it falls
    /// back to hidraw, Input reports only, which claims no USB interface. `--device` and
    /// `--serial` choose a route explicitly.
    #[command(subcommand_negates_reqs = true)]
    Ups(ups::Args),

    /// Read the Argon ONE UP's battery from its fuel gauge. Read-only.
    ///
    /// Reads only the CW2217's documented read-only registers; safe alongside the vendor's
    /// daemon and argond.
    Battery(battery::Args),

    /// Show what the fan controller would do. Always a dry run.
    Fan(fan::Args),

    /// Bring up the OLED display. Previews only, unless `--write` or `--off` is given.
    Oled(oled::Args),

    /// Show desktop notifications for UPS events. Runs inside a desktop session.
    NotifyAgent(notify::Args),

    /// Act on a laptop lid: power-save or shutdown, per the `[lid]` configuration.
    ///
    /// Runs inside a desktop session, started from /etc/xdg/autostart. Exits at once on a
    /// machine without a lid switch.
    LidAgent(lid_agent::Args),

    /// Read the UPS clock. `--t15 --write` runs the experiment that confirms how to set it.
    ///
    /// Without those flags this only queries. The write exists solely as task T15 in
    /// docs/testing/HUMAN-TASKS.md: setting the clock is an `inferred` fact, and an inferred
    /// fact may not back a write path until it has been observed.
    Rtc(rtc::Args),

    /// Power off now, and have the UPS power the machine on again at a set time.
    ///
    /// argond sets the wake, reads it back, and only then schedules the poweroff a minute out.
    /// Needs root (the control socket is argond's) and mode "full".
    Poweroff(poweroff::Args),

    /// Check that this machine is configured for the Argon hardware it has.
    ///
    /// Says what is missing and the exact command that fixes it. Read-only: it reads
    /// config.txt, asks dpkg and systemd, and probes I2C addresses only with the quick-write
    /// that carries no data byte. Boot configuration needs root and a reboot, so it prints
    /// those commands rather than running them.
    Setup(setup::Args),

    /// Identify the Zigbee module. `--probe` runs the read-only health probe (task T16).
    ///
    /// Without --probe nothing is opened. The probe listens first, then sends only `SYS_PING`
    /// and `SYS_VERSION`, and refuses if anything else holds the port.
    Zigbee(zigbee::Args),

    /// Measure the case button's pulse widths.
    ///
    /// The MCU decodes the press and emits a calibrated pulse whose width encodes the
    /// action. This times those pulses with kernel nanosecond timestamps.
    Button(button::Args),
}

fn main() -> std::process::ExitCode {
    use clap::CommandFactory;
    argon_hal::platform::exit_quietly_on_broken_stdout();
    let cli = Cli::parse();
    if cli.man {
        let man = clap_mangen::Man::new(Cli::command()).section("1");
        return match man.render(&mut std::io::stdout()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("argonctl: {e}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let Some(command) = cli.command else {
        // As before the subcommand became optional: no subcommand is a usage error.
        let _ = Cli::command().print_help();
        return std::process::ExitCode::from(2);
    };
    match command {
        Command::Doctor(args) => doctor::run(&args),
        Command::Setup(args) => setup::run(&args),
        Command::Ups(args) => ups::run(&args),
        Command::Button(args) => button::run(&args),
        Command::Battery(args) => battery::run(&args),
        Command::Fan(args) => fan::run(&args),
        Command::Oled(args) => oled::run(&args),
        Command::NotifyAgent(args) => notify::run(&args),
        Command::LidAgent(args) => lid_agent::run(&args),
        Command::Rtc(args) => rtc::run(&args),
        Command::Zigbee(args) => zigbee::run(&args),
        Command::Poweroff(args) => poweroff::run(&args),
    }
}
