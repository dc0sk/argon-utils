// SPDX-License-Identifier: GPL-3.0-or-later
//! Wiring the metrics exporter to the daemon's state.

use argon_device::fan_control::KernelFan;
use argon_hal::fan_hwmon::PwmFan;
use argon_hal::foreign;
use argon_hal::mode::Mode;
use argon_telemetry::{Server, Snapshot};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

/// What the control loop tells the exporter.
///
/// Only what the loop uniquely knows. Anything readable from the system — fan speed, vendor
/// units — is read at scrape time instead, so it cannot go stale between polls.
#[derive(Debug, Clone, Copy, Default)]
pub struct State {
    /// Last temperature read, in tenths of a degree.
    pub cpu_decicelsius: Option<i32>,
    /// Consecutive failed sensor reads.
    pub sensor_failures: u32,
}

/// Records one control iteration.
pub fn record(state: &Arc<Mutex<State>>, cpu_decicelsius: Option<i32>, failures: u32) {
    let mut s = state.lock().unwrap_or_else(PoisonError::into_inner);
    if cpu_decicelsius.is_some() {
        s.cpu_decicelsius = cpu_decicelsius;
    }
    s.sensor_failures = failures;
}

/// Starts the exporter on a background thread, returning the address it bound.
///
/// # Errors
///
/// Fails if the address is unparseable or cannot be bound.
pub fn spawn(
    listen: &str,
    mode: Mode,
    state: Arc<Mutex<State>>,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    let addr: SocketAddr = listen.parse()?;
    let server = Server::bind(addr)?;
    let bound = server.local_addr()?;

    std::thread::spawn(move || {
        loop {
            // A failure serving one client is not a reason to stop exporting.
            let _ = server.serve_one(|| snapshot(mode, &state));
        }
    });
    Ok(bound)
}

/// Gathers everything the exporter reports.
fn snapshot(mode: Mode, state: &Arc<Mutex<State>>) -> Snapshot {
    let loop_state = *state.lock().unwrap_or_else(PoisonError::into_inner);

    let fan = PwmFan::find();
    let reading = fan.as_ref().and_then(PwmFan::read);
    let governor = KernelFan::governor_state();

    let usb = argon_hal::discovery::usb_devices();
    let devices = vec![
        ("ups", argon_hal::discovery::find_argon_ups(&usb).is_some()),
        (
            "zigbee",
            argon_hal::discovery::find_argon_zigbee(&usb).is_some(),
        ),
    ];

    Snapshot {
        version: env!("CARGO_PKG_VERSION"),
        mode: mode.as_str(),
        cpu_decicelsius: loop_state.cpu_decicelsius,
        fan_pwm: reading.map(|r| r.pwm),
        fan_rpm: reading.and_then(|r| r.rpm),
        fan_backend: fan.as_ref().map(|_| "kernel-pwm"),
        // The kernel fan is never writable by us: taking it over would mean disabling the
        // thermal zone and with it the critical trip. See ADR and the capture notes.
        fan_writable: fan.as_ref().map(|_| false),
        governor_state: governor.first().map(|c| (c.state, c.max_state)),
        vendor_units: foreign::vendor_units()
            .into_iter()
            .map(|u| {
                let active = u.is_active();
                (u.unit, active)
            })
            .collect(),
        devices,
        sensor_failures: loop_state.sensor_failures,
    }
}
