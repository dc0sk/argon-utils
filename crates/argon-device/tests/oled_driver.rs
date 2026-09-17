// SPDX-License-Identifier: GPL-3.0-or-later
//! What the OLED driver actually puts on the bus, checked without a panel.

use argon_device::oled::{DATA_CHUNK, Oled, test_pattern};
use argon_hal::Result;
use argon_hal::i2c::{DryRun, I2cBus, ReadOnly};
use argon_proto::oled::{self, FrameBuffer};

/// A bus that goes nowhere; `DryRun` on top of it records the transactions.
struct NullBus;

impl I2cBus for NullBus {
    fn write(&mut self, _addr: u8, _data: &[u8]) -> Result<()> {
        Ok(())
    }
    fn probe(&mut self, _addr: u8) -> Result<bool> {
        Ok(true)
    }
    fn describe(&self) -> String {
        "null".into()
    }
}

fn recorded(f: impl FnOnce(&mut Oled<DryRun<NullBus>>)) -> Vec<(u8, Vec<u8>)> {
    let mut panel = Oled::new(DryRun::new(NullBus), false);
    f(&mut panel);
    panel.bus().writes().to_vec()
}

#[test]
fn every_transaction_goes_to_the_panel_address() {
    let writes = recorded(|p| {
        p.init().unwrap();
        p.flush(&test_pattern()).unwrap();
        p.off().unwrap();
    });
    assert!(!writes.is_empty());
    for (addr, _) in &writes {
        assert_eq!(*addr, oled::ADDR, "a transaction went to 0x{addr:02x}");
    }
}

#[test]
fn init_is_the_datasheet_sequence_then_orientation() {
    let writes = recorded(|p| p.init().unwrap());
    assert_eq!(writes.len(), 2);

    let mut expected = vec![oled::CONTROL_COMMAND];
    expected.extend_from_slice(&oled::init_sequence());
    assert_eq!(
        writes[0].1, expected,
        "first transaction must be the full init sequence"
    );
    assert_eq!(
        writes[1].1,
        vec![0x00, 0xA1, 0xC8],
        "then the normal orientation"
    );
}

#[test]
fn rotation_sends_the_other_remap() {
    let mut panel = Oled::new(DryRun::new(NullBus), true);
    panel.init().unwrap();
    assert_eq!(panel.bus().writes()[1].1, vec![0x00, 0xA0, 0xC0]);
}

#[test]
fn orientation_is_set_before_any_pixel_data() {
    // Segment re-map only affects data written after it (datasheet §10.1.6). Sending it after
    // a frame would leave that frame half-transformed until the next flush.
    let writes = recorded(|p| {
        p.init().unwrap();
        p.flush(&test_pattern()).unwrap();
    });
    let orient = writes
        .iter()
        .position(|(_, t)| t == &[0x00, 0xA1, 0xC8])
        .expect("orientation command");
    let first_data = writes
        .iter()
        .position(|(_, t)| t.first() == Some(&oled::CONTROL_DATA))
        .expect("pixel data");
    assert!(
        orient < first_data,
        "orientation at {orient}, first pixel data at {first_data}"
    );
}

#[test]
fn a_flush_sets_the_window_then_sends_exactly_one_frame_of_data() {
    let writes = recorded(|p| p.flush(&test_pattern()).unwrap());

    let mut window = vec![oled::CONTROL_COMMAND];
    window.extend_from_slice(&oled::full_frame_window());
    assert_eq!(
        writes[0].1, window,
        "a flush must reset the address window first"
    );

    let data: Vec<&Vec<u8>> = writes[1..].iter().map(|(_, t)| t).collect();
    for t in &data {
        assert_eq!(
            t[0],
            oled::CONTROL_DATA,
            "every data transaction needs the data control byte"
        );
        assert!(
            t.len() <= DATA_CHUNK + 1,
            "a transaction of {} bytes",
            t.len()
        );
    }
    let pixels: Vec<u8> = data.iter().flat_map(|t| t[1..].iter().copied()).collect();
    assert_eq!(
        pixels,
        test_pattern().as_bytes(),
        "the bytes sent are not the frame"
    );
}

#[test]
fn flushing_twice_resets_the_window_each_time() {
    // Without this the second flush starts wherever the first one finished and tears.
    let writes = recorded(|p| {
        p.flush(&test_pattern()).unwrap();
        p.flush(&test_pattern()).unwrap();
    });
    let mut window = vec![oled::CONTROL_COMMAND];
    window.extend_from_slice(&oled::full_frame_window());
    assert_eq!(writes.iter().filter(|(_, t)| *t == window).count(), 2);
}

#[test]
fn off_clears_the_memory_before_switching_off() {
    // Switching off alone would bring the old image back on the next power-on, and a static
    // image left in place is what burns an OLED in.
    let writes = recorded(|p| p.off().unwrap());
    let pixels: Vec<u8> = writes
        .iter()
        .filter(|(_, t)| t.first() == Some(&oled::CONTROL_DATA))
        .flat_map(|(_, t)| t[1..].iter().copied())
        .collect();
    assert_eq!(pixels.len(), FrameBuffer::new().as_bytes().len());
    assert!(
        pixels.iter().all(|b| *b == 0),
        "off() did not blank the memory"
    );
    assert_eq!(
        writes.last().unwrap().1,
        vec![0x00, 0xAE],
        "off() must end with display-off"
    );
}

#[test]
fn a_read_only_transport_sends_nothing() {
    let mut panel = Oled::new(ReadOnly(NullBus), false);
    assert!(panel.init().is_err());
    assert!(panel.flush(&test_pattern()).is_err());
}

#[test]
fn the_test_pattern_has_a_full_border_and_a_corner_marker() {
    let fb = test_pattern();
    for x in 0..oled::WIDTH {
        assert!(
            fb.pixel(x, 0) && fb.pixel(x, oled::HEIGHT - 1),
            "border gap at column {x}"
        );
    }
    for y in 0..oled::HEIGHT {
        assert!(
            fb.pixel(0, y) && fb.pixel(oled::WIDTH - 1, y),
            "border gap at row {y}"
        );
    }
    // The orientation marker must be in the top left, and the matching spot in the top right
    // must be empty -- otherwise a horizontally mirrored panel would look identical in the
    // photo. (The bottom corners are covered by the progress bar, so vertical orientation is
    // read from the text instead.)
    assert!(fb.pixel(5, 5), "no marker in the top-left corner");
    assert!(
        !fb.pixel(oled::WIDTH - 6, 5),
        "the top-right corner mirrors the marker"
    );
}
