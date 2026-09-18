// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests over adversarial input.
//!
//! Everything this crate parses comes from somewhere we do not control: a device on the far
//! end of a serial link that can desynchronise, a USB descriptor, a config file a user may
//! have edited by hand. These tests assert the properties that must hold for *all* input,
//! not just the inputs we thought of.
//!
//! # Why proptest rather than cargo-fuzz
//!
//! Coverage-guided fuzzing would explore deeper, but `cargo-fuzz` needs a nightly toolchain
//! that is not installed here. Writing fuzz targets and not running them would claim
//! coverage this project does not have, so these run on stable instead, in the normal test
//! gate, today. Coverage-guided fuzzing remains worth adding later; these are not a
//! substitute for it, and the docs should not describe them as one.
//!
//! # These tests were verified to be able to fail
//!
//! Sabotage-checked on 2026-09-15: a `panic!` planted on a specific input in the frame
//! decoder was found by `decoding_arbitrary_bytes_never_panics` in well under a second. A
//! robustness test that has never been seen to fail is indistinguishable from one that
//! cannot.

use argon_proto::bcd;
use argon_proto::fan;
use argon_proto::hid::ReportDescriptor;
use argon_proto::ups::{Frame, FrameReader, checksum, encode_read};
use proptest::prelude::*;

// ---------------------------------------------------------------------------------------
// UPS frame codec
// ---------------------------------------------------------------------------------------

proptest! {
    /// Whatever arrives on the wire, the decoder must not panic and must consume every byte.
    #[test]
    fn decoding_arbitrary_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let mut r = FrameReader::new();
        for b in bytes {
            let _ = r.push(b);
        }
    }

    /// Anything encoded decodes back to itself.
    #[test]
    fn frames_round_trip(cmd in any::<u8>(), payload in prop::collection::vec(any::<u8>(), 0..=255)) {
        let frame = Frame::new(cmd, &payload).expect("payload is within the length field");
        let mut buf = [0u8; 260];
        let n = frame.encode_into(&mut buf).expect("buffer is large enough");

        let mut r = FrameReader::new();
        let mut decoded = None;
        for &b in &buf[..n] {
            if let Some(Ok(f)) = r.push(b) {
                decoded = Some(f);
            }
        }
        let decoded = decoded.expect("a well-formed frame must decode");
        prop_assert_eq!(decoded.cmd(), cmd);
        prop_assert_eq!(decoded.payload(), &payload[..]);
    }

    /// A frame is found after leading rubbish that contains no start byte.
    ///
    /// The "contains no start byte" qualifier is not a convenience -- it is the strongest
    /// property this protocol can offer. `0xFE` has no escaping and is legal inside a length
    /// field, so a single stray start byte immediately before a frame is read as a length of
    /// 254 and swallows the frame behind it. See `ARGON-UPS-FRAME-AMBIGUITY` in
    /// docs/protocol/FACTS.md.
    ///
    /// An earlier version of this test asserted recovery from *arbitrary* junk. proptest
    /// refuted it in under a second with the minimal counterexample `junk = [0xFE]`, which
    /// is how the limitation was found.
    #[test]
    fn garbage_without_a_start_byte_does_not_lose_a_frame(
        junk in prop::collection::vec(any::<u8>(), 0..512),
        cmd in any::<u8>(),
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let junk: Vec<u8> = junk.into_iter().filter(|b| *b != argon_proto::ups::START_BYTE).collect();
        let frame = Frame::new(cmd, &payload).unwrap();
        let mut buf = [0u8; 260];
        let n = frame.encode_into(&mut buf).unwrap();

        let mut r = FrameReader::new();
        let mut found = None;
        for &b in junk.iter().chain(&buf[..n]) {
            if let Some(Ok(f)) = r.push(b) {
                found = Some(f);
            }
        }
        let found = found.expect("the appended frame must be found");
        prop_assert_eq!(found.cmd(), cmd);
        prop_assert_eq!(found.payload(), &payload[..]);
    }

    /// Whatever junk precedes it, a reset always restores the reader to a usable state.
    ///
    /// This is the guarantee the transport actually relies on: `SerialLink::request` resets
    /// before each exchange, so a desync costs one request and no more.
    #[test]
    fn a_reset_always_recovers_the_reader(
        junk in prop::collection::vec(any::<u8>(), 0..512),
        cmd in any::<u8>(),
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let frame = Frame::new(cmd, &payload).unwrap();
        let mut buf = [0u8; 260];
        let n = frame.encode_into(&mut buf).unwrap();

        let mut r = FrameReader::new();
        for b in junk {
            let _ = r.push(b);
        }
        r.reset();

        let mut found = None;
        for &b in &buf[..n] {
            if let Some(Ok(f)) = r.push(b) {
                found = Some(f);
            }
        }
        let found = found.expect("a reset reader must decode the next frame");
        prop_assert_eq!(found.cmd(), cmd);
        prop_assert_eq!(found.payload(), &payload[..]);
    }

    /// The documented four-byte read form must match the general encoder, for every command.
    #[test]
    fn the_read_shortcut_agrees_with_the_general_encoder(cmd in any::<u8>()) {
        let mut general = [0u8; 4];
        let n = Frame::new(cmd, &[]).unwrap().encode_into(&mut general).unwrap();
        prop_assert_eq!(n, 4);
        prop_assert_eq!(encode_read(cmd), general);
    }

    /// The checksum is the low byte of the sum, checked against an independent computation.
    #[test]
    fn checksum_matches_an_independent_oracle(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let oracle = bytes.iter().fold(0u32, |a, b| a + u32::from(*b)) & 0xFF;
        prop_assert_eq!(u32::from(checksum(&bytes)), oracle);
    }

    /// A frame whose checksum is wrong is never returned as a frame.
    #[test]
    fn a_corrupted_checksum_is_never_accepted(
        cmd in any::<u8>(),
        payload in prop::collection::vec(any::<u8>(), 0..64),
        flip in 1u8..=255,
    ) {
        let frame = Frame::new(cmd, &payload).unwrap();
        let mut buf = [0u8; 260];
        let n = frame.encode_into(&mut buf).unwrap();
        buf[n - 1] ^= flip; // any non-zero flip makes it wrong

        let mut r = FrameReader::new();
        for &b in &buf[..n] {
            if let Some(Ok(f)) = r.push(b) {
                prop_assert!(false, "accepted a frame with a bad checksum: {:?}", f);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// BCD
// ---------------------------------------------------------------------------------------

proptest! {
    #[test]
    fn bcd_round_trips(v in 0u8..=99) {
        prop_assert_eq!(bcd::decode(bcd::encode(v).unwrap()), Ok(v));
    }

    /// Decoding never yields a value a valid encoding could not have produced.
    #[test]
    fn bcd_decode_is_either_correct_or_rejects(b in any::<u8>()) {
        match bcd::decode(b) {
            Ok(v) => {
                prop_assert!(v <= 99);
                prop_assert_eq!(bcd::encode(v), Ok(b));
            }
            Err(_) => {
                // Rejected input must genuinely have a hex nibble.
                prop_assert!((b & 0x0F) > 9 || (b >> 4) > 9);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// HID report descriptors
// ---------------------------------------------------------------------------------------

proptest! {
    /// Descriptors come from the device. Parsing must terminate and never panic.
    #[test]
    fn parsing_arbitrary_descriptors_never_panics(
        bytes in prop::collection::vec(any::<u8>(), 0..1024)
    ) {
        let _ = ReportDescriptor::parse(&bytes);
    }

    /// Extraction is safe on any field a parse produces, from any payload.
    ///
    /// Note what is deliberately *not* asserted: that `logical_min <= logical_max`. A
    /// malformed descriptor may declare an inverted range, and the parser's job is to report
    /// what the bytes say, not to invent coherence the device did not provide. proptest
    /// refuted the stronger claim, which is the correct outcome -- the assertion was wrong,
    /// not the parser.
    #[test]
    fn extraction_is_safe_for_any_parsed_field(
        bytes in prop::collection::vec(any::<u8>(), 0..1024),
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        if let Ok(desc) = ReportDescriptor::parse(&bytes) {
            for f in desc.fields() {
                // Must not panic, and must refuse rather than read past the payload.
                if let Some(v) = f.extract(&payload) {
                    let bits = f.bit_size as usize;
                    prop_assert!(bits > 0 && bits <= 32, "extracted from a {}-bit field", bits);
                    // The value must be representable in the field's declared width.
                    if !f.is_signed() {
                        let limit = if bits == 64 { i64::MAX } else { (1i64 << bits) - 1 };
                        prop_assert!(v >= 0 && v <= limit, "{} does not fit in {} bits", v, bits);
                    }
                    // Reading past the end must be refused, not padded.
                    prop_assert!(f.bit_offset + bits <= payload.len() * 8);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Fan configuration
// ---------------------------------------------------------------------------------------

proptest! {
    /// Config files are hand-edited. Parsing must never panic on any text.
    #[test]
    fn parsing_arbitrary_config_never_panics(text in ".{0,512}") {
        let _ = fan::parse(&text);
    }

    /// Any curve that parses must be usable and monotonic across its whole range.
    #[test]
    fn a_parsed_curve_is_always_monotonic(text in ".{0,512}") {
        if let Ok(curve) = fan::parse(&text) {
            let mut last = 0u8;
            for dc in (-1000..2000).step_by(7) {
                let d = curve.duty_for(dc).percent();
                prop_assert!(d >= last, "duty fell from {} to {} at {}", last, d, dc);
                last = d;
            }
        }
    }

    /// Whatever the config said, a controller never emits an out-of-range duty.
    #[test]
    fn a_controller_never_emits_an_invalid_duty(
        text in ".{0,256}",
        temps in prop::collection::vec(-2000i32..3000, 1..64),
        min_duty in any::<u8>(),
        allow_stop in any::<bool>(),
    ) {
        if let Ok(curve) = fan::parse(&text) {
            let mut c = fan::FanController::new(curve, 3, min_duty, allow_stop);
            for t in temps {
                let d = c.update(t);
                prop_assert!(d.percent() <= 100, "duty {} exceeds 100", d.percent());
                if !allow_stop {
                    prop_assert!(d.is_spinning(), "fan set to {} with allow_stop = false", d);
                }
            }
        }
    }
}

mod unix_time {
    use argon_proto::ups::UpsTime;
    use proptest::prelude::*;

    /// Vectors computed independently with Python's `calendar.timegm`, not with this code, so
    /// the conversion is not being checked against itself.
    /// Year, month, day, hour, minute, second.
    type Civil = (u16, u8, u8, u8, u8, u8);

    const VECTORS: [(u64, Civil); 6] = [
        (946_684_800, (2000, 1, 1, 0, 0, 0)),
        (951_825_600, (2000, 2, 29, 12, 0, 0)),
        (1_789_651_755, (2026, 9, 17, 13, 29, 15)),
        (1_798_761_599, (2026, 12, 31, 23, 59, 59)),
        (1_835_417_228, (2028, 2, 29, 6, 7, 8)),
        (4_102_444_799, (2099, 12, 31, 23, 59, 59)),
    ];

    #[test]
    fn known_instants_convert_both_ways() {
        for (secs, (year, month, day, hour, minute, second)) in VECTORS {
            let t = UpsTime::from_unix_seconds(secs).expect("in range");
            assert_eq!(
                (t.year, t.month, t.day, t.hour, t.minute, t.second),
                (year, month, day, hour, minute, Some(second)),
                "from {secs}"
            );
            assert_eq!(t.to_unix_seconds(), Some(secs), "back to {secs}");
        }
    }

    #[test]
    fn years_the_device_cannot_hold_are_refused() {
        assert_eq!(UpsTime::from_unix_seconds(946_684_799), None, "1999-12-31");
        assert_eq!(
            UpsTime::from_unix_seconds(4_102_444_800),
            None,
            "2100-01-01"
        );
    }

    proptest! {
        #[test]
        fn every_representable_second_round_trips(secs in 946_684_800u64..=4_102_444_799) {
            let t = UpsTime::from_unix_seconds(secs).unwrap();
            prop_assert!(t.is_plausible());
            prop_assert_eq!(t.to_unix_seconds(), Some(secs));
            // And through the wire encoding the device actually receives.
            let wire = t.encode_clock().unwrap();
            let back = UpsTime::decode_clock(&wire).unwrap();
            prop_assert_eq!(back.to_unix_seconds(), Some(secs));
        }
    }
}
