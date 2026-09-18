// SPDX-License-Identifier: GPL-3.0-or-later
//! The Cellwise CW2217 battery fuel gauge, as fitted to the Argon ONE UP.
//!
//! Facts: `CW2217-*` are `documented` in the chip's datasheet ([DS-CW2217] in
//! `docs/protocol/FACTS.md`); `ONEUP-0x64-IDENTITY`, `ONEUP-CURRENT-SIGN` and
//! `ONEUP-GAUGE-BURST` are `observed` on a ONE UP on 2026-09-18.
//!
//! Only reading is modelled. The registers that change the gauge's behaviour -- `CONFIG`
//! (sleep, restart) and the battery profile -- are the integrator's, and nothing in this
//! project writes them; they do not appear in [`READS`], which is the whole of what a driver
//! may ask the chip for.

use crate::ups::PowerSource;

/// The chip's fixed 7-bit I2C address (`CW2217-ADDR`, the BAAD variant).
pub const ADDR: u8 = 0x64;

/// What `VERSION` reads on every CW2217, in every mode (`CW2217-VERSION`).
pub const VERSION_VALUE: u8 = 0xA0;

/// Register addresses (`CW2217-*`).
pub mod reg {
    /// IC version, one byte, fixed [`super::VERSION_VALUE`].
    pub const VERSION: u8 = 0x00;
    /// Cell voltage, two bytes.
    pub const VCELL: u8 = 0x02;
    /// State of charge, two bytes: whole percent, then 1/256 %.
    pub const SOC: u8 = 0x04;
    /// Current, two bytes, signed.
    pub const CURRENT: u8 = 0x0E;
    /// Charge cycles, two bytes.
    pub const CYCLES: u8 = 0xA4;
    /// State of health, one byte, percent.
    pub const SOH: u8 = 0xA6;
}

/// Every read a driver may make: `(register, length)`. All read-only registers.
///
/// A register read puts the register number on the bus before the repeated start; on this chip
/// that byte only moves the register pointer (`CW2217-READ`). `TEMP` is left out: on the ONE UP
/// it holds its reset value and is not a measurement (`ONEUP-GAUGE-TEMP`).
pub const READS: &[(u8, usize)] = &[
    (reg::VERSION, 1),
    (reg::VCELL, 2),
    (reg::SOC, 2),
    (reg::CURRENT, 2),
    (reg::CYCLES, 2),
    (reg::SOH, 1),
];

/// Whether `(register, length)` is one of [`READS`].
#[must_use]
pub fn is_permitted_read(register: u8, len: usize) -> bool {
    READS.iter().any(|&(r, n)| r == register && n == len)
}

/// Cell voltage in microvolts: 14 bits, 312.5 µV each (`CW2217-VCELL`).
#[must_use]
pub fn vcell_microvolts(raw: [u8; 2]) -> u32 {
    let counts = u32::from(raw[0] & 0x3F) << 8 | u32::from(raw[1]);
    counts * 3_125 / 10
}

/// State of charge as whole percent, clamped to 100, and the 1/256 % remainder (`CW2217-SOC`).
#[must_use]
pub const fn soc(raw: [u8; 2]) -> (u8, u8) {
    let whole = if raw[0] > 100 { 100 } else { raw[0] };
    (whole, raw[1])
}

/// The current register as the signed count it is (`CW2217-CURRENT`). Positive is charging.
///
/// Not converted to amperes: that needs the board's sense resistor, which is `unknown` on the
/// ONE UP (`ONEUP-RSENSE`).
#[must_use]
pub const fn current_raw(raw: [u8; 2]) -> i16 {
    i16::from_be_bytes(raw)
}

/// Which way charge is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Into the battery.
    Charging,
    /// Out of it: the battery is supplying the machine.
    Discharging,
    /// Neither, within the noise: charged and resting on the charger.
    Idle,
}

/// How far from zero the current must be to count as flowing, in register counts.
///
/// Observed on a ONE UP: -5 to 0 at rest on the charger; -2092 to -3008 on battery with the
/// machine running; +2688 to +2875 charging (`ONEUP-CURRENT-SIGN`). 200 is 40 times the rest
/// noise and under a tenth of the smallest discharge seen. A judgement from one session, not
/// a documented figure.
pub const FLOW_THRESHOLD: i16 = 200;

/// Classifies a current reading.
#[must_use]
pub const fn flow(current: i16) -> Flow {
    if current <= -FLOW_THRESHOLD {
        Flow::Discharging
    } else if current >= FLOW_THRESHOLD {
        Flow::Charging
    } else {
        Flow::Idle
    }
}

/// Where the machine's power is coming from, for the battery policy.
///
/// **On battery means the battery is discharging**, whatever is plugged in. A charger too weak
/// for the load -- Argon recommends 45 W at least -- leaves the battery draining with a cable
/// attached, and for deciding whether to shut down before it runs out, that is on battery.
#[must_use]
pub const fn power_source(flow: Flow) -> PowerSource {
    match flow {
        Flow::Discharging => PowerSource::Battery,
        Flow::Charging | Flow::Idle => PowerSource::Mains,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_values_read_on_a_one_up() {
        // OBS-2026-09-18-one-up-survey: VCELL 36f8, SOC 6400, current f440 / 0b3b.
        assert_eq!(vcell_microvolts([0x36, 0xf8]), 4_397_500);
        assert_eq!(soc([0x64, 0x00]), (100, 0));
        assert_eq!(current_raw([0xf4, 0x40]), -3008);
        assert_eq!(current_raw([0x0b, 0x3b]), 2875);
    }

    #[test]
    fn the_two_unused_vcell_bits_are_ignored() {
        assert_eq!(
            vcell_microvolts([0xff, 0xff]),
            vcell_microvolts([0x3f, 0xff])
        );
    }

    #[test]
    fn soc_above_100_is_clamped() {
        assert_eq!(soc([0xff, 0x80]), (100, 0x80));
    }

    #[test]
    fn the_observed_readings_classify_as_seen() {
        for rest in [-5, -2, 0] {
            assert_eq!(flow(rest), Flow::Idle, "{rest}");
        }
        for out in [-2092, -3008] {
            assert_eq!(flow(out), Flow::Discharging, "{out}");
        }
        for into in [2688, 2875] {
            assert_eq!(flow(into), Flow::Charging, "{into}");
        }
    }

    #[test]
    fn only_a_discharge_is_on_battery() {
        assert_eq!(power_source(Flow::Discharging), PowerSource::Battery);
        assert_eq!(power_source(Flow::Charging), PowerSource::Mains);
        assert_eq!(power_source(Flow::Idle), PowerSource::Mains);
    }

    #[test]
    fn the_registers_that_change_the_gauge_are_not_readable_through_this() {
        // CONFIG (0x08) and the profile are never touched. Not even read: nothing needs them.
        assert!(!is_permitted_read(0x08, 1));
        assert!(!is_permitted_read(0x10, 1));
        // A permitted register at the wrong length is not permitted either.
        assert!(!is_permitted_read(reg::SOC, 3));
        assert!(is_permitted_read(reg::SOC, 2));
    }
}
