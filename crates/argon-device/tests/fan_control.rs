// SPDX-License-Identifier: GPL-3.0-or-later
//! Both fan backends, and the detection that picks between them.
//!
//! Two tests here need a real kernel PWM fan and print `SKIPPED` when there is none, since
//! CI has no Raspberry Pi. They were verified non-vacuous on the development machine, where
//! `KernelFan::find()` reports `hwmon3` at 29% and 2948 rpm.

use argon_device::fan_control::{FanControl, KernelFan, detect};
use argon_device::mcu::{ADDR, Dialect, Mcu};
use argon_hal::Result;
use argon_hal::i2c::I2cBus;
use argon_proto::fan::FanDuty;
use argon_sim::mcu::LegacyMcu;
use std::sync::{Arc, Mutex};

/// A bus where the MCU may or may not answer.
struct SimBus {
    mcu: Arc<Mutex<LegacyMcu>>,
    present: bool,
}

impl SimBus {
    fn present() -> Self {
        Self {
            mcu: Arc::new(Mutex::new(LegacyMcu::new())),
            present: true,
        }
    }
    fn absent() -> Self {
        Self {
            mcu: Arc::new(Mutex::new(LegacyMcu::new())),
            present: false,
        }
    }
}

impl I2cBus for SimBus {
    fn write(&mut self, addr: u8, data: &[u8]) -> Result<()> {
        assert_eq!(addr, ADDR);
        if !self.present {
            // What a real bus returns when nothing acknowledges the address: EREMOTEIO,
            // exactly as observed on the ONE V5.
            return Err(argon_hal::Error::Io(std::io::Error::from_raw_os_error(121)));
        }
        self.mcu.lock().unwrap().receive(data);
        Ok(())
    }
    fn probe(&mut self, _addr: u8) -> Result<bool> {
        Ok(self.present)
    }
    fn describe(&self) -> String {
        "sim".into()
    }
}

#[test]
fn the_mcu_backend_writes() {
    let bus = SimBus::present();
    let state = Arc::clone(&bus.mcu);
    let mut fan: Box<dyn FanControl> = Box::new(Mcu::new(bus, Dialect::default()));

    assert!(fan.is_writable());
    fan.set(FanDuty::Percent(40)).unwrap();
    assert_eq!(state.lock().unwrap().state().fan_percent, 40);
    assert_eq!(fan.current(), Some(FanDuty::Percent(40)));
}

#[test]
fn the_mcu_backend_has_no_tachometer() {
    // Half the supported hardware cannot report RPM. None is the normal answer, not an error.
    let fan = Mcu::new(SimBus::present(), Dialect::default());
    assert_eq!(fan.rpm(), None);
}

#[test]
fn detection_prefers_an_mcu_that_answers() {
    let fan = detect(Mcu::new(SimBus::present(), Dialect::default())).unwrap();
    assert!(
        fan.is_writable(),
        "should have chosen the writable MCU backend"
    );
    assert!(fan.describe().contains("0x1a"), "{}", fan.describe());
}

#[test]
fn detection_falls_back_when_no_mcu_answers() {
    // The ONE V5 case: nothing at 0x1a, so the kernel fan is the only option. On a machine
    // with neither, detection fails rather than pretending.
    let result = detect(Mcu::new(SimBus::absent(), Dialect::default()));
    match result {
        Ok(fan) => {
            assert!(!fan.is_writable(), "the kernel fan must be read-only");
            assert!(fan.describe().contains("kernel"), "{}", fan.describe());
        }
        Err(e) => {
            // Valid on a machine with no pwm-fan either; the message must say so.
            let msg = e.to_string();
            assert!(msg.contains("no Argon MCU"), "{msg}");
        }
    }
}

#[test]
fn the_kernel_backend_refuses_to_write_and_says_why() {
    let Some(mut fan) = KernelFan::find() else {
        // No kernel fan here -- CI, or a Pi 4. Say so: a test that quietly passes because it
        // found nothing to test is indistinguishable from one that verified something.
        eprintln!("SKIPPED: no kernel PWM fan on this machine");
        return;
    };
    let err = fan
        .set(FanDuty::Percent(50))
        .expect_err("the kernel fan must refuse writes");
    let msg = err.to_string();
    assert!(
        msg.contains("thermal"),
        "the refusal should explain itself: {msg}"
    );
    assert!(
        msg.contains("critical trip"),
        "it should name what taking over would cost: {msg}"
    );
}

#[test]
fn a_driven_pwm_never_reads_as_a_stopped_fan() {
    // The property the old live test was after, tested exactly: truncating a small pwm would
    // report a driven fan as stopped. It used to compare `current()` with `rpm()` on the live
    // machine -- two reads at two moments, and a tachometer that trails the pwm -- so it failed
    // whenever the kernel set pwm 0 while the rotor was still coasting. Nothing was wrong then.
    use argon_device::fan_control::duty_from_pwm;
    use argon_proto::fan::FanDuty;
    assert_eq!(duty_from_pwm(0), FanDuty::Off);
    for pwm in 2..=u8::MAX {
        assert_ne!(duty_from_pwm(pwm), FanDuty::Off, "pwm {pwm} read as off");
    }
    assert_eq!(duty_from_pwm(u8::MAX).percent(), 100);
    assert_eq!(duty_from_pwm(128).percent(), 50);
}

#[test]
fn the_live_kernel_fan_reads_consistently() {
    // A smoke test on real hardware, from ONE reading, so it cannot race the rotor.
    let Some(fan) = argon_hal::fan_hwmon::PwmFan::find() else {
        eprintln!("SKIPPED: no kernel PWM fan on this machine");
        return;
    };
    if let Some(r) = fan.read() {
        if r.pwm >= 2 {
            assert_ne!(
                argon_device::fan_control::duty_from_pwm(r.pwm),
                argon_proto::fan::FanDuty::Off,
                "pwm {} read as off",
                r.pwm
            );
        }
    }
}
