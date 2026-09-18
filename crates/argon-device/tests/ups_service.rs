// SPDX-License-Identifier: GPL-3.0-or-later
//! The whole chain: battery readings -> policy -> scheduled poweroff -> status file -> notice.
//! Scripted UPS, fake logind. Nothing real is scheduled.

use argon_device::power::{Action, PowerControl, ShutdownCoordinator};
use argon_device::status::{UpsStatus, Urgency, Watcher};
use argon_device::ups::{QueryOnly, Ups, UpsLink, UpsMonitor};
use argon_device::ups_service::{contention, step};
use argon_hal::{Error, Result};
use argon_proto::ups::policy::{BatteryPolicy, PolicyConfig};
use argon_proto::ups::{Command, Frame};
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const UP: Duration = Duration::from_secs(3600);

#[derive(Default)]
struct Scripted(VecDeque<(u8, u8)>);

impl UpsLink for Scripted {
    fn request(&mut self, cmd: Command, _p: &[u8], _d: Instant) -> Result<Frame> {
        let (pct, charging) = self.0.pop_front().ok_or(Error::Timeout)?;
        Ok(Frame::new(cmd.as_byte(), &[pct, charging]).unwrap())
    }
}

#[derive(Default)]
struct FakeLogind {
    pending: Option<SystemTime>,
    schedules: u32,
    cancels: u32,
}

impl PowerControl for FakeLogind {
    fn schedule_poweroff(&mut self, delay: Duration, _m: &str) -> Result<SystemTime> {
        self.schedules += 1;
        let at = UNIX_EPOCH + Duration::from_secs(2_000_000_000) + delay;
        self.pending = Some(at);
        Ok(at)
    }
    fn cancel(&mut self) -> Result<()> {
        self.cancels += 1;
        self.pending = None;
        Ok(())
    }
    fn pending(&mut self) -> Result<Option<SystemTime>> {
        Ok(self.pending)
    }
    fn wall_message(&mut self) -> Result<Option<String>> {
        Ok(self
            .pending
            .map(|_| argon_device::power::SHUTDOWN_MESSAGE.to_owned()))
    }
}

/// (percent, charging byte): 0 = mains, 1 = battery, as on the wire.
fn run(readings: &[(u8, u8)], dry_run: bool) -> (Vec<(Action, UpsStatus)>, FakeLogind) {
    let mut monitor = UpsMonitor::new(
        Ups::new(QueryOnly(Scripted(readings.iter().copied().collect()))),
        BatteryPolicy::new(PolicyConfig::default()).unwrap(),
    );
    let mut coord =
        ShutdownCoordinator::new(FakeLogind::default(), Duration::from_secs(120), dry_run);
    let mut out = Vec::new();
    for i in 0..readings.len() {
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000 + i as u64 * 10);
        let c = step(&mut monitor, &mut coord, UP, now);
        out.push((c.action, c.status));
    }
    let power = std::mem::take(coord.power_mut());
    (out, power)
}

#[test]
fn drain_schedules_once_and_mains_cancels() {
    // Two mains readings at the end, because a recovery is confirmed the same way a critical
    // battery is: one mains reading no longer gives up a confirmed critical state. A supply
    // flapping faster than the poll interval would otherwise reset the confirmation streak on
    // every blip and the battery would empty with nothing ever scheduled.
    let (cycles, power) = run(
        &[(40, 1), (15, 1), (9, 1), (8, 1), (8, 1), (8, 0), (8, 0)],
        false,
    );
    let actions: Vec<&Action> = cycles.iter().map(|(a, _)| a).collect();
    assert!(
        matches!(actions[3], Action::Scheduled { .. }),
        "{actions:?}"
    );
    assert_eq!(
        actions[4],
        &Action::None,
        "rescheduled on a repeat of the same advice"
    );
    assert_eq!(
        actions[5],
        &Action::None,
        "gave up a confirmed critical state on a single mains reading: {actions:?}"
    );
    assert_eq!(actions[6], &Action::Cancelled, "{actions:?}");
    assert_eq!((power.schedules, power.cancels), (1, 1));

    // The published status carries the shutdown time while it is pending, and clears it after.
    assert!(cycles[3].1.shutdown_at.is_some());
    assert_eq!(cycles[3].1.level, "critical");
    assert!(cycles[6].1.shutdown_at.is_none());
    assert_eq!(cycles[6].1.level, "on-mains");
}

#[test]
fn the_desktop_agent_sees_the_countdown_and_the_cancellation() {
    // Status files as the daemon would publish them, fed to the agent's watcher.
    let (cycles, _) = run(&[(40, 1), (15, 1), (9, 1), (8, 1), (8, 0), (8, 0)], false);
    let mut w = Watcher::new();
    let fmt = |_: SystemTime| "HH:MM".to_owned();
    let notices: Vec<_> = cycles
        .iter()
        .map(|(_, s)| {
            // Round-trip through the file format, as the agent would read it.
            let parsed = UpsStatus::parse(&s.to_text()).unwrap();
            w.observe(Some(parsed.clone()), parsed.updated, &fmt)
        })
        .collect();

    let critical = notices
        .iter()
        .flatten()
        .find(|n| n.urgency == Urgency::Critical)
        .expect("no critical notice for the scheduled shutdown");
    assert!(
        critical.text.contains("powering off at HH:MM"),
        "{}",
        critical.text
    );
    assert!(
        notices
            .iter()
            .flatten()
            .any(|n| n.text.contains("shutdown cancelled")),
        "no cancellation notice: {notices:?}"
    );
}

#[test]
fn dry_run_publishes_state_but_schedules_nothing() {
    let (cycles, power) = run(&[(9, 1), (8, 1), (8, 0), (8, 0)], true);
    assert_eq!(cycles[1].0, Action::WouldSchedule);
    assert_eq!(cycles[3].0, Action::WouldCancel);
    assert_eq!((power.schedules, power.cancels), (0, 0));

    // And it publishes NO shutdown time: that field is what the desktop agent turns into a
    // critical "powering off at ..." notice, and in dry run nothing is scheduled to announce.
    assert!(
        cycles.iter().all(|(_, s)| s.shutdown_at.is_none()),
        "dry run published a shutdown time: {:?}",
        cycles
            .iter()
            .map(|(_, s)| s.shutdown_at)
            .collect::<Vec<_>>()
    );
}

#[test]
fn a_lost_ups_publishes_unknown_and_never_schedules() {
    // One critical reading, then the UPS stops answering. A second critical reading would have
    // confirmed critical and scheduled a poweroff; a failed read must not stand in for it.
    let mut monitor = UpsMonitor::new(
        Ups::new(QueryOnly(Scripted([(8, 1)].into_iter().collect()))),
        BatteryPolicy::new(PolicyConfig::default()).unwrap(),
    );
    let mut coord =
        ShutdownCoordinator::new(FakeLogind::default(), Duration::from_secs(120), false);

    let first = step(&mut monitor, &mut coord, UP, UNIX_EPOCH);
    assert_eq!(
        first.status.level, "low",
        "one reading must not confirm critical"
    );

    let second = step(
        &mut monitor,
        &mut coord,
        UP,
        UNIX_EPOCH + Duration::from_secs(10),
    );
    assert!(
        second.poll.error.is_some(),
        "the scripted UPS should have stopped answering"
    );
    assert_eq!(second.status.level, "unknown");
    assert_eq!(second.status.percent, None);
    assert_eq!(second.action, Action::None);
    assert_eq!(
        coord.power_mut().schedules,
        0,
        "scheduled a poweroff with the UPS silent"
    );
}

#[test]
fn only_the_vendor_ups_units_count_as_contention() {
    // On a ONE V5 argononed drives no hardware; it must not block UPS actions.
    let active = vec![
        "argononed.service".to_owned(),
        "argonupsrtcd.service".to_owned(),
        "argononeupsd.service".to_owned(),
    ];
    assert_eq!(
        contention(&active),
        vec!["argonupsrtcd.service", "argononeupsd.service"]
    );
    assert!(contention(&["argononed.service".to_owned()]).is_empty());
}

/// A ONE UP fuel gauge playing back `(percent, current)` readings, one per poll.
struct ScriptedGauge {
    script: VecDeque<(u8, i16)>,
    now: (u8, i16),
}

impl argon_hal::i2c::RegisterRead for ScriptedGauge {
    fn read_registers(&mut self, register: u8, buf: &mut [u8]) -> Result<()> {
        use argon_proto::cw2217::reg;
        match register {
            reg::VERSION => buf[0] = argon_proto::cw2217::VERSION_VALUE,
            // Each poll reads SOC first: that is where the script advances.
            reg::SOC => {
                self.now = self.script.pop_front().ok_or(Error::Timeout)?;
                buf.copy_from_slice(&[self.now.0, 0]);
            }
            reg::VCELL => buf.copy_from_slice(&[0x30, 0x00]),
            reg::CURRENT => buf.copy_from_slice(&self.now.1.to_be_bytes()),
            _ => buf.fill(0),
        }
        Ok(())
    }
    fn describe(&self) -> String {
        "scripted gauge".into()
    }
}

#[test]
fn a_one_up_draining_on_battery_is_powered_off_and_plugging_in_cancels_it() {
    use argon_device::gauge::{Cw2217, GaugeMonitor};
    // Discharging all the way down, then the charger: charging current twice confirms it.
    let script = [
        (40, -2200),
        (15, -2200),
        (9, -2200),
        (8, -2200),
        (8, 2800),
        (8, 2800),
    ];
    let gauge = Cw2217::identify(ScriptedGauge {
        script: script.into_iter().collect(),
        now: (0, 0),
    })
    .unwrap();
    let mut monitor =
        GaugeMonitor::new(gauge, BatteryPolicy::new(PolicyConfig::default()).unwrap());
    let mut coord =
        ShutdownCoordinator::new(FakeLogind::default(), Duration::from_secs(120), false);
    let mut actions = Vec::new();
    let mut levels = Vec::new();
    for i in 0..script.len() {
        let now = UNIX_EPOCH + Duration::from_secs(2_000_000_000 + i as u64 * 10);
        let c = step(&mut monitor, &mut coord, UP, now);
        actions.push(c.action);
        levels.push(c.status.level);
    }
    assert!(
        matches!(actions[3], Action::Scheduled { .. }),
        "{actions:?}"
    );
    assert_eq!(levels[3], "critical");
    assert_eq!(actions[5], Action::Cancelled, "{actions:?}");
    assert_eq!(levels[5], "on-mains");
    let power = std::mem::take(coord.power_mut());
    assert_eq!((power.schedules, power.cancels), (1, 1));
}
