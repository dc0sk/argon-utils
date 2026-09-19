// SPDX-License-Identifier: GPL-3.0-or-later OR MPL-2.0
//! Usage page and usage constants for HID Power Devices.
//!
//! From the USB-IF *Usage Tables for HID Power Devices*. These are `documented` facts: the
//! device publishes usage numbers, and the specification says what they mean.
//!
//! Only usages this project actually reads are named. Notably absent are the writable
//! Power-Device items `0x57` and `0x55` on report `0x12`/`0x13`: their semantics are not
//! confirmed, they are plausibly delay-before-shutdown and delay-before-startup, and writing
//! them could power the host off. `argon-utils` does not expose them. See
//! `ARGON-UPS-HID-DANGER` in `docs/protocol/FACTS.md`.

/// Power Device usage page.
pub const PAGE_POWER_DEVICE: u16 = 0x84;
/// Battery System usage page.
pub const PAGE_BATTERY_SYSTEM: u16 = 0x85;

// ---- Battery System page (0x85) ----

/// Remaining capacity as a percentage of full charge, 0..=100.
pub const REMAINING_CAPACITY_LIMIT: u16 = 0x29;
/// A rate value; on this device a 16-bit field with range 120..=1380.
pub const AT_RATE: u16 = 0x2a;
/// Whether capacity is reported in percent or in mAh.
pub const CAPACITY_MODE: u16 = 0x2c;
/// Battery is charging.
pub const CHARGING: u16 = 0x44;
/// Battery is discharging.
pub const DISCHARGING: u16 = 0x45;
/// Mains power is present.
pub const AC_PRESENT: u16 = 0x42;
/// A battery is installed.
pub const BATTERY_PRESENT: u16 = 0x43;
/// Battery needs replacement.
pub const NEED_REPLACEMENT: u16 = 0x4b;
/// Battery is fully charged.
pub const FULLY_CHARGED: u16 = 0x46;
/// Battery is fully discharged.
pub const FULLY_DISCHARGED: u16 = 0x47;
/// State of charge relative to full charge capacity, as a percentage.
pub const RELATIVE_STATE_OF_CHARGE: u16 = 0x66;
/// Remaining capacity.
pub const REMAINING_CAPACITY: u16 = 0x67;
/// Capacity when fully charged.
pub const FULL_CHARGE_CAPACITY: u16 = 0x68;
/// Predicted time until the battery is empty.
pub const RUN_TIME_TO_EMPTY: u16 = 0x69;
/// An averaged time value.
pub const AVERAGE_TIME: u16 = 0x6a;
/// Capacity as designed.
pub const DESIGN_CAPACITY: u16 = 0x83;

// ---- Power Device page (0x84) ----

/// Configured nominal voltage.
pub const CONFIG_VOLTAGE: u16 = 0x30;
/// The device has been asked to shut down.
pub const SHUTDOWN_REQUESTED: u16 = 0x68;
/// Shutdown is imminent.
pub const SHUTDOWN_IMMINENT: u16 = 0x69;
