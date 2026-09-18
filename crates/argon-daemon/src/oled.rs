// SPDX-License-Identifier: GPL-3.0-or-later
//! The daemon's OLED thread: a status page on the case display.
//!
//! It reads the status file the UPS thread publishes rather than sharing memory with it. That
//! costs one small file read per redraw and buys complete separation from the thread that
//! decides poweroffs, which is the one piece of this daemon that must not be disturbed by a
//! display bug. It also means the page is subject to exactly the staleness rule the tray
//! applies: if the UPS thread stops publishing, the case shows STALE, not the last number.

use argon_device::config::Config;
use argon_device::oled::Oled;
use argon_device::oled_page::{self, PageInput};
use argon_device::status::{self, UpsStatus};
use argon_hal::discovery;
use argon_hal::fan_hwmon::PwmFan;
use argon_hal::i2c::LinuxI2c;
use argon_hal::thermal::{TemperatureSource, ThermalZone};
use argon_proto::oled::{self, FrameBuffer};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// Starts the status page, if it is enabled and allowed.
///
/// Returns `None` -- after saying why, unless the display is simply not enabled -- when the
/// mode forbids device writes, the vendor daemon is running, or no panel answers.
pub fn spawn(
    config: &Config,
    active_units: &[String],
    stopping: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    if !config.oled.enabled {
        return None;
    }

    // The configured mode, not the fan's possibly-degraded one: MCU contention is about the
    // fan, and says nothing about whether this display may be written.
    let mode = config.mode().unwrap_or_default();
    if !mode.allows_writes() {
        eprintln!(
            "argond: oled: enabled, but mode is {mode}; drawing on the display is a device \
             write, so it stays off"
        );
        return None;
    }
    if active_units.iter().any(|u| u == "argononed.service") {
        eprintln!(
            "argond: oled: argononed is running and has its own code for this display; not \
             drawing, so the two do not overwrite each other"
        );
        return None;
    }

    let bus_path = if config.mcu.bus == "auto" {
        discovery::header_i2c_bus().map(|p| p.display().to_string())
    } else {
        Some(config.mcu.bus.clone())
    };
    let Some(bus_path) = bus_path else {
        eprintln!("argond: oled: no I2C bus found; is dtparam=i2c_arm=on set in config.txt?");
        return None;
    };
    // Bound to the panel's address: this transport refuses a write to anything else on the
    // bus, which on this machine also carries the DAC.
    let bus = match LinuxI2c::open(&bus_path, u16::from(oled::ADDR)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("argond: oled: cannot open {bus_path}: {e}");
            return None;
        }
    };
    let mut panel = Oled::new(bus, config.oled.flip);
    match panel.is_present() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!(
                "argond: oled: enabled, but nothing answers at 0x{:02x} on {bus_path}",
                oled::ADDR
            );
            return None;
        }
        Err(e) => {
            eprintln!("argond: oled: probing the panel failed: {e}");
            return None;
        }
    }

    eprintln!(
        "argond: oled: status page on {bus_path} @ 0x{:02x}, contrast {}, redraw every {}s",
        oled::ADDR,
        config.oled.contrast,
        config.oled.refresh_s
    );
    let settings = Settings {
        state_file: PathBuf::from(&config.ups.state_file),
        contrast: config.oled.contrast,
        interval: Duration::from_secs(config.oled.refresh_s.max(1)),
    };
    Some(std::thread::spawn(move || {
        run(&mut panel, &settings, &stopping);
    }))
}

struct Settings {
    state_file: PathBuf,
    contrast: u8,
    interval: Duration,
}

fn run(panel: &mut Oled<LinuxI2c>, settings: &Settings, stopping: &AtomicBool) {
    let started = Instant::now();
    let mut sensor = ThermalZone::find_cpu().ok();
    let fan = PwmFan::find();
    let mut shown: Option<FrameBuffer> = None;
    let mut needs_init = true;
    let mut error_logged = false;

    while !stopping.load(Ordering::Relaxed) {
        let status = read_status(&settings.state_file);
        let input = PageInput {
            status: status.as_ref(),
            now: SystemTime::now(),
            cpu_decicelsius: sensor.as_mut().and_then(|s| s.read_decicelsius().ok()),
            fan: fan.as_ref().and_then(PwmFan::read),
        };
        let page = oled_page::page(&input, &status::local_hhmm);
        let frame = oled_page::draw(&page, oled_page::shift_at(started.elapsed()));

        // Only write when the picture changed. Most redraws change nothing, and the bus is
        // shared.
        if needs_init || shown.as_ref() != Some(&frame) {
            match draw_frame(panel, &frame, settings.contrast, needs_init) {
                Ok(()) => {
                    shown = Some(frame);
                    needs_init = false;
                    if error_logged {
                        eprintln!("argond: oled: display back");
                        error_logged = false;
                    }
                }
                Err(e) => {
                    // A panel that was unplugged and plugged back in has lost its
                    // configuration, so the next attempt starts from the init sequence.
                    needs_init = true;
                    shown = None;
                    if !error_logged {
                        eprintln!("argond: oled: write failed, will keep retrying: {e}");
                        error_logged = true;
                    }
                }
            }
        }
        sleep_unless_stopping(settings.interval, stopping);
    }

    // Blank on the way out. A daemon that has stopped must not leave a status page behind:
    // it would go on claiming whatever it last said, and a static image burns in. SIGKILL
    // skips this, which is what the unit's ExecStopPost is for.
    match panel.off() {
        Ok(()) => eprintln!("argond: oled: display blanked"),
        Err(e) => eprintln!("argond: oled: could not blank the display: {e}"),
    }
}

fn draw_frame(
    panel: &mut Oled<LinuxI2c>,
    frame: &FrameBuffer,
    contrast: u8,
    init: bool,
) -> argon_hal::Result<()> {
    if init {
        panel.init()?;
        panel.set_contrast(contrast)?;
    }
    panel.flush(frame)
}

fn read_status(path: &Path) -> Option<UpsStatus> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| UpsStatus::parse(&t).ok())
}

/// Sleeps in short slices so a stop request is honoured promptly.
fn sleep_unless_stopping(total: Duration, stopping: &AtomicBool) {
    let slice = Duration::from_millis(250);
    let mut left = total;
    while !left.is_zero() && !stopping.load(Ordering::Relaxed) {
        let d = left.min(slice);
        std::thread::sleep(d);
        left = left.saturating_sub(d);
    }
}
