// SPDX-License-Identifier: GPL-3.0-or-later
//! SSD1306 OLED: command construction and a frame buffer.
//!
//! Written from the Solomon Systech SSD1306 datasheet (rev 1.1), which is the only available
//! source: the vendor's software never performs a full panel initialisation, so there is
//! nothing to copy even setting the licensing question aside. Fact `ARGON-OLED-INIT` is
//! `documented` for that reason.
//!
//! Pure: this module builds byte sequences and manipulates a buffer. Sending either is the
//! caller's problem.

mod font;
mod framebuffer;

pub use font::{GLYPH_HEIGHT, GLYPH_WIDTH, glyph};
pub use framebuffer::{FrameBuffer, HEIGHT, PAGES, WIDTH};

/// The panel's I2C address on Argon hardware (`ARGON-OLED-ADDR`).
pub const ADDR: u8 = 0x3c;

/// Control byte introducing a command stream (datasheet §8.1.5.2, Co=0 D/C#=0).
pub const CONTROL_COMMAND: u8 = 0x00;

/// Control byte introducing a data stream (Co=0 D/C#=1).
pub const CONTROL_DATA: u8 = 0x40;

/// How the display walks memory as bytes are written (datasheet §10.1.3, command `0x20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressingMode {
    /// Column advances, wrapping to the next page. What a full-frame flush wants.
    #[default]
    Horizontal = 0x00,
    /// Page advances, wrapping to the next column.
    Vertical = 0x01,
    /// Column advances within one page and does not wrap.
    Page = 0x02,
}

/// The power-on initialisation sequence.
///
/// Every value is from the datasheet, with the section given. The panel comes out of reset
/// with the charge pump disabled and the display off, so a sequence that merely writes pixels
/// produces a blank screen and no error — which is the usual reason a first OLED bring-up
/// appears to do nothing.
///
/// Returns commands only; the caller prefixes [`CONTROL_COMMAND`].
#[must_use]
pub fn init_sequence() -> [u8; 26] {
    [
        0xAE, // §10.1.12 display off while we reconfigure
        0xD5,
        0x80, // §10.1.16 clock: divide ratio 1, oscillator frequency 8
        0xA8,
        0x3F, // §10.1.10 multiplex ratio 64 (0x3F = 63, i.e. 64 rows)
        0xD3,
        0x00, // §10.1.15 display offset: none
        0x40, // §10.1.4 display start line 0
        0x8D,
        0x14, // §10.1.18 charge pump ON -- without this the panel stays dark
        0x20,
        AddressingMode::Horizontal as u8, // §10.1.3 memory addressing mode
        0xA1,                             // §10.1.6 segment re-map: column 127 maps to SEG0
        0xC8, // §10.1.14 COM scan direction remapped, so the image is not upside down
        0xDA,
        0x12, // §10.1.17 COM pins: alternative configuration, no left/right remap
        0x81,
        0xCF, // §10.1.7 contrast
        0xD9,
        0xF1, // §10.1.19 pre-charge period: phase 1 = 1, phase 2 = 15
        0xDB,
        0x40, // §10.1.20 VCOMH deselect level
        0xA4, // §10.1.8 resume from RAM, rather than forcing all pixels on
        0xA6, // §10.1.9 normal, not inverted
        0x2E, // §10.2.3 deactivate scrolling
        0xAF, // §10.1.12 display on
    ]
}

/// Commands to address the whole panel before a full-frame data write.
///
/// Required before every flush in horizontal addressing mode: the column and page pointers
/// advance as data is written, so leaving them where the last flush finished would tear the
/// image.
#[must_use]
pub const fn full_frame_window() -> [u8; 6] {
    [
        0x21,
        0,
        LAST_COLUMN, // §10.1.1 column address range
        0x22,
        0,
        LAST_PAGE, // §10.1.2 page address range
    ]
}

/// Highest addressable column, as the panel wants it on the wire.
const LAST_COLUMN: u8 = 127;

/// Highest addressable page.
const LAST_PAGE: u8 = 7;

// The wire constants above and the buffer dimensions must describe the same panel. Keeping
// them separate leaves the command bytes free of casts; checking them here stops the two
// drifting apart if the buffer is ever resized for a 128x32 part.
const _: () = assert!(LAST_COLUMN as usize == WIDTH - 1);
const _: () = assert!(LAST_PAGE as usize == PAGES - 1);

/// Turns the display on or off without losing its contents (§10.1.12).
#[must_use]
pub const fn power(on: bool) -> u8 {
    if on { 0xAF } else { 0xAE }
}

/// Sets contrast, 0–255 (§10.1.7).
#[must_use]
pub const fn contrast(level: u8) -> [u8; 2] {
    [0x81, level]
}

/// Inverts the display, or returns it to normal (§10.1.9).
#[must_use]
pub const fn invert(inverted: bool) -> u8 {
    if inverted { 0xA7 } else { 0xA6 }
}
