// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl oled` — bring up the SSD1306 display.
//!
//! Previews in the terminal by default and touches nothing. `--write` sends the test pattern,
//! `--off` blanks and switches the panel off.

use argon_device::oled::{Oled, test_pattern};
use argon_hal::i2c::LinuxI2c;
use argon_hal::{discovery, foreign};
use argon_proto::oled::{self, FrameBuffer, HEIGHT, WIDTH};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(clap::Args)]
pub struct Args {
    /// Send the test pattern to the panel. Without this, nothing is sent.
    #[arg(long, conflicts_with = "off")]
    pub write: bool,

    /// Blank the panel and switch it off.
    ///
    /// Run this once you have your photo: a static image left on an OLED burns in.
    #[arg(long)]
    pub off: bool,

    /// The panel is mounted upside down: rotate the image 180 degrees.
    #[arg(long)]
    pub flip: bool,

    /// I2C bus device. Found by adapter name if not given.
    #[arg(long, value_name = "PATH")]
    pub bus: Option<PathBuf>,
}

pub fn run(args: &Args) -> ExitCode {
    if !args.write && !args.off {
        println!("Preview of the test pattern (nothing is sent to the panel):\n");
        print_preview(&test_pattern());
        println!("\nSend it with:  argonctl oled --write");
        return ExitCode::SUCCESS;
    }

    // The vendor's daemon holds /dev/i2c-1 and has OLED code. Its display thread is off on
    // this machine, but a refusal is cheaper than working out why the screen tears.
    let vendor: Vec<String> = foreign::vendor_units()
        .into_iter()
        .filter(|u| u.unit == "argononed.service" && u.is_active())
        .map(|u| u.unit)
        .collect();
    if !vendor.is_empty() {
        eprintln!(
            "argonctl: {} is running and also has code that drives this display.\n\
             Stop it for the test, and start it again afterwards:\n\n    \
             sudo systemctl stop argononed\n    \
             argonctl oled --write\n    \
             ...\n    \
             argonctl oled --off\n    \
             sudo systemctl start argononed\n\n\
             On an Argon ONE V5 with a Pi 5 stopping it costs nothing: its fan and button code has\n\
             no hardware to drive there.",
            vendor.join(", ")
        );
        return ExitCode::FAILURE;
    }

    let Some(bus_path) = args.bus.clone().or_else(discovery::header_i2c_bus) else {
        eprintln!("argonctl: no I2C bus found; is dtparam=i2c_arm=on set in config.txt?");
        return ExitCode::FAILURE;
    };
    let bus_str = bus_path.display().to_string();

    // Bound to the panel's address: this transport refuses a write to anything else.
    let bus = match LinuxI2c::open(&bus_str, u16::from(oled::ADDR)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("argonctl: cannot open {bus_str}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut panel = Oled::new(bus, args.flip);

    match panel.is_present() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "argonctl: nothing answers at 0x{:02x} on {bus_str}",
                oled::ADDR
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("argonctl: probing 0x{:02x} failed: {e}", oled::ADDR);
            return ExitCode::FAILURE;
        }
    }

    if args.off {
        return match panel.off() {
            Ok(()) => {
                println!("panel blanked and switched off");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("argonctl: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if let Err(e) = panel.init().and_then(|()| panel.flush(&test_pattern())) {
        eprintln!("argonctl: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "test pattern sent to 0x{:02x} on {bus_str}{}",
        oled::ADDR,
        if args.flip { " (rotated 180)" } else { "" }
    );
    println!("take the photo, then run:  argonctl oled --off");
    ExitCode::SUCCESS
}

/// Prints a frame using half-block characters, two pixel rows per terminal row.
fn print_preview(fb: &FrameBuffer) {
    println!("  +{}+", "-".repeat(WIDTH));
    for y in (0..HEIGHT).step_by(2) {
        let line: String = (0..WIDTH)
            .map(|x| match (fb.pixel(x, y), fb.pixel(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            })
            .collect();
        println!("  |{line}|");
    }
    println!("  +{}+", "-".repeat(WIDTH));
}
