// SPDX-License-Identifier: GPL-3.0-or-later
//! `argonctl oled` — bring up the SSD1306 display.
//!
//! Previews in the terminal by default and touches nothing. `--write` sends the test pattern,
//! `--off` blanks and switches the panel off.

use argon_device::config::Config;
use argon_device::oled::{Oled, test_pattern};
use argon_device::oled_page::{self, PageInput};
use argon_device::status::{self, UpsStatus};
use argon_hal::fan_hwmon::PwmFan;
use argon_hal::i2c::LinuxI2c;
use argon_hal::thermal::{TemperatureSource, ThermalZone};
use argon_hal::{discovery, foreign};
use argon_proto::oled::{self, FrameBuffer, HEIGHT, WIDTH};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

#[derive(clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "command-line flags, which clap models as independent bools"
)]
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

    /// Use argond's live status page instead of the test pattern.
    ///
    /// Without --write this previews it in the terminal from the status file argond
    /// publishes; with --write it draws it on the panel once.
    #[arg(long, conflicts_with = "off")]
    pub status: bool,

    /// With --off: do nothing unless the configuration enables the OLED.
    ///
    /// For the service's stop hook, which should blank a display argond was driving and leave
    /// one it was not driving alone.
    #[arg(long, requires = "off")]
    pub if_enabled: bool,
}

pub fn run(args: &Args) -> ExitCode {
    if args.if_enabled {
        let enabled = config_enables_oled();
        if !enabled {
            // Silent success: this is the stop hook on a machine that does not use the display.
            return ExitCode::SUCCESS;
        }
    }

    let frame = if args.status {
        status_frame()
    } else {
        test_pattern()
    };

    if !args.write && !args.off {
        let what = if args.status {
            "argond's status page, from its status file"
        } else {
            "the test pattern"
        };
        println!("Preview of {what} (nothing is sent to the panel):\n");
        print_preview(&frame);
        let flag = if args.status { " --status" } else { "" };
        println!("\nSend it with:  argonctl oled{flag} --write");
        return ExitCode::SUCCESS;
    }

    if vendor_holds_display() {
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

    let contrast = load_config().map_or(64, |c| c.oled.contrast);
    if let Err(e) = panel
        .init()
        .and_then(|()| panel.set_contrast(contrast))
        .and_then(|()| panel.flush(&frame))
    {
        eprintln!("argonctl: {e}");
        return ExitCode::FAILURE;
    }
    println!(
        "{} sent to 0x{:02x} on {bus_str}{}",
        if args.status {
            "status page"
        } else {
            "test pattern"
        },
        oled::ADDR,
        if args.flip { " (rotated 180)" } else { "" }
    );
    println!("blank it again with:  argonctl oled --off");
    ExitCode::SUCCESS
}

/// Whether the vendor's daemon is running, which also drives this display. Says so if it is.
///
/// Its display thread is off on this machine, but a refusal is cheaper than working out why
/// the screen tears.
fn vendor_holds_display() -> bool {
    let vendor: Vec<String> = foreign::vendor_units()
        .into_iter()
        .filter(|u| u.unit == "argononed.service" && u.is_active())
        .map(|u| u.unit)
        .collect();
    if vendor.is_empty() {
        return false;
    }
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
    true
}

/// The page argond would draw right now, from the status file it publishes.
fn status_frame() -> FrameBuffer {
    let config = load_config().unwrap_or_default();
    let status = std::fs::read_to_string(&config.ups.state_file)
        .ok()
        .and_then(|t| UpsStatus::parse(&t).ok());
    let input = PageInput {
        status: status.as_ref(),
        now: SystemTime::now(),
        cpu_decicelsius: ThermalZone::find_cpu()
            .ok()
            .and_then(|mut s| s.read_decicelsius().ok()),
        fan: PwmFan::find().as_ref().and_then(PwmFan::read),
    };
    oled_page::draw(&oled_page::page(&input, &status::local_hhmm), (0, 0))
}

fn load_config() -> Option<Config> {
    let path = std::path::Path::new(argon_device::config::DEFAULT_PATH);
    path.exists().then(|| Config::load(path).ok()).flatten()
}

/// Whether the installed configuration turns the OLED on.
fn config_enables_oled() -> bool {
    load_config().is_some_and(|c| c.oled.enabled)
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
