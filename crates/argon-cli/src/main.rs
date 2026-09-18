// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl` — command-line tool for Argon40 Raspberry Pi enclosures and UPS.

use clap::{Parser, Subcommand};

mod button;
mod doctor;
mod fan;
mod notify;
mod oled;
mod rtc;
mod ups;

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
    command: Command,
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
    /// Uses hidraw and Input reports only, so it claims no USB interface and cannot
    /// disturb whatever holds the serial port.
    #[command(subcommand_negates_reqs = true)]
    Ups(ups::Args),

    /// Show what the fan controller would do. Always a dry run.
    Fan(fan::Args),

    /// Bring up the OLED display. Previews only, unless `--write` or `--off` is given.
    Oled(oled::Args),

    /// Show desktop notifications for UPS events. Runs inside a desktop session.
    NotifyAgent(notify::Args),

    /// Read the UPS clock. `--t15 --write` runs the experiment that confirms how to set it.
    ///
    /// Without those flags this only queries. The write exists solely as task T15 in
    /// docs/testing/HUMAN-TASKS.md: setting the clock is an `inferred` fact, and an inferred
    /// fact may not back a write path until it has been observed.
    Rtc(rtc::Args),

    /// Measure the case button's pulse widths.
    ///
    /// The MCU decodes the press and emits a calibrated pulse whose width encodes the
    /// action. This times those pulses with kernel nanosecond timestamps.
    Button(button::Args),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor(args) => doctor::run(&args),
        Command::Ups(args) => ups::run(&args),
        Command::Button(args) => button::run(&args),
        Command::Fan(args) => fan::run(&args),
        Command::Oled(args) => oled::run(&args),
        Command::NotifyAgent(args) => notify::run(&args),
        Command::Rtc(args) => rtc::run(&args),
    }
}
