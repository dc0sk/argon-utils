// SPDX-License-Identifier: GPL-3.0-or-later
//! A simulated Argon ONE-family MCU, modelling **both** protocol dialects.
//!
//! The point of this module is the hazard. Firmware implementing only the documented
//! single-byte protocol has no concept of registers: an `SMBus` `write_byte_data(0x1a, 0x80, v)`
//! arrives as the bytes `80 v`, and `0x80` is 128, which clamps to a fan duty of 100%. Worse,
//! an `SMBus` register *read* places the register number on the bus the same way, so reading
//! register `0x80` to "safely detect" the dialect sets the fan to full.
//!
//! [`LegacyMcu`] reproduces that faithfully. A test can therefore assert that our code, in
//! its default configuration, never produces the transaction — turning the single most
//! damaging misconfiguration into a failing unit test rather than a field incident.
//!
//! See ADR-0002 and `ARGON-MCU-HAZARD` in `docs/protocol/FACTS.md`.

/// The documented I2C address of the ONE-family MCU.
pub const MCU_ADDR: u8 = 0x1a;

/// Documented single-byte commands (`ARGON-MCU-L-*`).
pub mod opcode {
    /// Stop the fan.
    pub const FAN_OFF: u8 = 0x00;
    /// Highest valid fan duty as a literal percent.
    pub const FAN_MAX: u8 = 0x64;
    /// Select "default mode": a button press is required to power on.
    pub const MODE_DEFAULT: u8 = 0xFD;
    /// Select "always on" mode.
    pub const MODE_ALWAYS_ON: u8 = 0xFE;
    /// Arm power-cut via UART TX voltage monitoring.
    pub const ARM_POWER_CUT: u8 = 0xFF;
    /// IR code block-write command.
    pub const IR_WRITE: u8 = 0xAA;
    /// Enter the firmware bootloader. **`argon-utils` never emits this.**
    pub const BOOTLOADER: u8 = 0xBB;
}

/// Which power-on behaviour the MCU is set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerMode {
    /// A button press is needed to power on after shutdown or power loss.
    RequiresButton,
    /// Power flows to the Pi without a button press.
    AlwaysOn,
}

/// What a simulated MCU has been told to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McuState {
    /// Current fan duty as a percentage.
    pub fan_percent: u8,
    /// Power-on behaviour.
    pub power_mode: PowerMode,
    /// Whether the power cut has been armed.
    pub power_cut_armed: bool,
    /// Whether the MCU was put into its bootloader.
    ///
    /// Reaching this on real hardware risks an unrecoverable device, because the bootloader
    /// *exit* sequence is unknown. The simulator records it so a test can assert we never do.
    pub in_bootloader: bool,
    /// Every transaction received, for assertions.
    pub transactions: Vec<Vec<u8>>,
}

impl Default for McuState {
    fn default() -> Self {
        Self {
            fan_percent: 0,
            power_mode: PowerMode::RequiresButton,
            power_cut_armed: false,
            in_bootloader: false,
            transactions: Vec::new(),
        }
    }
}

/// Firmware that implements **only** the documented single-byte protocol.
///
/// Every byte it receives is a command. It has no registers, and cannot tell a register
/// number from a fan duty — because on the wire there is no difference.
#[derive(Debug, Default)]
pub struct LegacyMcu {
    state: McuState,
}

impl LegacyMcu {
    /// A fresh MCU.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the MCU has been told to do.
    #[must_use]
    pub const fn state(&self) -> &McuState {
        &self.state
    }

    /// Delivers a transaction to the MCU.
    pub fn receive(&mut self, data: &[u8]) {
        self.state.transactions.push(data.to_vec());

        // Legacy firmware interprets the FIRST byte as a command and ignores any that
        // follow. This is what makes a register write dangerous: `write_byte_data(0x80, 25)`
        // puts `80 19` on the bus, and the MCU acts on `0x80`.
        let Some(&first) = data.first() else { return };
        match first {
            opcode::FAN_OFF => self.state.fan_percent = 0,
            1..=opcode::FAN_MAX => self.state.fan_percent = first,
            opcode::MODE_DEFAULT => self.state.power_mode = PowerMode::RequiresButton,
            opcode::MODE_ALWAYS_ON => self.state.power_mode = PowerMode::AlwaysOn,
            opcode::ARM_POWER_CUT => self.state.power_cut_armed = true,
            opcode::BOOTLOADER => self.state.in_bootloader = true,
            opcode::IR_WRITE => {}
            // Anything above 0x64 that is not a known command is still a fan-duty byte to
            // this firmware, clamped to full. 0x80 -- the duty-cycle *register* number in the
            // newer protocol -- lands here, which is the whole hazard.
            other => self.state.fan_percent = other.min(100),
        }
    }
}

/// Firmware that implements the register protocol.
///
/// Included for completeness. `argon-utils` does not address it by default, and the register
/// code path is behind a Cargo feature that is off in shipped builds.
#[derive(Debug, Default)]
pub struct RegisterMcu {
    state: McuState,
}

impl RegisterMcu {
    /// A fresh MCU.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What the MCU has been told to do.
    #[must_use]
    pub const fn state(&self) -> &McuState {
        &self.state
    }

    /// Delivers a transaction to the MCU.
    pub fn receive(&mut self, data: &[u8]) {
        self.state.transactions.push(data.to_vec());
        match data {
            // Register write: register number then value.
            [0x80, value, ..] => self.state.fan_percent = (*value).min(100),
            [0x86, 1, ..] => self.state.power_cut_armed = true,
            // A bare byte is still understood, for compatibility.
            [single] => match *single {
                opcode::FAN_OFF => self.state.fan_percent = 0,
                1..=opcode::FAN_MAX => self.state.fan_percent = *single,
                opcode::ARM_POWER_CUT => self.state.power_cut_armed = true,
                opcode::BOOTLOADER => self.state.in_bootloader = true,
                _ => {}
            },
            _ => {}
        }
    }
}
