// SPDX-License-Identifier: GPL-3.0-or-later
//! Texas Instruments Z-Stack "Monitor and Test" (MT) framing, as spoken by a CC2652-based Zigbee
//! coordinator such as the Argon Industria Zigbee module.
//!
//! Written from TI's published Z-Stack MT API documentation: `documented` facts in the ledger's
//! terms ([DOC-TI-MT], `ZIGBEE-*` in `docs/protocol/FACTS.md`). That documents the firmware,
//! not the Argon module, so what the module actually runs is confirmed by task T16. Only side-effect-free requests are defined: this is a
//! health probe, not a Zigbee stack, and a coordinator's network state belongs to whatever runs
//! the network (zigbee2mqtt, ZHA), not to us.
//!
//! # Framing
//!
//! `0xFE | LEN | CMD0 | CMD1 | DATA[LEN] | FCS`, where `FCS` is the XOR of every byte from `LEN`
//! through the last data byte. Note the start byte is the same as the Argon UPS protocol's, but
//! the checksum is not -- XOR here, a sum there -- so the two codecs must never be mixed up.
//!
//! `CMD0` packs a type in its top three bits (`0x20` synchronous request, `0x40` asynchronous
//! indication, `0x60` synchronous response) and a subsystem in the low five (`0x01` = SYS).

/// Start of frame.
pub const SOF: u8 = 0xFE;

/// Largest data length a frame can carry.
pub const MAX_DATA: usize = 250;

/// `SYS_PING`: asks the firmware to answer; the reply carries its capability bits.
pub const SYS_PING: (u8, u8) = (0x21, 0x01);
/// The synchronous response to `SYS_PING`.
pub const SYS_PING_RSP: (u8, u8) = (0x61, 0x01);
/// `SYS_VERSION`: transport revision, product and release numbers.
pub const SYS_VERSION: (u8, u8) = (0x21, 0x02);
/// The synchronous response to `SYS_VERSION`.
pub const SYS_VERSION_RSP: (u8, u8) = (0x61, 0x02);
/// `SYS_RESET_IND`: sent unprompted by the firmware after every restart.
pub const SYS_RESET_IND: (u8, u8) = (0x41, 0x80);

/// One MT frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MtFrame {
    /// Type and subsystem.
    pub cmd0: u8,
    /// Command id within the subsystem.
    pub cmd1: u8,
    data: [u8; MAX_DATA],
    len: usize,
}

impl MtFrame {
    /// A frame with the given command and data, or `None` if the data is longer than
    /// [`MAX_DATA`].
    #[must_use]
    pub fn new((cmd0, cmd1): (u8, u8), data: &[u8]) -> Option<Self> {
        if data.len() > MAX_DATA {
            return None;
        }
        let mut buf = [0u8; MAX_DATA];
        buf[..data.len()].copy_from_slice(data);
        Some(Self {
            cmd0,
            cmd1,
            data: buf,
            len: data.len(),
        })
    }

    /// The data bytes.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data[..self.len]
    }

    /// Whether this is the given command.
    #[must_use]
    pub const fn is(&self, (cmd0, cmd1): (u8, u8)) -> bool {
        self.cmd0 == cmd0 && self.cmd1 == cmd1
    }

    /// Encodes into `out`, returning the length written, or `None` if `out` is too small.
    #[must_use]
    pub fn encode_into(&self, out: &mut [u8]) -> Option<usize> {
        let n = self.len + 5;
        if out.len() < n {
            return None;
        }
        // `len` is at most MAX_DATA = 250, so it fits a byte.
        #[allow(clippy::cast_possible_truncation)]
        let len = self.len as u8;
        out[0] = SOF;
        out[1] = len;
        out[2] = self.cmd0;
        out[3] = self.cmd1;
        out[4..4 + self.len].copy_from_slice(self.data());
        out[n - 1] = fcs(&out[1..n - 1]);
        Some(n)
    }
}

/// The frame check sequence: XOR of every byte given.
#[must_use]
pub fn fcs(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, b| acc ^ b)
}

/// Why bytes were rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MtError {
    /// The check byte did not match.
    BadFcs {
        /// What arrived.
        got: u8,
        /// What the bytes add up to.
        want: u8,
    },
    /// A length above [`MAX_DATA`].
    TooLong(u8),
}

impl core::fmt::Display for MtError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadFcs { got, want } => write!(f, "bad FCS 0x{got:02x}, expected 0x{want:02x}"),
            Self::TooLong(n) => write!(f, "length {n} exceeds {MAX_DATA}"),
        }
    }
}

/// Assembles frames from a byte stream, resynchronising on `SOF`.
#[derive(Debug, Clone)]
pub struct MtReader {
    buf: [u8; MAX_DATA + 5],
    have: usize,
}

impl Default for MtReader {
    fn default() -> Self {
        Self::new()
    }
}

impl MtReader {
    /// An empty reader.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: [0; MAX_DATA + 5],
            have: 0,
        }
    }

    /// Feeds one byte. Returns a frame, a rejection, or nothing yet.
    pub fn push(&mut self, b: u8) -> Option<Result<MtFrame, MtError>> {
        if self.have == 0 {
            if b == SOF {
                self.buf[0] = b;
                self.have = 1;
            }
            return None;
        }
        self.buf[self.have] = b;
        self.have += 1;

        if self.have == 2 && usize::from(b) > MAX_DATA {
            self.have = 0;
            return Some(Err(MtError::TooLong(b)));
        }
        let data_len = usize::from(self.buf[1]);
        let total = data_len + 5;
        if self.have < total {
            return None;
        }
        self.have = 0;
        let want = fcs(&self.buf[1..total - 1]);
        let got = self.buf[total - 1];
        if got != want {
            return Some(Err(MtError::BadFcs { got, want }));
        }
        MtFrame::new((self.buf[2], self.buf[3]), &self.buf[4..4 + data_len]).map(Ok)
    }
}

/// The `SYS_VERSION` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    /// Transport protocol revision.
    pub transport_rev: u8,
    /// Product id.
    pub product: u8,
    /// Major release.
    pub major: u8,
    /// Minor release.
    pub minor: u8,
    /// Maintenance release.
    pub maint: u8,
    /// Firmware build revision, present in newer Z-Stack releases (a 4-byte little-endian
    /// number, commonly a build date such as `20230507`).
    pub revision: Option<u32>,
}

impl Version {
    /// Decodes the data of a `SYS_VERSION` response: five bytes, or nine with a revision.
    #[must_use]
    pub fn decode(data: &[u8]) -> Option<Self> {
        match *data {
            [transport_rev, product, major, minor, maint] => Some(Self {
                transport_rev,
                product,
                major,
                minor,
                maint,
                revision: None,
            }),
            [
                transport_rev,
                product,
                major,
                minor,
                maint,
                r0,
                r1,
                r2,
                r3,
                ..,
            ] => Some(Self {
                transport_rev,
                product,
                major,
                minor,
                maint,
                revision: Some(u32::from_le_bytes([r0, r1, r2, r3])),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    fn encode(cmd: (u8, u8), data: &[u8]) -> ([u8; 16], usize) {
        let mut out = [0u8; 16];
        let n = MtFrame::new(cmd, data)
            .unwrap()
            .encode_into(&mut out)
            .unwrap();
        (out, n)
    }

    #[test]
    fn the_published_ping_bytes_come_out() {
        // "FE 00 21 01 20" is the widely published SYS_PING request; an independent check on
        // the XOR, rather than this code checking itself.
        let (out, n) = encode(SYS_PING, &[]);
        assert_eq!(&out[..n], &[0xFE, 0x00, 0x21, 0x01, 0x20]);
        let (out, n) = encode(SYS_VERSION, &[]);
        assert_eq!(&out[..n], &[0xFE, 0x00, 0x21, 0x02, 0x23]);
    }

    #[test]
    fn a_frame_round_trips_through_the_reader_after_noise() {
        let (out, n) = encode(SYS_PING_RSP, &[0x59, 0x06]);
        let mut r = MtReader::new();
        // Junk first, including bytes that are not SOF: the reader must ignore them.
        for b in [0x00, 0x55, 0x55, 0x13] {
            assert_eq!(r.push(b), None);
        }
        let mut got = None;
        for &b in &out[..n] {
            if let Some(f) = r.push(b) {
                got = Some(f);
            }
        }
        let f = got.expect("no frame").expect("rejected");
        assert!(f.is(SYS_PING_RSP));
        assert_eq!(f.data(), &[0x59, 0x06]);
    }

    #[test]
    fn a_corrupted_frame_is_rejected_not_returned() {
        let (mut out, n) = encode(SYS_VERSION_RSP, &[2, 0, 2, 7, 1]);
        out[n - 1] ^= 0xFF;
        let mut r = MtReader::new();
        let res: Vec<_> = out[..n].iter().filter_map(|&b| r.push(b)).collect();
        assert!(matches!(res.as_slice(), [Err(MtError::BadFcs { .. })]));
    }

    #[test]
    fn an_impossible_length_resynchronises() {
        let mut r = MtReader::new();
        assert_eq!(r.push(SOF), None);
        assert_eq!(r.push(0xFF), Some(Err(MtError::TooLong(0xFF))));
        // And the next frame still decodes.
        let (out, n) = encode(SYS_PING_RSP, &[1, 0]);
        let ok: Vec<_> = out[..n].iter().filter_map(|&b| r.push(b)).collect();
        assert!(matches!(ok.as_slice(), [Ok(_)]));
    }

    #[test]
    fn versions_decode_with_and_without_a_revision() {
        let v = Version::decode(&[2, 0, 2, 7, 1]).unwrap();
        assert_eq!((v.major, v.minor, v.maint, v.revision), (2, 7, 1, None));
        let rev = 20_230_507u32.to_le_bytes();
        let v = Version::decode(&[2, 1, 2, 7, 1, rev[0], rev[1], rev[2], rev[3]]).unwrap();
        assert_eq!(v.revision, Some(20_230_507));
        assert_eq!(Version::decode(&[1, 2, 3]), None);
    }
}
