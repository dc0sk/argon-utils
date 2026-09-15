// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden tests against the report descriptor captured from a real Argon PWR UPS.
//!
//! Evidence: `docs/protocol/captures/OBS-2026-09-15-ups-hid-descriptor.md`.
//! This is the one place the parser is checked against a device rather than against a
//! descriptor we wrote ourselves, so it is the test that would catch a parser that only
//! works on its author's assumptions.

use argon_proto::hid::{ItemKind, ReportDescriptor, usage};

/// The descriptor as published by the device, committed as evidence.
const DESCRIPTOR: &[u8] =
    include_bytes!("../../../docs/protocol/captures/ups-hid-report-descriptor.bin");

fn parsed() -> ReportDescriptor {
    ReportDescriptor::parse(DESCRIPTOR).expect("the captured descriptor must parse")
}

#[test]
fn descriptor_is_the_expected_capture() {
    // Guards against the fixture being replaced without the tests being revisited.
    assert_eq!(DESCRIPTOR.len(), 416);
    assert_eq!(&DESCRIPTOR[..6], &[0x05, 0x84, 0x09, 0x04, 0xa1, 0x01]);
}

#[test]
fn it_is_a_power_device() {
    let d = parsed();
    assert!(
        d.fields()
            .iter()
            .any(|f| f.usage_page == usage::PAGE_POWER_DEVICE),
        "no Power Device page fields"
    );
    assert!(
        d.fields()
            .iter()
            .any(|f| f.usage_page == usage::PAGE_BATTERY_SYSTEM),
        "no Battery System page fields"
    );
}

#[test]
fn charge_percentage_is_readable_as_an_input_report() {
    // This is the field the whole telemetry path depends on. It must be an Input report,
    // because that is what a plain read() on hidraw delivers -- no ioctl, no libusb, and
    // therefore no interface claim that could detach cdc_acm and take the serial port down.
    let d = parsed();
    let f = d
        .find(
            ItemKind::Input,
            usage::PAGE_BATTERY_SYSTEM,
            usage::RELATIVE_STATE_OF_CHARGE,
        )
        .expect("RelativeStateOfCharge must be an Input field");

    assert_eq!(f.report_id, 0x0c);
    assert_eq!(f.bit_size, 8);
    assert_eq!(f.bit_offset, 0);
    assert_eq!((f.logical_min, f.logical_max), (0, 100));
    assert!(!f.is_signed());
}

#[test]
fn present_status_bits_are_where_the_device_says() {
    let d = parsed();
    // Report 0x07 packs the status flags as single bits, in declaration order.
    for (usage_id, expected_offset) in [
        (usage::CHARGING, 0),
        (usage::DISCHARGING, 1),
        (usage::AC_PRESENT, 4),
        (usage::BATTERY_PRESENT, 5),
        (usage::NEED_REPLACEMENT, 6),
        (usage::FULLY_CHARGED, 8),
        (usage::FULLY_DISCHARGED, 9),
    ] {
        let f = d
            .find(ItemKind::Input, usage::PAGE_BATTERY_SYSTEM, usage_id)
            .unwrap_or_else(|| panic!("usage {usage_id:#x} missing from PresentStatus"));
        assert_eq!(
            f.report_id, 0x07,
            "usage {usage_id:#x} on unexpected report"
        );
        assert_eq!(f.bit_size, 1, "usage {usage_id:#x} is not a single bit");
        assert_eq!(
            f.bit_offset, expected_offset,
            "usage {usage_id:#x} at wrong bit"
        );
    }
}

#[test]
fn runtime_and_capacity_are_present_and_sixteen_bit() {
    let d = parsed();
    for (usage_id, report_id) in [
        (usage::RUN_TIME_TO_EMPTY, 0x1c),
        (usage::FULL_CHARGE_CAPACITY, 0x0d),
    ] {
        let f = d
            .find(ItemKind::Input, usage::PAGE_BATTERY_SYSTEM, usage_id)
            .unwrap_or_else(|| panic!("usage {usage_id:#x} missing"));
        assert_eq!(f.report_id, report_id);
        assert_eq!(f.bit_size, 16);
    }
}

#[test]
fn the_low_battery_threshold_is_writable_and_volatile() {
    // Both halves matter. Writable means the threshold can live in the device, surviving our
    // daemon dying. Volatile means the descriptor does NOT promise it survives a UPS power
    // cycle -- so the daemon must re-assert it on every reconnect rather than assume.
    let d = parsed();
    let f = d
        .find(
            ItemKind::Feature,
            usage::PAGE_BATTERY_SYSTEM,
            usage::REMAINING_CAPACITY_LIMIT,
        )
        .expect("RemainingCapacityLimit must be a Feature field");
    assert_eq!(f.report_id, 0x11);
    assert!(!f.is_constant(), "expected a writable (non-constant) field");
    assert!(
        f.is_volatile(),
        "expected the field to be declared volatile"
    );
}

#[test]
fn signed_sixteen_bit_ranges_are_decoded_as_signed() {
    // Reports 0x12 and 0x13 declare logical min 0x8000 in two bytes, which is -32768. If the
    // parser failed to sign-extend, these would read as 32768 and the fields would look
    // unsigned. We never write these, but getting their sign wrong would mean the parser is
    // wrong generally.
    let d = parsed();
    let signed: Vec<_> = d
        .fields()
        .iter()
        .filter(|f| f.report_id == 0x12 || f.report_id == 0x13)
        .collect();
    assert!(!signed.is_empty(), "reports 0x12/0x13 missing");
    for f in signed {
        assert_eq!(
            f.logical_min, -32768,
            "report {:#x} min not sign-extended",
            f.report_id
        );
        assert_eq!(f.logical_max, 32767);
        assert!(f.is_signed());
    }
}

#[test]
fn extracting_a_percentage_from_a_synthetic_report() {
    let d = parsed();
    let f = d
        .find(
            ItemKind::Input,
            usage::PAGE_BATTERY_SYSTEM,
            usage::RELATIVE_STATE_OF_CHARGE,
        )
        .unwrap();
    // Payload excludes the report-ID byte.
    assert_eq!(f.extract(&[93]), Some(93));
    assert_eq!(f.extract(&[100]), Some(100));
    // A truncated report yields nothing rather than a value from bits never received.
    assert_eq!(f.extract(&[]), None);
}

#[test]
fn a_truncated_descriptor_is_rejected_not_guessed() {
    // Cut mid-item: the last item's data runs off the end.
    let truncated = &DESCRIPTOR[..DESCRIPTOR.len() - 1];
    // Either it parses (if the cut landed on an item boundary) or it errors -- but it must
    // never panic, and it must never invent a field from bytes that are not there.
    if let Ok(d) = ReportDescriptor::parse(truncated) {
        assert!(d.fields().len() <= parsed().fields().len());
    }
}

#[test]
fn parser_terminates_on_arbitrary_bytes() {
    // Cheap stand-in for the fuzz target: the parser must make progress and stop on any
    // input, since descriptors come from a device we do not control.
    for seed in 0u16..512 {
        let junk: Vec<u8> = (0..64u16)
            .map(|i| (i.wrapping_mul(seed) & 0xFF) as u8)
            .collect();
        let _ = ReportDescriptor::parse(&junk);
    }
}
