// SPDX-License-Identifier: GPL-3.0-or-later
//! The fan control task.
//!
//! [`FanTask::step`] performs exactly one iteration and never sleeps, so the whole policy is
//! testable against a scripted temperature series without a Raspberry Pi and without waiting
//! for a real CPU to heat up. The daemon loop around it is thin by design: logic that only
//! runs inside a `loop { sleep }` is logic that only gets tested by watching it.

use crate::mcu::Mcu;
use argon_hal::i2c::I2cBus;
use argon_hal::thermal::TemperatureSource;
use argon_proto::fan::{FanController, FanDuty};
use std::sync::{Arc, Mutex};

/// What one iteration did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Temperature read, duty applied.
    Applied {
        /// The temperature read, in tenths of a degree.
        decicelsius: i32,
        /// The duty now in force.
        duty: FanDuty,
        /// Whether this iteration wrote to the device.
        wrote: bool,
    },
    /// The temperature could not be read, and the fan was forced to a safe duty.
    ///
    /// Not a no-op: a control loop that cannot see the temperature must not keep applying
    /// whatever it last decided, because it no longer has any basis for that decision.
    SensorFailed {
        /// The duty forced in response.
        fallback: FanDuty,
        /// How many consecutive reads have now failed.
        consecutive: u32,
    },
}

/// Drives the fan from a temperature source.
pub struct FanTask<B: I2cBus, T: TemperatureSource> {
    mcu: Arc<Mutex<Mcu<B>>>,
    source: T,
    controller: FanController,
    /// Duty to force when the sensor cannot be read.
    sensor_fail_duty: FanDuty,
    /// Consecutive failed reads.
    failures: u32,
    /// Last duty actually written, to avoid rewriting an unchanged value.
    last_written: Option<FanDuty>,
    /// Last temperature successfully read.
    last_temp: Option<i32>,
}

impl<B: I2cBus, T: TemperatureSource> FanTask<B, T> {
    /// Creates a task.
    pub const fn new(
        mcu: Arc<Mutex<Mcu<B>>>,
        source: T,
        controller: FanController,
        sensor_fail_duty: FanDuty,
    ) -> Self {
        Self {
            mcu,
            source,
            controller,
            sensor_fail_duty,
            failures: 0,
            last_written: None,
            last_temp: None,
        }
    }

    /// The last temperature read, in tenths of a degree.
    pub const fn last_temperature(&self) -> Option<i32> {
        self.last_temp
    }

    /// Consecutive failed sensor reads.
    pub const fn consecutive_failures(&self) -> u32 {
        self.failures
    }

    /// Performs one control iteration.
    ///
    /// Never sleeps, and never returns an error for a failed sensor read: losing the sensor
    /// is a condition to handle, not a reason to stop controlling the fan. A device write
    /// that fails is surfaced, because that means the daemon is no longer in control and the
    /// caller needs to decide what to do about it.
    ///
    /// # Errors
    ///
    /// Fails if writing to the MCU fails.
    pub fn step(&mut self) -> argon_hal::Result<Step> {
        let Ok(decicelsius) = self.source.read_decicelsius() else {
            self.failures = self.failures.saturating_add(1);
            // Reset the controller so that when the sensor returns, hysteresis does not hold
            // a duty chosen from a temperature we can no longer vouch for.
            self.controller.reset();
            let fallback = self.sensor_fail_duty;
            self.write_if_changed(fallback)?;
            return Ok(Step::SensorFailed {
                fallback,
                consecutive: self.failures,
            });
        };

        self.failures = 0;
        self.last_temp = Some(decicelsius);
        let duty = self.controller.update(decicelsius);
        let wrote = self.write_if_changed(duty)?;
        Ok(Step::Applied {
            decicelsius,
            duty,
            wrote,
        })
    }

    /// Writes a duty only when it differs from the last one written.
    ///
    /// The rate limiter would drop a redundant write anyway, but it would also count it as
    /// suppressed — which would make the suppression metric measure our own chattiness
    /// rather than real curve activity.
    fn write_if_changed(&mut self, duty: FanDuty) -> argon_hal::Result<bool> {
        if self.last_written == Some(duty) {
            return Ok(false);
        }
        let mut mcu = match self.mcu.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        match mcu.set_fan(duty) {
            Ok(()) => {
                self.last_written = Some(duty);
                Ok(true)
            }
            // The transport declined for now, so nothing reached the device and nothing is
            // recorded as written: the next poll tries again. Not an error to the caller --
            // the loop is doing its job -- but emphatically not a success either.
            Err(argon_hal::Error::RateLimited { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }
}
