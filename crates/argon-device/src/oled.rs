// SPDX-License-Identifier: GPL-3.0-or-later
//! The SSD1306 OLED, driven over some I2C transport.
//!
//! The command bytes and frame layout come from `argon_proto::oled`, which is written from
//! the Solomon Systech datasheet. This module only decides what order to send them in and how
//! to split a frame into I2C transactions.

use argon_hal::Result;
use argon_hal::i2c::I2cBus;
use argon_proto::oled::{self, CONTROL_COMMAND, CONTROL_DATA, FrameBuffer, WIDTH};

/// Bytes of pixel data per I2C transaction.
///
/// One page (128 bytes) per write. The panel accepts one continuous stream, but a transfer of
/// 1025 bytes is at the mercy of every layer's buffer limit on the way down, and splitting
/// costs nothing: in horizontal addressing mode the panel's pointer simply carries on where
/// the previous write stopped.
pub const DATA_CHUNK: usize = WIDTH;

/// An SSD1306 panel on a bus.
pub struct Oled<B: I2cBus> {
    bus: B,
    rotated_180: bool,
}

impl<B: I2cBus> Oled<B> {
    /// Binds to a panel. Nothing is sent until [`Oled::init`].
    pub const fn new(bus: B, rotated_180: bool) -> Self {
        Self { bus, rotated_180 }
    }

    /// Whether the panel acknowledges its address, without sending it any data.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    pub fn is_present(&mut self) -> Result<bool> {
        self.bus.probe(oled::ADDR)
    }

    /// Runs the power-on initialisation and sets the orientation.
    ///
    /// Orientation goes last, after the datasheet sequence, and before any frame is flushed —
    /// segment re-map only affects data written after it.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn init(&mut self) -> Result<()> {
        self.command(&oled::init_sequence())?;
        self.command(&oled::orientation(self.rotated_180))
    }

    /// Sends a whole frame.
    ///
    /// Sets the address window first. In horizontal addressing mode the panel's pointers
    /// advance as data arrives, so a flush that trusted them to still be at the origin would
    /// tear the image the second time it ran.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn flush(&mut self, frame: &FrameBuffer) -> Result<()> {
        self.command(&oled::full_frame_window())?;
        let mut tx = [0u8; DATA_CHUNK + 1];
        tx[0] = CONTROL_DATA;
        for chunk in frame.as_bytes().chunks(DATA_CHUNK) {
            tx[1..=chunk.len()].copy_from_slice(chunk);
            self.bus.write(oled::ADDR, &tx[..=chunk.len()])?;
        }
        Ok(())
    }

    /// Sets the brightness, 0-255 (SSD1306 datasheet section 10.1.7).
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn set_contrast(&mut self, level: u8) -> Result<()> {
        self.command(&oled::contrast(level))
    }

    /// Blanks the panel and switches it off.
    ///
    /// Clears the memory as well as switching off: a static image left on an OLED burns in,
    /// and switching the display on again later should not bring back whatever was there.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn off(&mut self) -> Result<()> {
        self.flush(&FrameBuffer::new())?;
        self.command(&[oled::power(false)])
    }

    /// The underlying transport, for inspection in tests and dry runs.
    pub const fn bus(&self) -> &B {
        &self.bus
    }

    fn command(&mut self, bytes: &[u8]) -> Result<()> {
        let mut tx = Vec::with_capacity(bytes.len() + 1);
        tx.push(CONTROL_COMMAND);
        tx.extend_from_slice(bytes);
        self.bus.write(oled::ADDR, &tx)
    }
}

/// The T7 bring-up screen.
///
/// Designed so a single photograph answers every question the bring-up needs answered:
///
/// - **Is the orientation right?** The text reads correctly and the solid box is in the top
///   left corner. If the text is upside down or mirrored, run again with the other
///   orientation.
/// - **Is every pixel addressable?** A one-pixel border runs round the whole edge. A missing
///   or doubled edge means the panel is not the 128x64 SSD1306 the code assumes.
/// - **Is it an SSD1306 at all?** The SH1106 is a common lookalike at the same address with a
///   132-pixel memory and a two-column offset. On one, this screen appears shifted by two
///   pixels with garbage along an edge.
/// - **Do partial fills work?** A bar at 60%.
#[must_use]
pub fn test_pattern() -> FrameBuffer {
    let mut fb = FrameBuffer::new();

    // Border: every edge pixel.
    fb.rect(0, 0, WIDTH, 1, true);
    fb.rect(0, argon_proto::oled::HEIGHT - 1, WIDTH, 1, true);
    fb.rect(0, 0, 1, argon_proto::oled::HEIGHT, true);
    fb.rect(WIDTH - 1, 0, 1, argon_proto::oled::HEIGHT, true);

    // Orientation marker: a solid box in the top-left corner.
    fb.rect(3, 3, 7, 7, true);

    fb.draw_text(14, 4, "argon-utils T7");
    fb.draw_text(4, 16, "SSD1306 @ 0x3c");
    fb.draw_text(4, 26, "box = top left");
    fb.draw_text(4, 36, "bar = 60%");
    fb.bar(4, 48, 120, 11, 60);
    fb
}
