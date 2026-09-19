// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! USB HID report descriptor parsing, and the Power Device usages we care about.
//!
//! Facts: `ARGON-UPS-HID-CLASS`, `ARGON-UPS-HID-TABLE` (`observed`);
//! `ARGON-UPS-HID-SEMANTICS` (`documented`, from the USB-IF HID Power Device usage tables).
//!
//! Field positions are taken from the descriptor the device itself publishes, never from
//! hardcoded offsets. A firmware revision that moves or drops a report therefore degrades to
//! "field absent" instead of silently returning a plausible number from the wrong bits.

mod descriptor;
pub mod usage;

pub use descriptor::{Field, ItemKind, ParseError, ReportDescriptor};
