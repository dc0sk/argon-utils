// SPDX-License-Identifier: GPL-3.0-or-later
//! Simulated Argon devices, for testing without hardware.
//!
//! Not published. This exists so the protocol and policy layers can be exercised in CI on a
//! machine with no Raspberry Pi and no Argon hardware attached.
//!
//! The UPS simulator runs over a **real PTY**, not a mocked trait. That matters: it exercises
//! the actual `serialport` code path, including partial reads, timeouts and frames split
//! across read boundaries. A mock that hands over whole frames would pass while the real
//! transport was broken in exactly the ways serial links break.

pub mod mcu;
pub mod ups;
