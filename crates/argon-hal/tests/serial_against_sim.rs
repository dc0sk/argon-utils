// SPDX-License-Identifier: GPL-3.0-or-later
//! The real serial transport against a simulated UPS, over a real PTY.
//!
//! A PTY rather than a mock, deliberately. This runs the actual `serialport` code path —
//! partial reads, read timeouts, frames split across read boundaries — which is where serial
//! code goes wrong. A mock handing over whole frames would pass while the transport was
//! broken in precisely those ways.
//!
//! Needs no Raspberry Pi and no Argon hardware, so it runs in CI.
//!
//! # These assertions were verified to be load-bearing
//!
//! The two deadline tests were checked by sabotage on 2026-09-15: making the deadline fire
//! ten seconds late caused exactly those two to fail, and the suite's runtime went from
//! 0.9 s to 10.3 s. `a_slow_but_complete_response_still_arrives` correctly kept passing,
//! since it asserts the opposite property. A timeout test that has never been seen to fail
//! is indistinguishable from one that cannot.

use argon_hal::serial::SerialLink;
use argon_proto::ups::{BatteryStatus, Command, PowerSource, UpsTime};
use argon_sim::ups::{Faults, UpsSim, UpsState};
use serialport::SerialPort;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Runs a simulator on one end of a PTY pair and hands the caller a link to the other.
fn with_sim<F>(state: UpsState, faults: Faults, body: F)
where
    F: FnOnce(&mut SerialLink) + Send + 'static,
{
    let (master, slave) = serialport::TTYPort::pair().expect("PTY pair");
    let slave_name = slave.name().expect("slave path");

    let stop = Arc::new(AtomicBool::new(false));
    let sim_stop = Arc::clone(&stop);
    let sim = std::thread::spawn(move || {
        let mut master = master;
        let mut sim = UpsSim::with_faults(state, faults);
        let _ = sim.serve_until(
            &mut master,
            Instant::now() + Duration::from_secs(10),
            &sim_stop,
        );
    });

    // Keep the slave handle open: closing every handle would tear the PTY down before the
    // link opens it by name.
    let _slave = slave;
    let mut link = SerialLink::open(&slave_name).expect("open the simulated port");
    body(&mut link);

    stop.store(true, Ordering::Relaxed);
    drop(link);
    let _ = sim.join();
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(3)
}

#[test]
fn reads_battery_status() {
    with_sim(UpsState::default(), Faults::default(), |link| {
        let frame = link
            .request(Command::BatteryStatus.as_byte(), &[], deadline())
            .unwrap();
        let status = BatteryStatus::decode(frame.payload()).unwrap();
        assert_eq!(status.percent, 93);
        assert_eq!(status.source, PowerSource::Mains);
    });
}

#[test]
fn reads_firmware_version() {
    with_sim(UpsState::default(), Faults::default(), |link| {
        let frame = link
            .request(Command::FirmwareVersion.as_byte(), &[], deadline())
            .unwrap();
        assert_eq!(frame.payload(), &[113]);
    });
}

#[test]
fn reads_the_clock() {
    with_sim(UpsState::default(), Faults::default(), |link| {
        let frame = link
            .request(Command::GetRtc.as_byte(), &[], deadline())
            .unwrap();
        let t = UpsTime::decode_clock(frame.payload()).unwrap();
        assert_eq!((t.year, t.month, t.day), (2026, 9, 15));
        assert!(t.is_plausible());
    });
}

#[test]
fn on_battery_is_reported_as_such() {
    let state = UpsState {
        percent: 42,
        charging_byte: 1,
        ..UpsState::default()
    };
    with_sim(state, Faults::default(), |link| {
        let frame = link
            .request(Command::BatteryStatus.as_byte(), &[], deadline())
            .unwrap();
        let status = BatteryStatus::decode(frame.payload()).unwrap();
        assert_eq!(status.percent, 42);
        assert_eq!(status.source, PowerSource::Battery);
    });
}

#[test]
fn recovers_from_junk_on_the_line() {
    // A desynchronised link is the normal failure mode here, not an exotic one.
    let faults = Faults {
        junk_prefix: 17,
        ..Faults::default()
    };
    with_sim(UpsState::default(), faults, |link| {
        let frame = link
            .request(Command::FirmwareVersion.as_byte(), &[], deadline())
            .unwrap();
        assert_eq!(frame.payload(), &[113]);
    });
}

#[test]
fn survives_a_corrupt_response_and_succeeds_on_retry() {
    // Every response is corrupted, so the first request must fail rather than return a frame
    // assembled from bad bytes.
    let faults = Faults {
        corrupt_every: 1,
        ..Faults::default()
    };
    with_sim(UpsState::default(), faults, |link| {
        let short = Instant::now() + Duration::from_millis(600);
        let result = link.request(Command::FirmwareVersion.as_byte(), &[], short);
        assert!(
            result.is_err(),
            "a corrupt frame was accepted as a response"
        );
    });
}

#[test]
fn a_silent_device_hits_the_deadline_instead_of_hanging() {
    // The failure this guards against: a per-read timeout that never becomes an operation
    // deadline, leaving a caller waiting indefinitely on a device that says nothing.
    let faults = Faults {
        silent: true,
        ..Faults::default()
    };
    with_sim(UpsState::default(), faults, |link| {
        let started = Instant::now();
        let result = link.request(
            Command::BatteryStatus.as_byte(),
            &[],
            Instant::now() + Duration::from_millis(500),
        );
        let elapsed = started.elapsed();
        assert!(result.is_err(), "expected a timeout");
        assert!(
            elapsed < Duration::from_secs(2),
            "took {elapsed:?}, deadline was 500ms"
        );
    });
}

#[test]
fn a_dribbling_device_is_still_bounded_by_the_deadline() {
    // One byte per 150ms. A per-read timeout of 100ms would never fire on a whole frame, so
    // only a real operation deadline bounds this.
    let faults = Faults {
        dribble: Some(Duration::from_millis(150)),
        ..Faults::default()
    };
    with_sim(UpsState::default(), faults, |link| {
        let started = Instant::now();
        let result = link.request(
            Command::BatteryStatus.as_byte(),
            &[],
            Instant::now() + Duration::from_millis(400),
        );
        let elapsed = started.elapsed();
        assert!(result.is_err(), "expected the deadline to fire");
        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    });
}

#[test]
fn a_slow_but_complete_response_still_arrives() {
    // The mirror of the previous test: slow is not the same as broken, and a generous
    // deadline must let a dribbled frame through.
    let faults = Faults {
        dribble: Some(Duration::from_millis(20)),
        ..Faults::default()
    };
    with_sim(UpsState::default(), faults, |link| {
        let frame = link
            .request(
                Command::FirmwareVersion.as_byte(),
                &[],
                Instant::now() + Duration::from_secs(3),
            )
            .expect("a slow response should still be read");
        assert_eq!(frame.payload(), &[113]);
    });
}

#[test]
fn setting_the_clock_round_trips_through_the_device() {
    with_sim(UpsState::default(), Faults::default(), |link| {
        let want = UpsTime {
            year: 2027,
            month: 3,
            day: 4,
            hour: 5,
            minute: 6,
            second: Some(7),
        };
        link.request(
            Command::SetRtc.as_byte(),
            &want.encode_clock().unwrap(),
            deadline(),
        )
        .expect("set clock");
        let frame = link
            .request(Command::GetRtc.as_byte(), &[], deadline())
            .unwrap();
        assert_eq!(UpsTime::decode_clock(frame.payload()).unwrap(), want);
    });
}
