// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! The UPS serial frame codec (`ARGON-UPS-FRAME`, `ARGON-UPS-READSHORT`).
//!
//! Wire format:
//!
//! ```text
//! 0xFE | payload_len | cmd | payload[payload_len] | checksum
//! ```
//!
//! where `checksum` is the sum of every preceding byte of the frame, truncated to 8 bits.
//! A pure read is the four-byte degenerate case with an empty payload.

use core::fmt;

/// The byte that begins every frame.
pub const START_BYTE: u8 = 0xFE;

/// The largest payload a frame can carry, bounded by the one-byte length field.
pub const MAX_PAYLOAD: usize = u8::MAX as usize;

/// The largest complete frame: start + len + cmd + payload + checksum.
const MAX_FRAME: usize = MAX_PAYLOAD + 4;

/// A decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    cmd: u8,
    len: u8,
    payload: [u8; MAX_PAYLOAD],
}

impl Frame {
    /// Builds a frame from a command byte and payload.
    pub fn new(cmd: u8, payload: &[u8]) -> Result<Self, FrameError> {
        // try_from is the bounds check: the length field is one byte, so a payload that
        // does not fit in a u8 is exactly the payload this frame format cannot describe.
        let len =
            u8::try_from(payload.len()).map_err(|_| FrameError::PayloadTooLong(payload.len()))?;
        let mut buf = [0u8; MAX_PAYLOAD];
        buf[..payload.len()].copy_from_slice(payload);
        Ok(Self {
            cmd,
            len,
            payload: buf,
        })
    }

    /// The command byte.
    #[must_use]
    pub const fn cmd(&self) -> u8 {
        self.cmd
    }

    /// The payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.len as usize]
    }

    /// Serialises the frame into `out`, returning the number of bytes written.
    ///
    /// `out` must be at least `payload.len() + 4` bytes.
    pub fn encode_into(&self, out: &mut [u8]) -> Result<usize, FrameError> {
        let total = self.len as usize + 4;
        if out.len() < total {
            return Err(FrameError::BufferTooSmall {
                need: total,
                got: out.len(),
            });
        }
        out[0] = START_BYTE;
        out[1] = self.len;
        out[2] = self.cmd;
        out[3..3 + self.len as usize].copy_from_slice(self.payload());
        out[total - 1] = checksum(&out[..total - 1]);
        Ok(total)
    }
}

/// The frame checksum: the low eight bits of the sum of all preceding bytes.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, b| acc.wrapping_add(*b))
}

/// Encodes the four-byte read-request form for `cmd` (`ARGON-UPS-READSHORT`).
#[must_use]
pub const fn encode_read(cmd: u8) -> [u8; 4] {
    // checksum == (0xFE + 0x00 + cmd) & 0xFF
    [START_BYTE, 0x00, cmd, cmd.wrapping_add(START_BYTE)]
}

/// Something wrong with a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// The payload exceeds what the one-byte length field can describe.
    PayloadTooLong(usize),
    /// The supplied output buffer was too small to hold the encoded frame.
    BufferTooSmall {
        /// Bytes required.
        need: usize,
        /// Bytes available.
        got: usize,
    },
    /// The frame's trailing checksum did not match the computed one.
    ///
    /// The frame is **rejected**, not truncated or partially accepted.
    BadChecksum {
        /// The checksum the device sent.
        got: u8,
        /// The checksum we computed.
        want: u8,
    },
    /// Bytes were discarded while hunting for a start byte.
    Desync {
        /// How many bytes were dropped.
        dropped: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadTooLong(n) => write!(f, "payload of {n} bytes exceeds {MAX_PAYLOAD}"),
            Self::BufferTooSmall { need, got } => {
                write!(f, "output buffer holds {got} bytes, need {need}")
            }
            Self::BadChecksum { got, want } => {
                write!(
                    f,
                    "checksum mismatch: device sent 0x{got:02x}, computed 0x{want:02x}"
                )
            }
            Self::Desync { dropped } => {
                write!(f, "resynchronised after discarding {dropped} bytes")
            }
        }
    }
}

impl core::error::Error for FrameError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hunting,
    Len,
    Cmd,
    Payload,
    Checksum,
}

/// An incremental frame decoder for a byte stream that may desynchronise.
///
/// # Resource bounds
///
/// The reader is a state machine, not an accumulate-until-it-parses buffer. Its memory is a
/// single fixed [`MAX_FRAME`]-byte array allocated up front, so a stream containing no start
/// byte — or an endless run of them — cannot grow it. Each [`FrameReader::push`] call does a
/// constant amount of work per byte.
///
/// This bounds *memory and work*. It does not bound *time*: a device dribbling one byte per
/// read timeout will keep a caller waiting forever. A real operation deadline spanning the
/// whole exchange, rather than a per-read timeout, belongs in the I/O layer and is that
/// layer's responsibility.
#[derive(Debug)]
pub struct FrameReader {
    state: State,
    buf: [u8; MAX_FRAME],
    /// Bytes of the current frame held so far, including the start byte.
    have: usize,
    /// Declared payload length of the frame in progress.
    want_payload: usize,
    /// Bytes discarded since the last good frame, for diagnostics.
    dropped: usize,
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameReader {
    /// Creates a reader hunting for the start of a frame.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Hunting,
            buf: [0u8; MAX_FRAME],
            have: 0,
            want_payload: 0,
            dropped: 0,
        }
    }

    /// Discards any partially-received frame and returns to hunting.
    pub fn reset(&mut self) {
        self.state = State::Hunting;
        self.have = 0;
        self.want_payload = 0;
        self.dropped = 0;
    }

    /// Feeds one byte, returning a frame once one completes.
    ///
    /// Returns `Err` for a rejected frame; the reader resynchronises and continues, so an
    /// error is not terminal.
    pub fn push(&mut self, byte: u8) -> Option<Result<Frame, FrameError>> {
        match self.state {
            State::Hunting => {
                if byte == START_BYTE {
                    self.buf[0] = byte;
                    self.have = 1;
                    self.state = State::Len;
                    if self.dropped > 0 {
                        let dropped = core::mem::take(&mut self.dropped);
                        return Some(Err(FrameError::Desync { dropped }));
                    }
                } else {
                    self.dropped = self.dropped.saturating_add(1);
                }
                None
            }
            State::Len => {
                self.buf[1] = byte;
                self.have = 2;
                self.want_payload = byte as usize;
                self.state = State::Cmd;
                None
            }
            State::Cmd => {
                self.buf[2] = byte;
                self.have = 3;
                self.state = if self.want_payload == 0 {
                    State::Checksum
                } else {
                    State::Payload
                };
                None
            }
            State::Payload => {
                self.buf[self.have] = byte;
                self.have += 1;
                if self.have == self.want_payload + 3 {
                    self.state = State::Checksum;
                }
                None
            }
            State::Checksum => {
                let want = checksum(&self.buf[..self.have]);
                let cmd = self.buf[2];
                let payload_end = self.have;
                let result = if byte == want {
                    Frame::new(cmd, &self.buf[3..payload_end])
                } else {
                    Err(FrameError::BadChecksum { got: byte, want })
                };
                self.state = State::Hunting;
                self.have = 0;
                self.want_payload = 0;
                Some(result)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    fn decode_all(bytes: &[u8]) -> (Vec<Frame>, Vec<FrameError>) {
        let mut r = FrameReader::new();
        let (mut ok, mut err) = (Vec::new(), Vec::new());
        for &b in bytes {
            match r.push(b) {
                Some(Ok(f)) => ok.push(f),
                Some(Err(e)) => err.push(e),
                None => {}
            }
        }
        (ok, err)
    }

    #[test]
    fn read_shortcut_matches_the_general_encoder() {
        // The documented 4-byte read form must be exactly what the general path produces.
        for cmd in 0..=255u8 {
            let short = encode_read(cmd);
            let mut general = [0u8; 4];
            let n = Frame::new(cmd, &[])
                .unwrap()
                .encode_into(&mut general)
                .unwrap();
            assert_eq!(n, 4);
            assert_eq!(short, general, "divergence at cmd 0x{cmd:02x}");
        }
    }

    #[test]
    fn round_trips_every_command_and_payload_length() {
        for cmd in [0u8, 2, 3, 4, 5, 6, 7, 8, 9, 0xFE, 0xFF] {
            for len in [0usize, 1, 2, 5, 6, 64, 254, 255] {
                let payload: Vec<u8> = (0..len)
                    .map(|i| u8::try_from((i * 7 + 3) % 256).unwrap())
                    .collect();
                let f = Frame::new(cmd, &payload).unwrap();
                let mut buf = [0u8; MAX_FRAME];
                let n = f.encode_into(&mut buf).unwrap();
                let (frames, errs) = decode_all(&buf[..n]);
                assert!(errs.is_empty(), "cmd {cmd} len {len}: {errs:?}");
                assert_eq!(frames.len(), 1, "cmd {cmd} len {len}");
                assert_eq!(frames[0].cmd(), cmd);
                assert_eq!(frames[0].payload(), &payload[..]);
            }
        }
    }

    #[test]
    fn a_corrupt_checksum_is_rejected_not_accepted() {
        let f = Frame::new(0, &[0x57, 0x00]).unwrap();
        let mut buf = [0u8; MAX_FRAME];
        let n = f.encode_into(&mut buf).unwrap();
        buf[n - 1] ^= 0xFF; // corrupt only the checksum
        let (frames, errs) = decode_all(&buf[..n]);
        assert!(frames.is_empty(), "a bad-checksum frame was accepted");
        assert!(
            matches!(errs.as_slice(), [FrameError::BadChecksum { .. }]),
            "{errs:?}"
        );
    }

    #[test]
    fn resynchronises_after_leading_garbage() {
        let f = Frame::new(4, &[113]).unwrap();
        let mut frame = [0u8; MAX_FRAME];
        let n = f.encode_into(&mut frame).unwrap();

        let mut stream = vec![0x11, 0x22, 0x33, 0x44];
        stream.extend_from_slice(&frame[..n]);

        let (frames, errs) = decode_all(&stream);
        assert_eq!(frames.len(), 1, "failed to recover the frame after garbage");
        assert_eq!(frames[0].payload(), &[113]);
        assert!(
            matches!(errs.as_slice(), [FrameError::Desync { dropped: 4 }]),
            "{errs:?}"
        );
    }

    #[test]
    fn a_stream_with_no_start_byte_consumes_bounded_memory() {
        // The failure this guards: an accumulate-until-parse decoder growing without limit
        // on a link that never produces a start byte.
        let mut r = FrameReader::new();
        for _ in 0..1_000_000 {
            assert!(r.push(0x00).is_none());
        }
        assert_eq!(r.have, 0, "reader retained partial state while hunting");
        assert_eq!(core::mem::size_of_val(&r.buf), MAX_FRAME);
    }

    #[test]
    fn an_endless_run_of_start_bytes_does_not_wedge_or_grow() {
        let mut r = FrameReader::new();
        for _ in 0..100_000 {
            let _ = r.push(START_BYTE);
        }
        assert!(r.have <= MAX_FRAME);
    }

    #[test]
    fn declared_length_bounds_the_frame() {
        // len = 0xFF is the largest a frame can declare; it must not overrun the buffer.
        let mut stream = vec![START_BYTE, 0xFF, 0x00];
        stream.extend(core::iter::repeat_n(0xAB, 255));
        let want = checksum(&stream);
        stream.push(want);
        let (frames, errs) = decode_all(&stream);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload().len(), 255);
    }

    #[test]
    fn real_exchanges_captured_from_hardware() {
        // Recorded from an Argon PWR UPS, firmware 113, on 2026-09-17. See
        // crates/argon-proto/tests/tapes/ups-reads.json for provenance. This is what
        // promoted ARGON-UPS-FRAME from `inferred` to `observed`.
        //
        // Worth noting: the firmware-version exchange below was written as a *synthetic*
        // test before any hardware capture existed, predicted purely from the framing rules
        // -- and the device produced those bytes exactly.
        for (cmd, payload, wire) in [
            (
                0u8,
                &[0x5Bu8, 0x00][..],
                &[0xFE, 0x02, 0x00, 0x5B, 0x00, 0x5B][..],
            ),
            (4, &[113][..], &[0xFE, 0x01, 0x04, 113, 0x74][..]),
            (
                5,
                &[0x26, 0x09, 0x17, 0x13, 0x29, 0x15][..],
                &[0xFE, 0x06, 0x05, 0x26, 0x09, 0x17, 0x13, 0x29, 0x15, 0xA0][..],
            ),
            // No wake schedule set: a well-formed frame with an empty payload.
            (7, &[][..], &[0xFE, 0x00, 0x07, 0x05][..]),
            (
                2,
                &[0x03, 0x52][..],
                &[0xFE, 0x02, 0x02, 0x03, 0x52, 0x57][..],
            ),
        ] {
            // Our encoder must produce exactly what the device sent.
            let frame = Frame::new(cmd, payload).unwrap();
            let mut buf = [0u8; MAX_FRAME];
            let n = frame.encode_into(&mut buf).unwrap();
            assert_eq!(
                &buf[..n],
                wire,
                "encoding cmd {cmd} diverged from the captured wire"
            );

            // And our decoder must read the device's bytes back.
            let (frames, errs) = decode_all(wire);
            assert!(errs.is_empty(), "cmd {cmd}: {errs:?}");
            assert_eq!(frames.len(), 1, "cmd {cmd}");
            assert_eq!(frames[0].cmd(), cmd);
            assert_eq!(frames[0].payload(), payload);
        }
    }

    #[test]
    fn the_read_requests_match_what_was_sent_to_hardware() {
        for (cmd, wire) in [
            (0u8, [0xFE, 0x00, 0x00, 0xFE]),
            (2, [0xFE, 0x00, 0x02, 0x00]),
            (4, [0xFE, 0x00, 0x04, 0x02]),
            (5, [0xFE, 0x00, 0x05, 0x03]),
            (7, [0xFE, 0x00, 0x07, 0x05]),
        ] {
            assert_eq!(encode_read(cmd), wire, "read request for cmd {cmd}");
        }
    }
}
