// SPDX-License-Identifier: GPL-3.0-or-later
//! Device drivers for Argon40 hardware.
//!
//! Drivers are written against the transport traits in `argon-hal`, never against a concrete
//! device. That is what lets the safety policy be a property of the transport a driver is
//! handed rather than a rule each driver must remember.

pub mod mcu;
pub mod safety;
