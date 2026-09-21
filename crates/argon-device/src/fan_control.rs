// SPDX-License-Identifier: GPL-3.0-or-later
//! The fan, whichever thing is actually driving it.
//!
//! Two backends, because Argon cases do not agree on how the fan is wired:
//!
//! - [`McuFan`] — the documented I2C MCU at `0x1a`: the ONE V2 (Pi 4), the ONE V3 (Pi 5), EON,
//!   Fan HAT.
//! - [`KernelFan`] — the Pi 5's own PWM fan header under the kernel thermal governor. This is
//!   what an Argon ONE V5 on a Pi 5 uses, where there is no MCU on the bus at all.
//!
//! The control loop is written against the trait and never learns which one it has. That was
//! the point of making it a trait before knowing there would be two.

use crate::mcu::Mcu;
use argon_hal::i2c::I2cBus;
use argon_hal::{Error, Result, fan_hwmon};
use argon_proto::fan::FanDuty;

/// Something that can report, and possibly set, the fan.
pub trait FanControl {
    /// Sets the fan duty.
    ///
    /// # Errors
    ///
    /// Fails on device error, or if this backend does not permit writes.
    fn set(&mut self, duty: FanDuty) -> Result<()>;

    /// The duty currently believed to be in force, if known.
    fn current(&self) -> Option<FanDuty>;

    /// Measured speed in RPM, if the fan reports one.
    ///
    /// The I2C MCU has no tachometer; the Pi 5 header does. Returning `None` is the normal
    /// answer for half the hardware this supports, not a failure.
    fn rpm(&self) -> Option<u32> {
        None
    }

    /// Whether this backend can actually set the fan.
    fn is_writable(&self) -> bool;

    /// A short description for logs and status output.
    fn describe(&self) -> String;
}

impl<B: I2cBus> FanControl for Mcu<B> {
    fn set(&mut self, duty: FanDuty) -> Result<()> {
        self.set_fan(duty)
    }

    fn current(&self) -> Option<FanDuty> {
        self.duty()
    }

    fn is_writable(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("Argon MCU at i2c 0x{:02x}", crate::mcu::ADDR)
    }
}

/// The Raspberry Pi's own PWM fan, under the kernel thermal governor.
///
/// **Read-only, deliberately.** Writing `pwm1` does not take control — the governor rewrites
/// it at the next thermal update, so that is a fight rather than control. The only mechanism
/// that does take control is disabling the thermal zone, and a zone is all-or-nothing: that
/// would also disable the 110 °C critical trip, i.e. the software thermal shutdown.
///
/// Trading the machine's thermal emergency protection for a finer fan curve is not a trade
/// this daemon should make on its own. If manual control is genuinely wanted, the honest
/// route is a device-tree overlay removing the cooling maps at boot — deliberate, visible and
/// reversible — rather than a daemon quietly writing `disabled` to a sysfs file.
///
/// See `docs/protocol/captures/OBS-2026-09-16-taking-the-pi5-fan.md`.
pub struct KernelFan {
    fan: fan_hwmon::PwmFan,
}

impl KernelFan {
    /// Finds the kernel PWM fan, if this machine has one.
    #[must_use]
    pub fn find() -> Option<Self> {
        fan_hwmon::PwmFan::find().map(|fan| Self { fan })
    }

    /// The cooling devices the governor has bound, if any.
    #[must_use]
    pub fn governor_state() -> Vec<fan_hwmon::CoolingDevice> {
        fan_hwmon::cooling_devices()
            .into_iter()
            .filter(|c| c.kind.contains("fan"))
            .collect()
    }
}

impl FanControl for KernelFan {
    fn set(&mut self, duty: FanDuty) -> Result<()> {
        Err(Error::WriteBlocked {
            what: format!("kernel pwm fan <- {duty}"),
            reason: "the kernel thermal governor owns this fan. Taking it over means \
                     disabling the whole thermal zone, which also disables the 110C critical \
                     trip -- see docs/protocol/captures/OBS-2026-09-16-taking-the-pi5-fan.md",
        })
    }

    fn current(&self) -> Option<FanDuty> {
        // Report what the governor has set, scaled from the kernel's 0-255 to a percentage.
        // Rounded rather than truncated: a pwm of 3 is 1%, not 0%, and reporting a running
        // fan as stopped would be the wrong error to make.
        let reading = self.fan.read()?;
        let percent = (u32::from(reading.pwm) * 100 + 127) / 255;
        Some(FanDuty::clamped(u8::try_from(percent).unwrap_or(100)))
    }

    fn rpm(&self) -> Option<u32> {
        self.fan.read().and_then(|r| r.rpm)
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn describe(&self) -> String {
        let governor = Self::governor_state();
        let state = governor.first().map_or_else(
            || "unbound".to_owned(),
            |c| format!("{}/{}", c.state, c.max_state),
        );
        format!(
            "kernel pwm fan at {} (governor state {state}, read-only)",
            self.fan.path().display()
        )
    }
}

/// Picks whichever backend this machine actually has.
///
/// Prefers the MCU when one answers, because a machine with an Argon MCU is one where the fan
/// is genuinely ours to drive. Falls back to the kernel fan, which is what a Pi 5 has.
///
/// # Errors
///
/// Fails if neither backend is available.
pub fn detect<B: I2cBus + 'static>(mcu: Mcu<B>) -> Result<Box<dyn FanControl>> {
    let mut mcu = mcu;
    if mcu.is_present().unwrap_or(false) {
        return Ok(Box::new(mcu));
    }
    KernelFan::find().map_or_else(
        || {
            Err(Error::Parse {
                what: "a fan controller",
                got: "no Argon MCU answered, and this machine has no kernel PWM fan".to_owned(),
            })
        },
        |k| Ok(Box::new(k) as Box<dyn FanControl>),
    )
}
