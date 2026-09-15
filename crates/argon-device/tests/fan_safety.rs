// SPDX-License-Identifier: GPL-3.0-or-later
//! The fan must never be left stopped because the daemon stopped.

use argon_device::mcu::{ADDR, Dialect, Mcu};
use argon_device::safety::{DEFAULT_SAFE_DUTY, FanSafeGuard, HOT_SAFE_DUTY};
use argon_hal::Result;
use argon_hal::i2c::{I2cBus, ReadOnly};
use argon_proto::fan::FanDuty;
use argon_sim::mcu::LegacyMcu;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct SimBus {
    mcu: Arc<Mutex<LegacyMcu>>,
}

impl SimBus {
    fn new() -> Self {
        Self {
            mcu: Arc::new(Mutex::new(LegacyMcu::new())),
        }
    }
}

impl I2cBus for SimBus {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        assert_eq!(addr, ADDR);
        self.mcu.lock().unwrap().receive(data);
        Ok(())
    }
    fn probe(&mut self, _addr: u8) -> Result<bool> {
        Ok(true)
    }
    fn describe(&self) -> String {
        "sim".into()
    }
}

/// Builds an MCU behind a mutex, plus a handle to the simulated device's state.
fn rig() -> (Arc<Mutex<Mcu<SimBus>>>, Arc<Mutex<LegacyMcu>>) {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    (
        Arc::new(Mutex::new(Mcu::new(bus, Dialect::default()))),
        state,
    )
}

fn fan_percent(state: &Arc<Mutex<LegacyMcu>>) -> u8 {
    state.lock().unwrap().state().fan_percent
}

#[test]
fn dropping_the_guard_restores_a_running_duty() {
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();
    assert_eq!(fan_percent(&state), 0);

    {
        let _guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
    }

    assert_eq!(
        fan_percent(&state),
        DEFAULT_SAFE_DUTY,
        "fan left stopped after the guard dropped"
    );
}

#[test]
fn a_panic_while_holding_the_guard_still_restores_the_fan() {
    // The case this guard exists for. Note this only works because the workspace release
    // profile uses panic = "unwind"; under panic = "abort" a Drop impl does not run at all,
    // which was verified rather than assumed.
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();

    let mcu_for_thread = Arc::clone(&mcu);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = FanSafeGuard::new(mcu_for_thread, FanDuty::clamped(DEFAULT_SAFE_DUTY));
        panic!("something went wrong in the control loop");
    }));

    assert!(result.is_err(), "the panic should have propagated");
    assert_eq!(
        fan_percent(&state),
        DEFAULT_SAFE_DUTY,
        "fan left stopped after a panic"
    );
}

#[test]
fn a_poisoned_mutex_does_not_stop_the_guard() {
    // A poisoned lock means another thread panicked while holding the MCU -- exactly when the
    // fan most needs setting. Giving up there would disable the guard in its most important
    // case.
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();

    let poisoner = Arc::clone(&mcu);
    let _ = std::thread::spawn(move || {
        let _held = poisoner.lock().unwrap();
        panic!("poison the lock");
    })
    .join();
    assert!(mcu.is_poisoned(), "test setup failed to poison the mutex");

    {
        let _guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
    }

    assert_eq!(
        fan_percent(&state),
        DEFAULT_SAFE_DUTY,
        "guard gave up on a poisoned mutex"
    );
}

#[test]
fn restore_now_also_survives_a_poisoned_mutex() {
    // Drop recovers from poisoning; restore_now must agree, or the same daemon behaves
    // differently depending on which path happens to run first.
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();

    let poisoner = Arc::clone(&mcu);
    let _ = std::thread::spawn(move || {
        let _held = poisoner.lock().unwrap();
        panic!("poison the lock");
    })
    .join();
    assert!(mcu.is_poisoned());

    let guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
    assert!(
        guard.restore_now(),
        "restore_now gave up on a poisoned mutex"
    );
    assert_eq!(fan_percent(&state), DEFAULT_SAFE_DUTY);
}

#[test]
fn a_hot_machine_gets_full_duty() {
    let (mcu, state) = rig();
    {
        let _guard = FanSafeGuard::for_temperature(Arc::clone(&mcu), Some(80), 75);
    }
    assert_eq!(fan_percent(&state), HOT_SAFE_DUTY);
}

#[test]
fn a_cool_machine_gets_the_ordinary_safe_duty() {
    let (mcu, state) = rig();
    {
        let _guard = FanSafeGuard::for_temperature(Arc::clone(&mcu), Some(45), 75);
    }
    assert_eq!(fan_percent(&state), DEFAULT_SAFE_DUTY);
}

#[test]
fn an_unknown_temperature_is_treated_as_cool_but_never_as_stopped() {
    let (mcu, state) = rig();
    {
        let _guard = FanSafeGuard::for_temperature(Arc::clone(&mcu), None, 75);
    }
    let p = fan_percent(&state);
    assert!(p > 0, "unknown temperature left the fan stopped");
    assert_eq!(p, DEFAULT_SAFE_DUTY);
}

#[test]
fn the_safe_duty_always_spins_the_fan() {
    // A "safe" duty below the speed at which the fan physically turns would be a stopped fan
    // wearing a different name.
    assert!(FanDuty::clamped(DEFAULT_SAFE_DUTY).is_spinning());
    assert!(FanDuty::clamped(HOT_SAFE_DUTY).is_spinning());
}

#[test]
fn disarming_leaves_the_fan_alone() {
    // Used when handing control back to another process: restoring a duty there would fight
    // whatever is taking over.
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Percent(20)).unwrap();
    {
        let mut guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
        guard.disarm();
    }
    assert_eq!(
        fan_percent(&state),
        20,
        "a disarmed guard still wrote to the fan"
    );
}

#[test]
fn restore_now_acts_without_consuming_the_guard() {
    // Signal handlers need to act before the process unwinds.
    let (mcu, state) = rig();
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();

    let guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
    assert!(guard.restore_now());
    assert_eq!(fan_percent(&state), DEFAULT_SAFE_DUTY);

    // And the guard is still armed afterwards.
    mcu.lock().unwrap().set_fan(FanDuty::Off).unwrap();
    drop(guard);
    assert_eq!(fan_percent(&state), DEFAULT_SAFE_DUTY);
}

#[test]
fn a_read_only_transport_makes_the_guard_a_no_op_not_a_crash() {
    // In read-only mode there is nothing to restore, because nothing was ever changed. The
    // guard must not panic on the refused write -- panicking inside Drop during an unwind
    // aborts the process.
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mcu = Arc::new(Mutex::new(Mcu::new(ReadOnly(bus), Dialect::default())));
    {
        let _guard = FanSafeGuard::new(Arc::clone(&mcu), FanDuty::clamped(DEFAULT_SAFE_DUTY));
    }
    assert_eq!(
        fan_percent(&state),
        0,
        "a read-only guard reached the device"
    );
}
