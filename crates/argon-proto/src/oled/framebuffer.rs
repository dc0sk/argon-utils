// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! A monochrome frame buffer laid out the way the SSD1306 stores pixels.

use super::font::{GLYPH_WIDTH, glyph};

/// Panel width in pixels.
pub const WIDTH: usize = 128;

/// Panel height in pixels.
pub const HEIGHT: usize = 64;

/// Pages, each eight pixel rows tall.
pub const PAGES: usize = HEIGHT / 8;

/// A 128x64 monochrome buffer in the panel's own page layout.
///
/// One byte holds eight vertically-adjacent pixels, least significant bit at the top. Storing
/// it this way means a flush is a straight copy: converting from a row-major buffer on every
/// frame would be pure overhead on a device whose whole point is being cheap to drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameBuffer {
    data: [u8; WIDTH * PAGES],
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameBuffer {
    /// A blank buffer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: [0; WIDTH * PAGES],
        }
    }

    /// The raw bytes, ready to send after a data control byte.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Clears every pixel.
    pub const fn clear(&mut self) {
        self.data = [0; WIDTH * PAGES];
    }

    /// Fills every pixel.
    pub const fn fill(&mut self) {
        self.data = [0xFF; WIDTH * PAGES];
    }

    /// Sets or clears one pixel.
    ///
    /// Coordinates outside the panel are ignored rather than wrapped or panicking. Drawing
    /// code routinely computes positions that fall off the edge, and a panel that silently
    /// clips is easier to write against than one that either aborts or draws in the wrong
    /// place.
    pub const fn set_pixel(&mut self, x: usize, y: usize, on: bool) {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let index = (y / 8) * WIDTH + x;
        let bit = 1u8 << (y % 8);
        if on {
            self.data[index] |= bit;
        } else {
            self.data[index] &= !bit;
        }
    }

    /// Reads one pixel. Out-of-range coordinates read as unset.
    #[must_use]
    pub const fn pixel(&self, x: usize, y: usize) -> bool {
        if x >= WIDTH || y >= HEIGHT {
            return false;
        }
        self.data[(y / 8) * WIDTH + x] & (1u8 << (y % 8)) != 0
    }

    /// Draws a filled rectangle.
    pub const fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, on: bool) {
        let mut dy = 0;
        while dy < h {
            let mut dx = 0;
            while dx < w {
                self.set_pixel(x + dx, y + dy, on);
                dx += 1;
            }
            dy += 1;
        }
    }

    /// Draws a horizontal progress bar with a one-pixel border, filled to `percent`.
    ///
    /// Integer arithmetic throughout: this crate is `no_std`, where `f32::round` does not
    /// exist, and a percentage is what every caller actually has — a battery level, a fan
    /// duty. Values above 100 clamp, so a device that occasionally reports 110% gets a full
    /// bar rather than a drawing that runs off the end of the panel.
    pub const fn bar(&mut self, x: usize, y: usize, w: usize, h: usize, percent: u8) {
        if w < 2 || h < 2 {
            return;
        }
        self.rect(x, y, w, 1, true);
        self.rect(x, y + h - 1, w, 1, true);
        self.rect(x, y, 1, h, true);
        self.rect(x + w - 1, y, 1, h, true);

        let inner = w - 2;
        let pct = if percent > 100 { 100 } else { percent } as usize;
        // Round to nearest rather than truncating, so a nearly-full bar does not read as one
        // pixel short of full.
        let filled = (inner * pct + 50) / 100;
        self.rect(x + 1, y + 1, filled, h - 2, true);
    }

    /// Draws one character, returning the width consumed including the trailing gap.
    pub fn draw_char(&mut self, x: usize, y: usize, c: char) -> usize {
        let columns = glyph(c);
        for (dx, column) in columns.iter().enumerate() {
            for dy in 0..8 {
                if column & (1 << dy) != 0 {
                    self.set_pixel(x + dx, y + dy, true);
                }
            }
        }
        GLYPH_WIDTH + 1
    }

    /// Draws a string, returning the width consumed.
    ///
    /// Stops at the right edge rather than wrapping: a status line that silently continues on
    /// the next row is harder to read than one that is visibly cut off.
    pub fn draw_text(&mut self, x: usize, y: usize, text: &str) -> usize {
        let mut cursor = x;
        for c in text.chars() {
            if cursor + GLYPH_WIDTH > WIDTH {
                break;
            }
            cursor += self.draw_char(cursor, y, c);
        }
        cursor - x
    }

    /// The width a string would occupy, without drawing it.
    #[must_use]
    pub fn text_width(text: &str) -> usize {
        text.chars().count() * (GLYPH_WIDTH + 1)
    }

    /// Draws text centred within a box.
    pub fn draw_text_centred(&mut self, x: usize, y: usize, box_width: usize, text: &str) {
        let width = Self::text_width(text);
        let offset = box_width.saturating_sub(width) / 2;
        self.draw_text(x + offset, y, text);
    }

    /// How many pixels are set. Used by tests to assert that drawing did something.
    #[must_use]
    pub fn lit_pixels(&self) -> u32 {
        self.data.iter().map(|b| b.count_ones()).sum()
    }
}
