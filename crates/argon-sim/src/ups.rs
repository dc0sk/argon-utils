// SPDX-License-Identifier: GPL-3.0-or-later
//! A simulated Argon PWR UPS speaking the `0xFE`-framed serial protocol.

use argon_proto::ups::{Command, Frame, FrameReader, UpsTime};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The state a simulated UPS reports.
#[derive(Debug, Clone, Copy)]
pub struct UpsState {
    /// Charge percentage.
    pub percent: u8,
    /// Wire encoding of the power source: 0 is mains, non-zero is battery.
    pub charging_byte: u8,
    /// Firmware version reported by command 4.
    pub firmware: u8,
    /// Current RTC value.
    pub clock: UpsTime,
    /// Wake schedule, if set.
    pub wake: Option<UpsTime>,
    /// Raw charge-current value reported by command 2. Units unknown; deliberately opaque.
    pub charge_current_raw: u16,
}

impl Default for UpsState {
    fn default() -> Self {
        Self {
            percent: 93,
            charging_byte: 0,
            firmware: 113,
            clock: UpsTime {
                year: 2026,
                month: 9,
                day: 15,
                hour: 14,
                minute: 30,
                second: Some(0),
            },
            wake: None,
            charge_current_raw: 0,
        }
    }
}

/// How the simulator should misbehave, so error handling can be tested deliberately.
#[derive(Debug, Clone, Copy, Default)]
pub struct Faults {
    /// Emit this many junk bytes before each response, to force a resynchronisation.
    pub junk_prefix: usize,
    /// Corrupt the checksum of every Nth response. 0 disables.
    pub corrupt_every: usize,
    /// Do not answer at all. Used to prove the caller's deadline actually bounds it.
    pub silent: bool,
    /// Split each response into single-byte writes with this pause between them.
    pub dribble: Option<Duration>,
    /// Acknowledge a clock set as usual but do not change the clock -- a device that ignores
    /// the write. The negative control for task T15: an experiment that cannot tell this apart
    /// from a real set would confirm nothing.
    pub ignore_clock_set: bool,
    /// Acknowledge a wake-schedule set but do not store it. The negative control for T17.
    pub ignore_wake_set: bool,
}

/// A simulated UPS serving one client over a byte stream.
pub struct UpsSim {
    state: UpsState,
    faults: Faults,
    reader: FrameReader,
    responses: usize,
    /// When `state.clock` was last set. The real clock ticks, and a frozen one made every
    /// read-back after a set look wrong by the elapsed time.
    clock_set_at: Instant,
}

impl UpsSim {
    /// Creates a simulator with the given state.
    #[must_use]
    pub fn new(state: UpsState) -> Self {
        Self::with_faults(state, Faults::default())
    }

    /// Creates a simulator that misbehaves in the given ways.
    #[must_use]
    pub fn with_faults(state: UpsState, faults: Faults) -> Self {
        Self {
            state,
            faults,
            reader: FrameReader::new(),
            responses: 0,
            clock_set_at: Instant::now(),
        }
    }

    /// The clock as it reads now: the last value set, plus the time since.
    fn clock_now(&self) -> UpsTime {
        self.state
            .clock
            .to_unix_seconds()
            .map(|s| s + self.clock_set_at.elapsed().as_secs())
            .and_then(UpsTime::from_unix_seconds)
            .unwrap_or(self.state.clock)
    }

    /// The state the simulator is reporting.
    #[must_use]
    pub const fn state(&self) -> &UpsState {
        &self.state
    }

    /// Serves requests until `deadline`.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the stream.
    pub fn serve<S: Read + Write>(
        &mut self,
        stream: &mut S,
        deadline: Instant,
    ) -> std::io::Result<()> {
        self.serve_until(stream, deadline, &Arc::new(AtomicBool::new(false)))
    }

    /// Serves requests until `deadline`, or until `stop` is set.
    ///
    /// The stop flag exists so tests do not have to wait out the deadline: without it a
    /// suite of ten tests pays the full serve window ten times over, and slow tests are
    /// tests people stop running.
    ///
    /// # Errors
    ///
    /// Propagates I/O errors from the stream.
    pub fn serve_until<S: Read + Write>(
        &mut self,
        stream: &mut S,
        deadline: Instant,
        stop: &Arc<AtomicBool>,
    ) -> std::io::Result<()> {
        let mut buf = [0u8; 64];
        while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
            let n = match stream.read(&mut buf) {
                Ok(0) => continue,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => continue,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            for &b in &buf[..n] {
                if let Some(Ok(frame)) = self.reader.push(b) {
                    self.respond(stream, &frame)?;
                }
            }
        }
        Ok(())
    }

    fn respond<S: Write>(&mut self, stream: &mut S, request: &Frame) -> std::io::Result<()> {
        if self.faults.silent {
            return Ok(());
        }
        let Some(cmd) = Command::from_byte(request.cmd()) else {
            // Unknown command: a real device's behaviour here is undetermined, and this
            // project never sends one. Silence is the safest stand-in.
            return Ok(());
        };

        let payload: Vec<u8> = match cmd {
            Command::BatteryStatus => vec![self.state.percent, self.state.charging_byte],
            Command::ChargeCurrent => self.state.charge_current_raw.to_be_bytes().to_vec(),
            Command::FirmwareVersion => vec![self.state.firmware],
            Command::GetRtc => self.clock_now().encode_clock().unwrap_or([0; 6]).to_vec(),
            Command::GetWake => match self.state.wake {
                Some(w) => w.encode_schedule().unwrap_or([0; 5]).to_vec(),
                // Observed on hardware (ARGON-UPS-CMD7-EMPTY): with nothing scheduled the
                // device replies with an empty payload. This used to be five zero bytes, a
                // guess made before the capture existed, which the real device contradicts.
                None => Vec::new(),
            },
            Command::SetRtc => {
                if let Ok(t) = UpsTime::decode_clock(request.payload()) {
                    if !self.faults.ignore_clock_set {
                        self.state.clock = t;
                        self.clock_set_at = Instant::now();
                    }
                }
                Vec::new()
            }
            Command::SetWake => {
                if let Ok(t) = UpsTime::decode_schedule(request.payload()) {
                    if !self.faults.ignore_wake_set {
                        self.state.wake = Some(t);
                    }
                }
                Vec::new()
            }
            // Both are acknowledged with an empty payload. ResetMeter is destructive on
            // real hardware; the simulator has no meter to discard.
            Command::ResetMeter | Command::Acknowledge => Vec::new(),
        };

        let Ok(frame) = Frame::new(request.cmd(), &payload) else {
            return Ok(());
        };
        let mut out = [0u8; 260];
        let Ok(n) = frame.encode_into(&mut out) else {
            return Ok(());
        };
        let mut bytes = out[..n].to_vec();

        self.responses += 1;
        if self.faults.corrupt_every > 0 && self.responses % self.faults.corrupt_every == 0 {
            if let Some(last) = bytes.last_mut() {
                *last ^= 0xFF;
            }
        }

        let mut wire = vec![0xAB; self.faults.junk_prefix];
        wire.extend_from_slice(&bytes);

        if let Some(gap) = self.faults.dribble {
            for b in wire {
                stream.write_all(&[b])?;
                stream.flush()?;
                std::thread::sleep(gap);
            }
        } else {
            stream.write_all(&wire)?;
            stream.flush()?;
        }
        Ok(())
    }
}
