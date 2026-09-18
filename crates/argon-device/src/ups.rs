// SPDX-License-Identifier: GPL-3.0-or-later
//! The Argon PWR UPS over its serial protocol, and monitoring built on it.
//!
//! Facts: `ARGON-UPS-FRAME`, `ARGON-UPS-CMD0/2/4/5/7` and `ARGON-UPS-CMD7-EMPTY`, all
//! `observed` on hardware on 2026-09-17; `ARGON-UPS-CMD3` and `ARGON-UPS-CMD3-REPLY`,
//! `observed` on 2026-09-18 (task T15). The HID interface is not used: it serves no data on
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

/// Which writes a [`Gate`] lets through, beyond queries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Writes {
    /// Setting the clock (`ARGON-UPS-CMD3`, observed in T15).
    pub clock: bool,
    /// Setting the wake schedule (`ARGON-UPS-CMD6`, observed in T17).
    pub wake: bool,
}

/// Queries, plus -- when allowed -- setting the clock and the wake schedule. Nothing else.
///
/// The link argond uses. Only writes whose semantics have been observed on hardware can be
/// let through: the clock (T15) and the wake schedule (T17). The meter reset and the
/// acknowledgement stay refused however the gate is built, because they are still only
/// `inferred`. Which writes are allowed is decided once, from the mode, when the gate is made.
pub struct Gate<L> {
    inner: L,
    writes: Writes,
}

impl<L> Gate<L> {
    /// Side-effect-free queries only: the same as [`QueryOnly`].
    pub const fn queries_only(inner: L) -> Self {
        Self::with_writes(
            inner,
            Writes {
                clock: false,
                wake: false,
            },
        )
    }

    /// Queries, and setting the clock.
    pub const fn with_clock_writes(inner: L) -> Self {
        Self::with_writes(
            inner,
            Writes {
                clock: true,
                wake: false,
            },
        )
    }

    /// Queries, and exactly the writes given.
    pub const fn with_writes(inner: L, writes: Writes) -> Self {
        Self { inner, writes }
    }

    /// The link behind the gate, for inspection in tests.
    pub const fn inner(&self) -> &L {
        &self.inner
    }

    /// Whether this gate lets `cmd` through.
    #[must_use]
    pub const fn allows(&self, cmd: Command) -> bool {
        QueryOnly::is_query(cmd)
            || (self.writes.clock && matches!(cmd, Command::SetRtc))
            || (self.writes.wake && matches!(cmd, Command::SetWake))
    }
}

impl<L: UpsLink> UpsLink for Gate<L> {
    fn request(&mut self, cmd: Command, payload: &[u8], deadline: Instant) -> Result<Frame> {
        if !self.allows(cmd) {
            return Err(Error::WriteBlocked {
                what: format!("ups command {cmd:?}"),
                reason: "not permitted by this link's gate",
            });
        }
        self.inner.request(cmd, payload, deadline)
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

    /// Sets the UPS clock, UTC.
    ///
    /// `ARGON-UPS-CMD3`: six BCD bytes, `YY MM DD HH MM SS`. `ARGON-UPS-CMD3-REPLY`: the device
    /// answers with an empty command-3 frame. Both observed in T15; a reply carrying a payload
    /// is not what was observed, so it is reported rather than accepted.
    ///
    /// # Errors
    ///
    /// Fails on link error, if the link's gate refuses the write, or on an unexpected reply.
    pub fn set_clock(&mut self, t: UpsTime) -> Result<()> {
        let payload = t.encode_clock().map_err(decode_err)?;
        let f = self
            .link
            .request(Command::SetRtc, &payload, Instant::now() + REQUEST_DEADLINE)?;
        if f.payload().is_empty() {
            Ok(())
        } else {
            Err(Error::Parse {
                what: "the reply to a clock set (expected an empty payload)",
                got: format!("{:02x?}", f.payload()),
            })
        }
    }

    /// Sets the wake schedule, UTC, to the minute.
    ///
    /// `ARGON-UPS-CMD6`: five BCD bytes, `YY MM DD HH MM`. `ARGON-UPS-CMD6-REPLY`: the device
    /// answers with an empty command-6 frame. Both observed in T17.
    ///
    /// # Errors
    ///
    /// Fails on link error, if the link's gate refuses the write, or on an unexpected reply.
    pub fn set_wake(&mut self, t: UpsTime) -> Result<()> {
        let payload = t.encode_schedule().map_err(decode_err)?;
        let f = self.link.request(
            Command::SetWake,
            &payload,
            Instant::now() + REQUEST_DEADLINE,
        )?;
        if f.payload().is_empty() {
            Ok(())
        } else {
            Err(Error::Parse {
                what: "the reply to a wake set (expected an empty payload)",
                got: format!("{:02x?}", f.payload()),
            })
        }
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
