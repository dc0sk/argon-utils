// SPDX-License-Identifier: GPL-3.0-or-later
//! Keeping the fan running when things go wrong.
//!
//! The failure this module exists to prevent: the daemon stops — crash, panic, SIGTERM,
//! a bad config reload — while the fan is at a low duty or stopped, and the MCU holds that
//! value indefinitely while the machine heats up.
//!
//! # Why this cannot be the only layer
//!
//! A `Drop` guard covers an orderly teardown and a panic. It cannot cover `SIGKILL`, a power
//! loss, or a kernel OOM kill, because the process stops executing. Those need
//! `ExecStopPost=` in the systemd unit, which runs after the process is gone.
//!
//! It also cannot cover `panic = "abort"`, under which `Drop` implementations do not run at
//! all. That is why the release profile in this workspace sets `panic = "unwind"`, with the
//! reasoning recorded there — a guard that is inert in shipped builds while passing its own
//! tests is worse than no guard, because it also removes the motivation to add a real one.

use crate::mcu::Mcu;
use argon_hal::i2c::I2cBus;
use argon_proto::fan::FanDuty;
use std::sync::{Arc, Mutex};

/// The duty to fall back to when the daemon stops.
///
/// Not the maximum: a machine that is merely idle should not be left howling indefinitely,
/// and an operator who finds the fan at full has no signal about whether that is the failure
/// or the intent. Not the minimum either, for the obvious reason.
pub const DEFAULT_SAFE_DUTY: u8 = 55;

/// The duty to fall back to when the machine was already hot.
pub const HOT_SAFE_DUTY: u8 = 100;

/// Restores a safe fan duty when dropped.
///
/// Holds the MCU behind a mutex so the guard and the control loop can share it. The guard
/// deliberately does not report failure: it runs while something has already gone wrong, and
/// panicking inside a `Drop` during an unwind aborts the process.
pub struct FanSafeGuard<B: I2cBus> {
    mcu: Arc<Mutex<Mcu<B>>>,
    safe_duty: FanDuty,
    disarmed: bool,
}

impl<B: I2cBus> FanSafeGuard<B> {
    /// Arms a guard that will restore `safe_duty`.
    pub fn new(mcu: Arc<Mutex<Mcu<B>>>, safe_duty: FanDuty) -> Self {
        Self {
            mcu,
            safe_duty,
            disarmed: false,
        }
    }

    /// Arms a guard whose fallback depends on how hot the machine was.
    ///
    /// A machine already above `hot_threshold_c` gets full duty; anything else gets the
    /// ordinary safe duty.
    pub fn for_temperature(
        mcu: Arc<Mutex<Mcu<B>>>,
        last_temp_c: Option<i32>,
        hot_threshold_c: i32,
    ) -> Self {
        let duty = if last_temp_c.is_some_and(|t| t >= hot_threshold_c) {
            FanDuty::clamped(HOT_SAFE_DUTY)
        } else {
            FanDuty::clamped(DEFAULT_SAFE_DUTY)
        };
        Self::new(mcu, duty)
    }

    /// The duty this guard would restore.
    #[must_use]
    pub const fn safe_duty(&self) -> FanDuty {
        self.safe_duty
    }

    /// Prevents the guard from acting.
    ///
    /// For the one case where the fan is deliberately being left as-is: handing control back
    /// to another process during a rollback. Restoring a duty there would fight whatever is
    /// taking over.
    pub const fn disarm(&mut self) {
        self.disarmed = true;
    }

    /// Restores the safe duty now, without consuming the guard.
    ///
    /// Returns whether the write succeeded. Used by signal handlers, which must act before
    /// the process unwinds.
    ///
    /// Recovers from a poisoned mutex for the same reason [`Drop`] does: poisoning means
    /// another thread panicked while holding the MCU, which is exactly when the fan most
    /// needs setting.
    #[must_use]
    pub fn restore_now(&self) -> bool {
        let mut mcu = match self.mcu.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        mcu.set_fan(self.safe_duty).is_ok()
    }
}

impl<B: I2cBus> Drop for FanSafeGuard<B> {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        // A poisoned mutex means another thread panicked while holding the MCU. That is
        // precisely when the fan most needs setting, so recover the lock rather than give up.
        let mut mcu = match self.mcu.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Errors are deliberately swallowed: this runs while something has already failed,
        // and panicking in a Drop during an unwind aborts the process.
        let _ = mcu.set_fan(self.safe_duty);
    }
}
