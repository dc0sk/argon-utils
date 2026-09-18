// SPDX-License-Identifier: GPL-3.0-or-later
//! The UPS driver, the query-only guard, and the monitor.

use argon_device::ups::{QueryOnly, Ups, UpsLink, UpsMonitor};
use argon_hal::serial::SerialLink;
use argon_hal::{Error, Result};
use argon_proto::ups::policy::{Advice, BatteryPolicy, Level, PolicyConfig};
use argon_proto::ups::{Command, Frame, PowerSource};
use argon_sim::ups::{Faults, UpsSim, UpsState};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const UP: Duration = Duration::from_secs(3600);

/// A link that replays scripted replies and records what was asked.
#[derive(Default)]
struct Scripted {
    replies: VecDeque<Result<Frame>>,
    sent: Vec<Command>,
}

impl Scripted {
    fn reply(mut self, cmd: Command, payload: &[u8]) -> Self {
        self.replies
            .push_back(Ok(Frame::new(cmd.as_byte(), payload).unwrap()));
        self
    }
    fn fail(mut self) -> Self {
        self.replies.push_back(Err(Error::Timeout));
        self
    }
}

impl UpsLink for Scripted {
    fn request(&mut self, cmd: Command, _payload: &[u8], _deadline: Instant) -> Result<Frame> {
        self.sent.push(cmd);
        self.replies.pop_front().unwrap_or(Err(Error::Timeout))
    }
}

#[test]
fn query_only_passes_exactly_the_confirmed_queries() {
    for cmd in [
        Command::BatteryStatus,
        Command::ChargeCurrent,
        Command::FirmwareVersion,
        Command::GetRtc,
        Command::GetWake,
    ] {
        assert!(QueryOnly::is_query(cmd), "{cmd:?} should be allowed");
    }
    for cmd in [
        Command::SetRtc,
        Command::SetWake,
        Command::ResetMeter,
        Command::Acknowledge,
    ] {
        assert!(!QueryOnly::is_query(cmd), "{cmd:?} must not be allowed");
    }
}

#[test]
fn a_refused_command_never_reaches_the_device() {
    let mut link = QueryOnly(Scripted::default());
    for cmd in [
        Command::SetRtc,
        Command::SetWake,
        Command::ResetMeter,
        Command::Acknowledge,
    ] {
        let r = link.request(cmd, &[], Instant::now());
        assert!(
            matches!(r, Err(Error::WriteBlocked { .. })),
            "{cmd:?}: {r:?}"
        );
    }
    assert!(
        link.0.sent.is_empty(),
        "refused commands reached the link: {:?}",
        link.0.sent
    );
}

#[test]
fn the_driver_decodes_the_bytes_captured_from_hardware() {
    // Payloads from crates/argon-proto/tests/tapes/ups-reads.json.
    let link = Scripted::default()
        .reply(Command::BatteryStatus, &[0x5B, 0x00])
        .reply(Command::FirmwareVersion, &[0x71])
        .reply(Command::GetRtc, &[0x26, 0x09, 0x17, 0x13, 0x29, 0x15])
        .reply(Command::GetWake, &[])
        .reply(Command::ChargeCurrent, &[0x03, 0x52]);
    let mut ups = Ups::new(QueryOnly(link));

    let b = ups.battery().unwrap();
    assert_eq!((b.percent, b.source), (91, PowerSource::Mains));
    assert_eq!(ups.firmware().unwrap(), 113);
    let t = ups.clock().unwrap();
    assert_eq!(
        (t.year, t.month, t.day, t.hour, t.minute, t.second),
        (2026, 9, 17, 13, 29, Some(15))
    );
    assert_eq!(
        ups.wake().unwrap(),
        None,
        "an empty payload means no schedule, not an error"
    );
    assert_eq!(ups.charge_current_raw().unwrap(), 850);
}

#[test]
fn an_unexpected_payload_is_an_error_not_a_guess() {
    let mut ups = Ups::new(Scripted::default().reply(Command::FirmwareVersion, &[1, 2]));
    assert!(ups.firmware().is_err());
}

fn monitor(link: Scripted) -> UpsMonitor<QueryOnly<Scripted>> {
    UpsMonitor::new(
        Ups::new(QueryOnly(link)),
        BatteryPolicy::new(PolicyConfig::default()).unwrap(),
    )
}

#[test]
fn a_draining_battery_ends_in_shutdown_advice() {
    let mut m = monitor(
        Scripted::default()
            .reply(Command::BatteryStatus, &[60, 1])
            .reply(Command::BatteryStatus, &[18, 1])
            .reply(Command::BatteryStatus, &[9, 1])
            .reply(Command::BatteryStatus, &[8, 1]),
    );
    let levels: Vec<(Level, Advice)> = (0..4)
        .map(|_| m.poll(UP))
        .map(|p| (p.decision.level, p.decision.advice))
        .collect();
    assert_eq!(
        levels,
        vec![
            (Level::OnBattery, Advice::None),
            (Level::Low, Advice::None),
            (Level::Low, Advice::None),
            (Level::Critical, Advice::Shutdown),
        ]
    );
}

#[test]
fn a_lost_link_is_reported_and_never_advises_shutdown() {
    let mut m = monitor(
        Scripted::default()
            .reply(Command::BatteryStatus, &[8, 1])
            .reply(Command::BatteryStatus, &[8, 1])
            .fail()
            .fail(),
    );
    m.poll(UP);
    assert_eq!(m.poll(UP).decision.advice, Advice::Shutdown);

    let p = m.poll(UP);
    assert!(p.error.is_some());
    assert_eq!(p.decision.level, Level::Unknown);
    assert_eq!(
        p.decision.advice,
        Advice::None,
        "advised shutdown with the link down"
    );
    assert_eq!(p.consecutive_failures, 1);
    assert_eq!(m.poll(UP).consecutive_failures, 2);
}

#[test]
fn a_recovered_link_clears_the_failure_count() {
    let mut m = monitor(
        Scripted::default()
            .fail()
            .fail()
            .reply(Command::BatteryStatus, &[70, 0]),
    );
    m.poll(UP);
    assert_eq!(m.poll(UP).consecutive_failures, 2);
    let p = m.poll(UP);
    assert_eq!(p.consecutive_failures, 0);
    assert_eq!(p.decision.level, Level::OnMains);
}

/// The real serial transport, through the guard, against the simulator over a real PTY.
#[test]
fn end_to_end_over_a_pty() {
    use serialport::SerialPort;

    let (master, slave) = serialport::TTYPort::pair().expect("PTY pair");
    let name = slave.name().expect("slave path");
    let stop = Arc::new(AtomicBool::new(false));
    let sim_stop = Arc::clone(&stop);
    let state = UpsState {
        percent: 42,
        charging_byte: 1,
        ..UpsState::default()
    };
    let sim = std::thread::spawn(move || {
        let mut master = master;
        let mut sim = UpsSim::with_faults(state, Faults::default());
        let _ = sim.serve_until(
            &mut master,
            Instant::now() + Duration::from_secs(10),
            &sim_stop,
        );
    });

    let _slave = slave;
    let link = SerialLink::open(&name).expect("open simulated port");
    let mut m = UpsMonitor::new(
        Ups::new(QueryOnly(link)),
        BatteryPolicy::new(PolicyConfig::default()).unwrap(),
    );

    let p = m.poll(UP);
    let b = p.battery.expect("a battery reading over the PTY");
    assert_eq!((b.percent, b.source), (42, PowerSource::Battery));
    assert_eq!(p.decision.level, Level::OnBattery);
    assert_eq!(m.ups_mut().firmware().unwrap(), 113);
    assert_eq!(m.ups_mut().wake().unwrap(), None);

    stop.store(true, Ordering::Relaxed);
    drop(m);
    let _ = sim.join();
}

mod gate {
    //! The link argond uses: queries, plus setting the clock when allowed, and nothing else.

    use super::Scripted;
    use argon_device::ups::{Gate, Ups, UpsLink};
    use argon_proto::ups::{Command, UpsTime};
    use std::time::{Duration, Instant};

    /// Every command the protocol has, so a new variant cannot slip past these tests: adding
    /// one to `Command` without deciding its gate breaks the exhaustive match below.
    const ALL: [Command; 9] = [
        Command::BatteryStatus,
        Command::ChargeCurrent,
        Command::SetRtc,
        Command::FirmwareVersion,
        Command::GetRtc,
        Command::SetWake,
        Command::GetWake,
        Command::Acknowledge,
        Command::ResetMeter,
    ];

    const fn expected_with_clock_writes(cmd: Command) -> bool {
        match cmd {
            Command::BatteryStatus
            | Command::ChargeCurrent
            | Command::FirmwareVersion
            | Command::GetRtc
            | Command::GetWake
            | Command::SetRtc => true,
            // Still only `inferred`: never through, however the gate is built.
            Command::SetWake | Command::Acknowledge | Command::ResetMeter => false,
        }
    }

    #[test]
    fn with_clock_writes_admits_exactly_the_queries_and_the_clock() {
        let g = Gate::with_clock_writes(());
        for cmd in ALL {
            assert_eq!(g.allows(cmd), expected_with_clock_writes(cmd), "{cmd:?}");
        }
    }

    #[test]
    fn queries_only_refuses_the_clock_too() {
        let g = Gate::queries_only(());
        assert!(!g.allows(Command::SetRtc));
        for cmd in ALL {
            if cmd != Command::SetRtc {
                assert_eq!(g.allows(cmd), expected_with_clock_writes(cmd), "{cmd:?}");
            }
        }
    }

    #[test]
    fn a_refused_write_never_reaches_the_device() {
        // Asserting on what the device RECEIVED, not only on the error: the scripted link
        // returns an error when it has no reply queued, so an error alone would also be what a
        // write that got through looks like.
        let mut g = Gate::with_clock_writes(Scripted::default());
        for cmd in [Command::SetWake, Command::ResetMeter, Command::Acknowledge] {
            assert!(
                g.request(cmd, &[], Instant::now() + Duration::from_secs(1))
                    .is_err()
            );
        }
        assert!(
            g.inner().sent.is_empty(),
            "reached the device: {:?}",
            g.inner().sent
        );
        // And the positive control: a permitted command does reach it.
        let _ = g.request(
            Command::SetRtc,
            &[],
            Instant::now() + Duration::from_secs(1),
        );
        assert_eq!(g.inner().sent, vec![Command::SetRtc]);

        let mut ups = Ups::new(Gate::queries_only(Scripted::default()));
        let t = UpsTime::from_unix_seconds(1_789_728_000).unwrap();
        assert!(
            ups.set_clock(t).is_err(),
            "a queries-only gate let a clock set through"
        );
    }

    #[test]
    fn a_set_expects_the_observed_empty_reply() {
        // ARGON-UPS-CMD3-REPLY: FE 00 03 01. Anything else was not observed and is reported.
        let t = UpsTime::from_unix_seconds(1_789_728_000).unwrap();
        let ok = Scripted::default().reply(Command::SetRtc, &[]);
        assert!(Ups::new(Gate::with_clock_writes(ok)).set_clock(t).is_ok());
        let odd = Scripted::default().reply(Command::SetRtc, &[0x01]);
        assert!(Ups::new(Gate::with_clock_writes(odd)).set_clock(t).is_err());
    }
}
