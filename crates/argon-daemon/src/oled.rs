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

/// The status page's on/off switch, shared with the D-Bus service.
///
/// Switched from the tray. The choice survives restarts and reboots: switched off, a marker file
/// sits in argond's state directory, and the page stays dark until switched on again.
#[derive(Debug)]
pub struct OledControl {
    /// A panel is being driven -- the thread started. Otherwise there is nothing to switch.
    available: AtomicBool,
    /// The page is wanted on.
    on: AtomicBool,
    /// Where "switched off" is remembered.
    record: Option<PathBuf>,
}

impl OledControl {
    /// A switch remembered at `record`: on unless the file exists.
    #[must_use]
    pub fn new(record: Option<PathBuf>) -> Self {
        let on = !record.as_deref().is_some_and(Path::exists);
        Self {
            available: AtomicBool::new(false),
            on: AtomicBool::new(on),
            record,
        }
    }

    /// The switch as argond's systemd unit keeps it: `$STATE_DIRECTORY/oled-off`.
    #[must_use]
    pub fn from_state_directory() -> Self {
        let record = std::env::var_os("STATE_DIRECTORY").and_then(|dirs| {
            let first = dirs.to_string_lossy().split(':').next()?.to_owned();
            (!first.is_empty()).then(|| PathBuf::from(first).join("oled-off"))
        });
        Self::new(record)
    }

    /// "on", "off", or "na" when no panel is being driven.
    #[must_use]
    pub fn state(&self) -> &'static str {
        if !self.available.load(Ordering::Relaxed) {
            "na"
        } else if self.on.load(Ordering::Relaxed) {
            "on"
        } else {
            "off"
        }
    }

    /// Switches the page on or off.
    ///
    /// # Errors
    ///
    /// Fails when no panel is being driven. Failing to remember the choice is logged, not an
    /// error: the switch itself has worked.
    pub fn set(&self, on: bool) -> Result<(), String> {
        if !self.available.load(Ordering::Relaxed) {
            return Err("no case display is being driven (see [oled] in the config)".into());
        }
        self.on.store(on, Ordering::Relaxed);
        if let Some(record) = &self.record {
            let remembered = if on {
                std::fs::remove_file(record).or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(e)
                    }
                })
            } else {
                std::fs::write(record, "switched off from the desktop\n")
            };
            if let Err(e) = remembered {
                eprintln!(
                    "argond: oled: switched, but cannot remember it in {}: {e}",
                    record.display()
                );
            }
        }
        Ok(())
    }

    fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// Marks a panel as driven, for other modules' tests.
    #[cfg(test)]
    pub fn mark_available_for_tests(&self) {
        self.available.store(true, Ordering::Relaxed);
    }
}

/// Starts the status page, if it is enabled and allowed.
///
/// Returns `None` -- after saying why, unless the display is simply not enabled -- when the
/// mode forbids device writes, the vendor daemon is running, or no panel answers.
pub fn spawn(
    config: &Config,
    active_units: &[String],
    stopping: Arc<AtomicBool>,
    control: Arc<OledControl>,
) -> Option<JoinHandle<()>> {
    if !config.oled.enabled {
        // Said out loud: otherwise "the tray shows no display switch" has no visible cause.
        eprintln!("argond: oled: not enabled ([oled] enabled = true switches the status page on)");
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
    control.available.store(true, Ordering::Relaxed);
    if !control.is_on() {
        eprintln!("argond: oled: switched off from the desktop; staying dark until switched on");
    }
    Some(std::thread::spawn(move || {
        run(&mut panel, &settings, &stopping, &control);
        control.available.store(false, Ordering::Relaxed);
    }))
}

struct Settings {
    state_file: PathBuf,
    contrast: u8,
    interval: Duration,
}

fn run(
    panel: &mut Oled<LinuxI2c>,
    settings: &Settings,
    stopping: &AtomicBool,
    control: &OledControl,
) {
    let started = Instant::now();
    let mut sensor = ThermalZone::find_cpu().ok();
    let fan = PwmFan::find();
    let mut shown: Option<FrameBuffer> = None;
    let mut needs_init = true;
    let mut error_logged = false;
    let mut dark = false;

    while !stopping.load(Ordering::Relaxed) {
        if !control.is_on() {
            if !dark {
                match panel.off() {
                    Ok(()) => eprintln!("argond: oled: switched off"),
                    Err(e) => eprintln!("argond: oled: could not switch off: {e}"),
                }
                dark = true;
                shown = None;
            }
            sleep_until(settings.interval, stopping, || control.is_on());
            continue;
        }
        if dark {
            eprintln!("argond: oled: switched on");
            dark = false;
            needs_init = true;
        }
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
        sleep_until(settings.interval, stopping, || !control.is_on());
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

/// Sleeps in short slices, so a stop request -- or a flip of the switch -- is honoured promptly.
fn sleep_until(total: Duration, stopping: &AtomicBool, wake: impl Fn() -> bool) {
    let slice = Duration::from_millis(250);
    let mut left = total;
    while !left.is_zero() && !stopping.load(Ordering::Relaxed) && !wake() {
        let d = left.min(slice);
        std::thread::sleep(d);
        left = left.saturating_sub(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("argon-oled-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("oled-off")
    }

    #[test]
    fn nothing_to_switch_without_a_panel() {
        let c = OledControl::new(None);
        assert_eq!(c.state(), "na");
        assert!(c.set(false).is_err());
    }

    #[test]
    fn switching_off_is_remembered_across_a_restart() {
        let r = record("remember");
        let _ = std::fs::remove_file(&r);
        let c = OledControl::new(Some(r.clone()));
        c.available.store(true, Ordering::Relaxed);
        assert_eq!(c.state(), "on");
        c.set(false).unwrap();
        assert_eq!(c.state(), "off");
        assert!(r.exists());
        // A new argond reads it back.
        let again = OledControl::new(Some(r.clone()));
        again.available.store(true, Ordering::Relaxed);
        assert_eq!(again.state(), "off");
        again.set(true).unwrap();
        assert!(!r.exists());
        assert!(OledControl::new(Some(r.clone())).is_on());
        let _ = std::fs::remove_dir_all(r.parent().unwrap());
    }
}
