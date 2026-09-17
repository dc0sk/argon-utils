// SPDX-License-Identifier: GPL-3.0-or-later
//! The Argon PWR UPS over its serial protocol, and monitoring built on it.
//!
//! Facts: `ARGON-UPS-FRAME`, `ARGON-UPS-CMD0/2/4/5/7` and `ARGON-UPS-CMD7-EMPTY`, all
//! `observed` on hardware on 2026-09-17. The HID interface is not used: it serves no data on
//! firmware 113 (`ARGON-UPS-HID-LIVE`).

use argon_hal::serial::SerialLink;
use argon_hal::{Error, Result};
use argon_proto::ups::policy::{BatteryPolicy, Decision, Observation};
use argon_proto::ups::{BatteryStatus, Command, Frame, UpsTime};
use std::time::{Duration, Instant};

/// How long one request may take, start to finish.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(2);

/// Something that can carry a UPS request and return its reply.
pub trait UpsLink {
    /// Sends a command and returns the reply frame for it.
    ///
    /// # Errors
    ///
    /// Fails on I/O error, timeout, or if the link refuses the command.
    fn request(&mut self, cmd: Command, payload: &[u8], deadline: Instant) -> Result<Frame>;
}

impl UpsLink for SerialLink {
    fn request(&mut self, cmd: Command, payload: &[u8], deadline: Instant) -> Result<Frame> {
        Self::request(self, cmd.as_byte(), payload, deadline)
    }
}

/// Passes only side-effect-free queries.
///
/// What read-only mode means for the UPS, enforced here rather than by convention. Allows
/// exactly the five queries confirmed on hardware; refuses setting the clock, setting a wake
/// schedule, resetting the battery meter, and acknowledging -- the last because its
/// semantics are only `inferred`.
pub struct QueryOnly<L>(pub L);

impl QueryOnly<()> {
    /// Whether a command is a side-effect-free query.
    #[must_use]
    pub const fn is_query(cmd: Command) -> bool {
        matches!(
            cmd,
            Command::BatteryStatus
                | Command::ChargeCurrent
                | Command::FirmwareVersion
                | Command::GetRtc
                | Command::GetWake
        )
    }
}

impl<L: UpsLink> UpsLink for QueryOnly<L> {
    fn request(&mut self, cmd: Command, payload: &[u8], deadline: Instant) -> Result<Frame> {
        if !QueryOnly::is_query(cmd) {
            return Err(Error::WriteBlocked {
                what: format!("ups command {cmd:?}"),
                reason: "only side-effect-free queries are allowed on this link",
            });
        }
        self.0.request(cmd, payload, deadline)
    }
}

/// The UPS, driven over some link.
pub struct Ups<L: UpsLink> {
    link: L,
}

impl<L: UpsLink> Ups<L> {
    /// Binds to a UPS.
    pub const fn new(link: L) -> Self {
        Self { link }
    }

    /// Battery percentage and power source.
    ///
    /// # Errors
    ///
    /// Fails on link error or an undecodable reply.
    pub fn battery(&mut self) -> Result<BatteryStatus> {
        let f = self.query(Command::BatteryStatus)?;
        BatteryStatus::decode(f.payload()).map_err(decode_err)
    }

    /// Firmware version.
    ///
    /// # Errors
    ///
    /// Fails on link error or an unexpected payload.
    pub fn firmware(&mut self) -> Result<u8> {
        let f = self.query(Command::FirmwareVersion)?;
        match f.payload() {
            [v] => Ok(*v),
            other => Err(Error::Parse {
                what: "a firmware version",
                got: format!("{other:02x?}"),
            }),
        }
    }

    /// The UPS clock, UTC.
    ///
    /// # Errors
    ///
    /// Fails on link error or an undecodable reply.
    pub fn clock(&mut self) -> Result<UpsTime> {
        let f = self.query(Command::GetRtc)?;
        UpsTime::decode_clock(f.payload()).map_err(decode_err)
    }

    /// The wake schedule, if one is set.
    ///
    /// # Errors
    ///
    /// Fails on link error or an undecodable reply.
    pub fn wake(&mut self) -> Result<Option<UpsTime>> {
        let f = self.query(Command::GetWake)?;
        UpsTime::decode_optional_schedule(f.payload()).map_err(decode_err)
    }

    /// The charge-current value, raw. **Units unknown** (`ARGON-UPS-CMD2`).
    ///
    /// # Errors
    ///
    /// Fails on link error or an unexpected payload.
    pub fn charge_current_raw(&mut self) -> Result<u16> {
        let f = self.query(Command::ChargeCurrent)?;
        match f.payload() {
            [hi, lo] => Ok(u16::from_be_bytes([*hi, *lo])),
            other => Err(Error::Parse {
                what: "a charge current",
                got: format!("{other:02x?}"),
            }),
        }
    }

    /// The underlying link.
    pub const fn link(&self) -> &L {
        &self.link
    }

    fn query(&mut self, cmd: Command) -> Result<Frame> {
        self.link
            .request(cmd, &[], Instant::now() + REQUEST_DEADLINE)
    }
}

fn decode_err(e: argon_proto::ups::DecodeError) -> Error {
    Error::Parse {
        what: "a UPS reply",
        got: e.to_string(),
    }
}

/// One monitoring poll.
#[derive(Debug)]
pub struct Poll {
    /// The battery reading, if it succeeded.
    pub battery: Option<BatteryStatus>,
    /// Why the reading failed, if it did.
    pub error: Option<Error>,
    /// What the policy made of it.
    pub decision: Decision,
    /// Consecutive failed polls, including this one.
    pub consecutive_failures: u32,
}

/// Polls the UPS and feeds the battery policy.
pub struct UpsMonitor<L: UpsLink> {
    ups: Ups<L>,
    policy: BatteryPolicy,
    failures: u32,
}

impl<L: UpsLink> UpsMonitor<L> {
    /// Creates a monitor.
    pub const fn new(ups: Ups<L>, policy: BatteryPolicy) -> Self {
        Self {
            ups,
            policy,
            failures: 0,
        }
    }

    /// The UPS, for occasional queries outside the battery poll.
    pub const fn ups_mut(&mut self) -> &mut Ups<L> {
        &mut self.ups
    }

    /// Reads the battery once and returns the policy's decision.
    ///
    /// A failed read is not an error here: it becomes an `Unknown` observation, which the
    /// policy never turns into shutdown advice. Losing the link is a condition to report, not
    /// a reason to stop monitoring.
    pub fn poll(&mut self, uptime: Duration) -> Poll {
        match self.ups.battery() {
            Ok(b) => {
                self.failures = 0;
                let decision = self.policy.observe(
                    Some(Observation {
                        percent: b.percent,
                        source: b.source,
                    }),
                    uptime,
                );
                Poll {
                    battery: Some(b),
                    error: None,
                    decision,
                    consecutive_failures: 0,
                }
            }
            Err(e) => {
                self.failures = self.failures.saturating_add(1);
                let decision = self.policy.observe(None, uptime);
                Poll {
                    battery: None,
                    error: Some(e),
                    decision,
                    consecutive_failures: self.failures,
                }
            }
        }
    }
}
