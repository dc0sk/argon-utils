// SPDX-License-Identifier: GPL-3.0-or-later
//! The Argon ONE-family MCU: fan, power mode, power cut.

use argon_hal::i2c::I2cBus;
use argon_hal::{Error, Result};
use argon_proto::fan::FanDuty;

/// Documented I2C address (`ARGON-MCU-ADDR`).
pub const ADDR: u8 = 0x1a;

/// Which protocol the MCU speaks.
///
/// **There is no safe way to detect this.** An `SMBus` register read places the register number
/// on the bus as a write before the repeated start, so probing register `0x80` on legacy
/// firmware sets the fan to full — the read is as destructive as the write. The dialect is
/// therefore configuration, never detection, and defaults to [`Dialect::Legacy`]. See
/// ADR-0002.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Dialect {
    /// The documented single-byte protocol: one byte, the duty. Observed on the ONE V1's MCU
    /// (`ONE-V1-MCU-LEGACY`), and the default because it is the one that cannot damage a case
    /// speaking the other: a register read pins a legacy fan at full.
    #[default]
    Legacy,
    /// The register protocol: register `0x80` holds the duty, `[0x80, duty]` sets it.
    /// Observed on the ONE V3's RP2040 (`ONE-V3-MCU-REGISTER`), from the vendor daemon's own bus
    /// traffic. Reached **only** by configuration (`[mcu] dialect = "register"`), never by
    /// detection: telling the two apart takes a transaction that is harmless on one and pins
    /// the other's fan at full.
    Register,
}

impl Dialect {
    /// The dialect a configuration value names, or `None` for anything else -- including
    /// `"auto"`, which does not exist because there is nothing safe to detect with.
    #[must_use]
    pub fn from_config(value: &str) -> Option<Self> {
        match value {
            "legacy" => Some(Self::Legacy),
            "register" => Some(Self::Register),
            _ => None,
        }
    }
}

/// Power-on behaviour (`ARGON-MCU-L-MODE1`, `ARGON-MCU-L-MODE2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    /// A button press is required to power on after shutdown or power loss.
    RequiresButton,
    /// Power flows to the Pi without a button press.
    AlwaysOn,
}

/// The MCU, driven over some I2C transport.
pub struct Mcu<B: I2cBus> {
    bus: B,
    dialect: Dialect,
    /// What we last told the fan to do.
    ///
    /// Shadow state rather than a read-back: the legacy protocol cannot report its duty, and
    /// reading it in the register protocol is the hazardous transaction. We are the only
    /// writer, so what we last sent is what it is.
    shadow_duty: Option<FanDuty>,
}

impl<B: I2cBus> Mcu<B> {
    /// Binds to an MCU on the given bus, using the given dialect.
    pub const fn new(bus: B, dialect: Dialect) -> Self {
        Self {
            bus,
            dialect,
            shadow_duty: None,
        }
    }

    /// The duty last written, if any.
    pub const fn duty(&self) -> Option<FanDuty> {
        self.shadow_duty
    }

    /// Whether the MCU acknowledges its address.
    ///
    /// Uses an `SMBus` quick-write, which transfers no data byte and so cannot be misread as a
    /// command by any firmware generation.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    pub fn is_present(&mut self) -> Result<bool> {
        self.bus.probe(ADDR)
    }

    /// Sets the fan duty.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn set_fan(&mut self, duty: FanDuty) -> Result<()> {
        match self.dialect {
            Dialect::Legacy => self.bus.write(ADDR, &[duty.as_mcu_byte()])?,
            Dialect::Register => self.bus.write(ADDR, &[0x80, duty.as_mcu_byte()])?,
        }
        self.shadow_duty = Some(duty);
        Ok(())
    }

    /// Selects the power-on behaviour.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn set_power_mode(&mut self, mode: PowerMode) -> Result<()> {
        let byte = match mode {
            PowerMode::RequiresButton => 0xFD,
            PowerMode::AlwaysOn => 0xFE,
        };
        self.bus.write(ADDR, &[byte])
    }

    /// Arms the MCU's power cut (`ARGON-MCU-L-PWRCUT`).
    ///
    /// The MCU then watches UART TX voltage and cuts power when it falls. **Destructive**:
    /// combined with the `POWER_OFF_ON_HALT` and `WAKE_ON_GPIO` settings commonly present in
    /// a Pi 5 bootloader EEPROM, this can produce a machine that halts and cannot be woken
    /// from the case button. Callers must gate this on
    /// [`Mode::allows_destructive`](argon_hal::mode::Mode::allows_destructive) and an
    /// explicit confirmation, and the recovery procedure must be documented before it is
    /// ever called on real hardware.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if the transport forbids writes.
    pub fn arm_power_cut(&mut self) -> Result<()> {
        self.bus.write(ADDR, &[0xFF])
    }

    /// Always fails. The bootloader opcode is never emitted.
    ///
    /// `0xBB` puts the MCU into its firmware bootloader, and the *exit* sequence is unknown —
    /// a failed flash can leave the device unrecoverable. The opcode is documented in
    /// `docs/protocol/FACTS.md` so that a future contributor recognises it as hazardous
    /// rather than as an unimplemented feature. This function exists to be the place that
    /// refusal is written down.
    ///
    /// # Errors
    ///
    /// Always.
    pub fn enter_bootloader(&mut self) -> Result<()> {
        Err(Error::WriteBlocked {
            what: format!("i2c 0x{ADDR:02x} <- bb"),
            reason: "argon-utils never enters the MCU bootloader: the exit sequence is \
                     unknown and a failed flash can be unrecoverable",
        })
    }

    /// The underlying transport, for inspection in tests and dry-run reporting.
    pub const fn bus(&self) -> &B {
        &self.bus
    }
}
