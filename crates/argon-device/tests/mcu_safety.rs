// SPDX-License-Identifier: GPL-3.0-or-later
//! The safety properties of the MCU driver, asserted against a simulated MCU.
//!
//! The most important test here is [`legacy_firmware_never_receives_a_register_write`]. It
//! turns the single most damaging misconfiguration this project can make — addressing legacy
//! firmware with the register protocol, which sets the fan to full — into a build failure
//! rather than a field incident.

use argon_device::mcu::{ADDR, Dialect, Mcu, PowerMode};
use argon_hal::i2c::{DryRun, I2cBus, RateLimited, ReadOnly};
use argon_hal::{Error, Result};
use argon_proto::fan::FanDuty;
use argon_sim::mcu::{LegacyMcu, opcode};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// An I2C bus wired to a simulated legacy MCU.
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
        assert_eq!(addr, ADDR, "wrote to an unexpected address");
        self.mcu.lock().unwrap().receive(data);
        Ok(())
    }
    fn probe(&mut self, addr: u8) -> Result<bool> {
        Ok(addr == ADDR)
    }
    fn describe(&self) -> String {
        "simulated legacy MCU".into()
    }
}

#[test]
fn legacy_firmware_never_receives_a_register_write() {
    // The hazard, made concrete. On legacy firmware a register write `80 19` is read as the
    // single command byte 0x80 = 128, clamped to a fan duty of 100%. So this test asserts two
    // things at once: that the driver's default dialect produces a single-byte transaction,
    // and that the resulting fan duty is the one we asked for rather than full blast.
    let bus = SimBus::new();
    let mcu_state = Arc::clone(&bus.mcu);
    let mut mcu = Mcu::new(bus, Dialect::default());

    mcu.set_fan(FanDuty::Percent(25)).unwrap();

    let sim = mcu_state.lock().unwrap();
    assert_eq!(
        sim.state().transactions,
        vec![vec![25u8]],
        "expected a single-byte transaction; a two-byte one is a register write"
    );
    assert_eq!(
        sim.state().fan_percent,
        25,
        "fan is at {}%, not the 25% requested -- this is the register-write hazard",
        sim.state().fan_percent
    );
}

#[test]
fn the_simulator_really_does_reproduce_the_hazard() {
    // Guards the guard. If the simulator did not model the hazard, the test above would pass
    // for the wrong reason and prove nothing. So drive the simulator directly with the
    // dangerous transaction and confirm it does the damaging thing.
    let mut sim = LegacyMcu::new();
    sim.receive(&[0x80, 25]); // what a register write puts on the wire
    assert_eq!(
        sim.state().fan_percent,
        100,
        "the simulator should have read 0x80 as a duty of 128 and clamped to 100"
    );
}

#[test]
fn the_default_dialect_is_the_safe_one() {
    assert_eq!(Dialect::default(), Dialect::Legacy);
}

#[test]
fn every_documented_duty_reaches_the_mcu_unchanged() {
    for percent in 1..=100u8 {
        let bus = SimBus::new();
        let state = Arc::clone(&bus.mcu);
        let mut mcu = Mcu::new(bus, Dialect::default());
        mcu.set_fan(FanDuty::Percent(percent)).unwrap();
        assert_eq!(state.lock().unwrap().state().fan_percent, percent);
    }
}

#[test]
fn stopping_the_fan_uses_the_documented_off_opcode() {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mut mcu = Mcu::new(bus, Dialect::default());
    mcu.set_fan(FanDuty::Off).unwrap();
    assert_eq!(
        state.lock().unwrap().state().transactions,
        vec![vec![opcode::FAN_OFF]]
    );
    assert_eq!(state.lock().unwrap().state().fan_percent, 0);
}

#[test]
fn a_read_only_transport_blocks_every_write() {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mut mcu = Mcu::new(ReadOnly(bus), Dialect::default());

    assert!(matches!(
        mcu.set_fan(FanDuty::Percent(50)),
        Err(Error::WriteBlocked { .. })
    ));
    assert!(matches!(
        mcu.set_power_mode(PowerMode::AlwaysOn),
        Err(Error::WriteBlocked { .. })
    ));
    assert!(matches!(
        mcu.arm_power_cut(),
        Err(Error::WriteBlocked { .. })
    ));

    assert!(
        state.lock().unwrap().state().transactions.is_empty(),
        "a read-only transport let a write through to the device"
    );
}

#[test]
fn a_read_only_transport_still_allows_probing() {
    // Read-only must not mean blind: presence detection uses a quick-write that transfers no
    // data byte, and refusing it would make the safe default useless for discovery.
    let mut mcu = Mcu::new(ReadOnly(SimBus::new()), Dialect::default());
    assert!(
        mcu.is_present().unwrap(),
        "probing must still work on a read-only transport"
    );
}

#[test]
fn dry_run_records_the_exact_bytes_and_sends_none() {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mut mcu = Mcu::new(DryRun::new(bus), Dialect::default());

    mcu.set_fan(FanDuty::Percent(40)).unwrap();
    mcu.set_power_mode(PowerMode::AlwaysOn).unwrap();

    assert_eq!(
        mcu.bus().writes(),
        &[(ADDR, vec![40u8]), (ADDR, vec![0xFEu8])],
        "dry run did not record the transactions it suppressed"
    );
    assert!(
        state.lock().unwrap().state().transactions.is_empty(),
        "dry run reached the device"
    );
}

#[test]
fn the_bootloader_opcode_is_never_emitted() {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mut mcu = Mcu::new(bus, Dialect::default());

    assert!(
        mcu.enter_bootloader().is_err(),
        "bootloader entry must always be refused"
    );

    let sim = state.lock().unwrap();
    assert!(!sim.state().in_bootloader, "the MCU entered its bootloader");
    assert!(
        !sim.state()
            .transactions
            .iter()
            .any(|t| t.contains(&opcode::BOOTLOADER)),
        "0xBB reached the bus"
    );
}

#[test]
fn rate_limiting_holds_back_a_burst_and_says_so_rather_than_claiming_success() {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let limited = RateLimited::new(bus, Duration::from_secs(60));
    let mut mcu = Mcu::new(limited, Dialect::default());

    let mut held = 0;
    for p in 1..=20u8 {
        match mcu.set_fan(FanDuty::Percent(p)) {
            Ok(()) => {}
            Err(argon_hal::Error::RateLimited { .. }) => held += 1,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    assert_eq!(
        state.lock().unwrap().state().transactions.len(),
        1,
        "rate limiter let a burst through"
    );
    // The point of the change: the 19 that did not happen are reported as not having
    // happened. Returning Ok for them let a caller record a duty the device never got.
    assert_eq!(held, 19, "a held write was reported as a success");
    assert_eq!(mcu.bus().suppressed(), 19);
}

#[test]
fn shadow_state_tracks_what_was_written() {
    // The legacy protocol cannot report its duty, and reading it in the register protocol is
    // the hazardous transaction. We are the only writer, so what we last sent is what it is.
    let mut mcu = Mcu::new(SimBus::new(), Dialect::default());
    assert_eq!(mcu.duty(), None);
    mcu.set_fan(FanDuty::Percent(55)).unwrap();
    assert_eq!(mcu.duty(), Some(FanDuty::Percent(55)));
}

#[test]
fn a_blocked_write_does_not_update_shadow_state() {
    // Believing we had set a duty that never reached the device would be worse than not
    // knowing: the fan safety logic reads this.
    let mut mcu = Mcu::new(ReadOnly(SimBus::new()), Dialect::default());
    let _ = mcu.set_fan(FanDuty::Percent(55));
    assert_eq!(
        mcu.duty(),
        None,
        "shadow state recorded a write that was refused"
    );
}
