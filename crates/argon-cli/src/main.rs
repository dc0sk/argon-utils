// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl` — command-line tool for Argon40 Raspberry Pi enclosures and UPS.

use clap::{Parser, Subcommand};

mod doctor;

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
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Doctor(args) => doctor::run(&args),
    }
}
