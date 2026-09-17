// SPDX-License-Identifier: GPL-3.0-or-later
//! Prometheus metrics for argon-utils.
//!
//! # What is deliberately absent
//!
//! There are no battery metrics. The UPS's HID interface is dormant on firmware 113, and its
//! serial protocol is held by the vendor's daemon on the development machine, so nothing here
//! can measure a battery today. Exporting `argon_ups_charge_ratio` as a zero, or as a stale
//! last-known value, would put a number on a dashboard that nobody could distinguish from a
//! real one — and an alert on a fabricated metric is worse than no alert.
//!
//! When the serial channel becomes available (task T4), the battery metrics are a small
//! addition. Until then their absence is the honest reading.
//!
//! # Not a separate process
//!
//! This is a library linked into the daemon, per ADR-0001. A standalone exporter would need
//! its own device access, making it a second reader on the UPS serial port and a second I2C
//! client — exactly the contention the safety architecture exists to prevent.

mod render;
mod server;

pub use render::{Snapshot, render};
pub use server::{Server, ServerError};
