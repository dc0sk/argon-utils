// SPDX-License-Identifier: GPL-3.0-or-later
//! The fan control loop, driven by a scripted temperature series.

use argon_device::fan::{FanTask, Step};
use argon_device::mcu::{ADDR, Dialect, Mcu};
use argon_hal::Result;
use argon_hal::i2c::{I2cBus, ReadOnly};
use argon_hal::thermal::ScriptedTemperature;
use argon_proto::fan::{CurvePoint, FanController, FanCurve, FanDuty};
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

fn curve() -> FanCurve {
    FanCurve::new(vec![
        CurvePoint::from_celsius(55, FanDuty::Percent(30)),
        CurvePoint::from_celsius(60, FanDuty::Percent(55)),
        CurvePoint::from_celsius(65, FanDuty::Percent(100)),
    ])
    .unwrap()
}

fn rig(
    temps: Vec<i32>,
    allow_stop: bool,
) -> (FanTask<SimBus, ScriptedTemperature>, Arc<Mutex<LegacyMcu>>) {
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mcu = Arc::new(Mutex::new(Mcu::new(bus, Dialect::default())));
    let controller = FanController::new(curve(), 3, 10, allow_stop);
    let task = FanTask::new(
        mcu,
        ScriptedTemperature::new(temps),
        controller,
        FanDuty::clamped(55),
    );
    (task, state)
}

fn fan(state: &Arc<Mutex<LegacyMcu>>) -> u8 {
    state.lock().unwrap().state().fan_percent
}

fn writes(state: &Arc<Mutex<LegacyMcu>>) -> usize {
    state.lock().unwrap().state().transactions.len()
}

#[test]
fn the_fan_follows_the_curve() {
    let (mut task, state) = rig(vec![400, 560, 610, 660], true);

    assert!(matches!(
        task.step().unwrap(),
        Step::Applied {
            duty: FanDuty::Off,
            ..
        }
    ));
    assert_eq!(fan(&state), 0);

    task.step().unwrap();
    assert_eq!(fan(&state), 30);

    task.step().unwrap();
    assert_eq!(fan(&state), 55);

    task.step().unwrap();
    assert_eq!(fan(&state), 100);
}

#[test]
fn an_unchanged_duty_is_not_rewritten() {
    // The rate limiter would drop a redundant write anyway, but it would also count it as
    // suppressed -- which would make that metric measure our own chattiness rather than real
    // curve activity.
    let (mut task, state) = rig(vec![610, 611, 612, 613, 614], true);
    for _ in 0..5 {
        task.step().unwrap();
    }
    assert_eq!(fan(&state), 55);
    assert_eq!(
        writes(&state),
        1,
        "wrote {} times for one duty",
        writes(&state)
    );
}

#[test]
fn a_reported_write_flag_matches_what_reached_the_device() {
    let (mut task, state) = rig(vec![610, 611, 660], true);

    let before = writes(&state);
    assert!(matches!(
        task.step().unwrap(),
        Step::Applied { wrote: true, .. }
    ));
    assert_eq!(writes(&state), before + 1);

    assert!(matches!(
        task.step().unwrap(),
        Step::Applied { wrote: false, .. }
    ));
    assert_eq!(
        writes(&state),
        before + 1,
        "reported no write but wrote anyway"
    );

    assert!(matches!(
        task.step().unwrap(),
        Step::Applied { wrote: true, .. }
    ));
    assert_eq!(writes(&state), before + 2);
}

#[test]
fn a_sensor_failure_forces_a_safe_duty_rather_than_coasting() {
    // A control loop that cannot see the temperature must not keep applying whatever it last
    // decided: it no longer has any basis for that decision, and the last decision may have
    // been "the machine is cool, stop the fan".
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mcu = Arc::new(Mutex::new(Mcu::new(bus, Dialect::default())));
    let mut task = FanTask::new(
        mcu,
        ScriptedTemperature::with_failures(vec![Some(400), None, None]),
        FanController::new(curve(), 3, 10, true),
        FanDuty::clamped(55),
    );

    task.step().unwrap();
    assert_eq!(fan(&state), 0, "cool machine should have stopped the fan");

    let step = task.step().unwrap();
    assert!(
        matches!(
            step,
            Step::SensorFailed {
                fallback: FanDuty::Percent(55),
                consecutive: 1
            }
        ),
        "got {step:?}"
    );
    assert_eq!(fan(&state), 55, "fan left stopped after the sensor failed");

    let step = task.step().unwrap();
    assert!(
        matches!(step, Step::SensorFailed { consecutive: 2, .. }),
        "got {step:?}"
    );
    assert_eq!(task.consecutive_failures(), 2);
}

#[test]
fn recovering_from_a_sensor_failure_does_not_carry_stale_hysteresis() {
    // After a gap in readings, hysteresis must not hold a duty chosen from a temperature we
    // can no longer vouch for.
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mcu = Arc::new(Mutex::new(Mcu::new(bus, Dialect::default())));
    let mut task = FanTask::new(
        mcu,
        ScriptedTemperature::with_failures(vec![Some(660), None, Some(400)]),
        FanController::new(curve(), 3, 10, true),
        FanDuty::clamped(55),
    );

    task.step().unwrap();
    assert_eq!(fan(&state), 100);

    task.step().unwrap(); // sensor fails
    assert_eq!(fan(&state), 55);

    // Now cool again. Without the reset, hysteresis from the 66C reading would hold a
    // higher duty even though the recorded state is the fallback.
    task.step().unwrap();
    assert_eq!(
        fan(&state),
        0,
        "stale hysteresis held the fan up after recovery"
    );
}

#[test]
fn a_recovered_sensor_clears_the_failure_count() {
    let mcu = Arc::new(Mutex::new(Mcu::new(SimBus::new(), Dialect::default())));
    let mut task = FanTask::new(
        mcu,
        ScriptedTemperature::with_failures(vec![None, None, Some(600)]),
        FanController::new(curve(), 3, 10, true),
        FanDuty::clamped(55),
    );
    task.step().unwrap();
    task.step().unwrap();
    assert_eq!(task.consecutive_failures(), 2);
    task.step().unwrap();
    assert_eq!(task.consecutive_failures(), 0);
    assert_eq!(task.last_temperature(), Some(600));
}

#[test]
fn with_stopping_disallowed_the_fan_never_stops() {
    let (mut task, state) = rig(vec![100, 200, 300, 400], false);
    for _ in 0..4 {
        task.step().unwrap();
        assert!(fan(&state) > 0, "fan stopped despite allow_stop = false");
    }
}

#[test]
fn a_blocked_write_is_surfaced_rather_than_swallowed() {
    // A failed device write means the daemon is no longer in control. The caller has to know:
    // silently continuing would leave the fan at whatever it was while the loop reports
    // healthy operation.
    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let mcu = Arc::new(Mutex::new(Mcu::new(ReadOnly(bus), Dialect::default())));
    let mut task = FanTask::new(
        mcu,
        ScriptedTemperature::new(vec![660]),
        FanController::new(curve(), 3, 10, true),
        FanDuty::clamped(55),
    );

    assert!(
        task.step().is_err(),
        "a refused write was reported as success"
    );
    assert_eq!(writes(&state), 0);
}

#[test]
fn a_long_run_never_produces_an_invalid_duty() {
    let temps: Vec<i32> = (0..500).map(|i| 300 + (i * 37) % 600).collect();
    let (mut task, state) = rig(temps, true);
    for _ in 0..500 {
        task.step().unwrap();
        assert!(fan(&state) <= 100, "fan duty {} exceeds 100", fan(&state));
    }
}

/// The bug found on a Pi 4 on 2026-09-22, as a test.
///
/// `argond` asserts a safe duty at startup, and the first curve step follows immediately
/// afterwards -- well inside the 500 ms write interval. The rate limiter used to return
/// `Ok(())` for that second write, so the task recorded the new duty as written and, writing
/// only on change, never sent it again. The fan ran at the startup duty for as long as the
/// curve stayed put: twelve minutes of flat temperature under a log line saying "off".
#[test]
fn a_held_back_write_is_retried_rather_than_believed() {
    use argon_hal::i2c::RateLimited;
    use std::time::Duration;

    let bus = SimBus::new();
    let state = Arc::clone(&bus.mcu);
    let interval = Duration::from_millis(30);
    let mcu = Arc::new(Mutex::new(Mcu::new(
        RateLimited::new(bus, interval),
        Dialect::default(),
    )));

    // What the daemon does first: assert a known duty. This consumes the write allowance.
    mcu.lock().unwrap().set_fan(FanDuty::Percent(55)).unwrap();

    let controller = FanController::new(curve(), 3, 10, true);
    let mut task = FanTask::new(
        Arc::clone(&mcu),
        ScriptedTemperature::new(vec![300, 300]),
        controller,
        FanDuty::Percent(55),
    );

    // Immediately after, the curve wants the fan off. The transport holds it back.
    let step = task
        .step()
        .expect("a held write is not an error to the caller");
    assert!(
        matches!(step, Step::Applied { wrote: false, .. }),
        "reported a write that never reached the device: {step:?}"
    );
    assert_eq!(
        state.lock().unwrap().state().fan_percent,
        55,
        "the device should still hold the startup duty"
    );

    // Once the interval has passed, the next poll must try again -- the whole point.
    std::thread::sleep(interval + Duration::from_millis(10));
    let step = task.step().expect("second step failed");
    assert!(
        matches!(step, Step::Applied { wrote: true, .. }),
        "the retry never happened: {step:?}"
    );
    assert_eq!(
        state.lock().unwrap().state().fan_percent,
        0,
        "the fan was left running while the curve said off"
    );
}
