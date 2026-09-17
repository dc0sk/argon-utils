// SPDX-License-Identifier: GPL-3.0-or-later
//! Device drivers for Argon40 hardware.
//!
//! Drivers are written against the transport traits in `argon-hal`, never against a concrete
//! device. That is what lets the safety policy be a property of the transport a driver is
//! handed rather than a rule each driver must remember.

pub mod config;
pub mod fan;
pub mod fan_control;
pub mod mcu;
pub mod oled;
pub mod safety;
pub mod ups;
