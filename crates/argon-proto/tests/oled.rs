// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! SSD1306 command construction and frame buffer behaviour.
//!
//! The init sequence is a golden test against the datasheet. It cannot prove the panel lights
//! up — only hardware can do that, which is task T7 — but it can prove the bytes are the ones
//! the datasheet specifies, and that a later edit did not quietly drop one.

use argon_proto::oled::{
    self, FrameBuffer, GLYPH_HEIGHT, GLYPH_WIDTH, HEIGHT, PAGES, WIDTH, glyph,
};

#[test]
fn the_init_sequence_matches_the_datasheet() {
    let init = oled::init_sequence();
    assert_eq!(
        init,
        [
            0xAE, 0xD5, 0x80, 0xA8, 0x3F, 0xD3, 0x00, 0x40, 0x8D, 0x14, 0x20, 0x00, 0xA1, 0xC8,
            0xDA, 0x12, 0x81, 0xCF, 0xD9, 0xF1, 0xDB, 0x40, 0xA4, 0xA6, 0x2E, 0xAF,
        ]
    );
}

#[test]
fn the_charge_pump_is_enabled_before_the_display_is_turned_on() {
    // The single most common reason a first OLED bring-up shows nothing: the panel powers up
    // with its charge pump off, so without 0x8D 0x14 every pixel write succeeds and the
    // screen stays dark. Order matters too -- enabling it after display-on wastes a frame.
    let init = oled::init_sequence();
    let pump = init
        .windows(2)
        .position(|w| w == [0x8D, 0x14])
        .expect("charge pump command");
    let on = init
        .iter()
        .position(|b| *b == 0xAF)
        .expect("display on command");
    assert!(
        pump < on,
        "charge pump enabled at {pump}, after display-on at {on}"
    );
}

#[test]
fn the_display_is_off_while_being_reconfigured() {
    let init = oled::init_sequence();
    assert_eq!(
        init[0], 0xAE,
        "the sequence should start by turning the display off"
    );
    assert_eq!(*init.last().unwrap(), 0xAF, "and end by turning it on");
}

#[test]
fn the_full_frame_window_covers_the_whole_panel() {
    // Required before every flush: in horizontal addressing the pointers advance as data is
    // written, so leaving them where the last flush finished tears the image.
    assert_eq!(oled::full_frame_window(), [0x21, 0, 127, 0x22, 0, 7]);
}

#[test]
fn control_bytes_are_the_documented_ones() {
    assert_eq!(oled::CONTROL_COMMAND, 0x00);
    assert_eq!(oled::CONTROL_DATA, 0x40);
    assert_eq!(oled::ADDR, 0x3c);
}

#[test]
fn simple_commands_round_trip() {
    assert_eq!(oled::power(true), 0xAF);
    assert_eq!(oled::power(false), 0xAE);
    assert_eq!(oled::invert(true), 0xA7);
    assert_eq!(oled::invert(false), 0xA6);
    assert_eq!(oled::contrast(0x7F), [0x81, 0x7F]);
}

#[test]
fn the_buffer_is_the_panel_size() {
    assert_eq!(WIDTH, 128);
    assert_eq!(HEIGHT, 64);
    assert_eq!(PAGES, 8);
    assert_eq!(FrameBuffer::new().as_bytes().len(), 1024);
}

#[test]
fn pixels_round_trip_at_every_position() {
    let mut fb = FrameBuffer::new();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            fb.set_pixel(x, y, true);
            assert!(fb.pixel(x, y), "pixel ({x},{y}) did not set");
            fb.set_pixel(x, y, false);
            assert!(!fb.pixel(x, y), "pixel ({x},{y}) did not clear");
        }
    }
    assert_eq!(fb.lit_pixels(), 0);
}

#[test]
fn pixels_are_stored_in_the_panels_own_page_layout() {
    // One byte holds eight vertically-adjacent pixels, least significant bit at the top. If
    // this were row-major, a flush would need a transform on every frame.
    let mut fb = FrameBuffer::new();
    fb.set_pixel(0, 0, true);
    assert_eq!(
        fb.as_bytes()[0],
        0x01,
        "pixel (0,0) should be bit 0 of byte 0"
    );

    fb.clear();
    fb.set_pixel(0, 7, true);
    assert_eq!(
        fb.as_bytes()[0],
        0x80,
        "pixel (0,7) should be bit 7 of byte 0"
    );

    fb.clear();
    fb.set_pixel(0, 8, true);
    assert_eq!(
        fb.as_bytes()[WIDTH],
        0x01,
        "pixel (0,8) should start page 1"
    );
}

#[test]
fn out_of_range_pixels_are_clipped_not_wrapped() {
    // Drawing code routinely computes positions off the edge. Wrapping would put pixels in
    // the wrong place, which is far harder to debug than nothing appearing.
    let mut fb = FrameBuffer::new();
    fb.set_pixel(WIDTH, 0, true);
    fb.set_pixel(0, HEIGHT, true);
    fb.set_pixel(usize::MAX, usize::MAX, true);
    assert_eq!(fb.lit_pixels(), 0, "an off-panel pixel landed somewhere");
    assert!(!fb.pixel(0, 0), "an off-panel pixel wrapped to the origin");
}

#[test]
fn clear_and_fill_are_complete() {
    let mut fb = FrameBuffer::new();
    fb.fill();
    assert_eq!(fb.lit_pixels(), u32::try_from(WIDTH * HEIGHT).unwrap());
    fb.clear();
    assert_eq!(fb.lit_pixels(), 0);
}

#[test]
fn a_bar_is_bordered_and_fills_proportionally() {
    let mut fb = FrameBuffer::new();
    fb.bar(0, 0, 102, 8, 0);
    let empty = fb.lit_pixels();

    fb.clear();
    fb.bar(0, 0, 102, 8, 50);
    let half = fb.lit_pixels();

    fb.clear();
    fb.bar(0, 0, 102, 8, 100);
    let full = fb.lit_pixels();

    assert!(empty > 0, "an empty bar should still draw its border");
    assert!(
        half > empty,
        "a half bar should be fuller than an empty one"
    );
    assert!(full > half, "a full bar should be fuller than a half one");
}

#[test]
fn a_bar_clamps_rather_than_overflowing_the_panel() {
    // A device reporting 110% should give a full bar, not a drawing that runs off the edge.
    let mut fb = FrameBuffer::new();
    fb.bar(0, 0, 102, 8, 255);
    let over = fb.lit_pixels();

    let mut fb2 = FrameBuffer::new();
    fb2.bar(0, 0, 102, 8, 100);
    assert_eq!(over, fb2.lit_pixels(), "255% drew differently from 100%");
}

#[test]
fn text_draws_and_advances() {
    let mut fb = FrameBuffer::new();
    let width = fb.draw_text(0, 0, "Argon");
    assert_eq!(width, FrameBuffer::text_width("Argon"));
    assert!(fb.lit_pixels() > 0, "text drew nothing");
}

#[test]
fn text_is_cut_off_at_the_edge_rather_than_wrapping() {
    // A status line that silently continues on the next row is harder to read than one that
    // is visibly truncated.
    let mut fb = FrameBuffer::new();
    let long = "X".repeat(100);
    fb.draw_text(0, 0, &long);
    // Nothing should have been drawn below the first glyph row.
    for y in GLYPH_HEIGHT + 1..HEIGHT {
        for x in 0..WIDTH {
            assert!(!fb.pixel(x, y), "text wrapped to ({x},{y})");
        }
    }
}

#[test]
fn every_printable_character_has_a_distinct_glyph_or_a_visible_box() {
    // A missing glyph must be visible, not blank: a silently skipped character produces a
    // layout that mysteriously does not line up.
    for code in 0x21..=0x7Eu8 {
        let g = glyph(code as char);
        assert!(
            g.iter().any(|c| *c != 0),
            "{:?} renders as blank",
            code as char
        );
    }
    // Space is the one glyph that should be empty.
    assert!(glyph(' ').iter().all(|c| *c == 0));
}

#[test]
fn non_ascii_renders_as_something_rather_than_vanishing() {
    let g = glyph('\u{00e9}');
    assert!(
        g.iter().any(|c| *c != 0),
        "a non-ASCII character rendered as blank"
    );
    assert_eq!(g.len(), GLYPH_WIDTH);
}

#[test]
fn a_realistic_status_screen_fits() {
    let mut fb = FrameBuffer::new();
    fb.draw_text(0, 0, "CPU 49.6C");
    fb.draw_text(0, 10, "FAN 29% 2948rpm");
    fb.draw_text(0, 20, "BAT 89% charging");
    fb.bar(0, 32, 102, 8, 89);
    assert!(
        fb.lit_pixels() > 100,
        "the status screen drew almost nothing"
    );
    assert_eq!(fb.as_bytes().len(), 1024);
}
