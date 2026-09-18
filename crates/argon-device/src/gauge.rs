// SPDX-License-Identifier: GPL-3.0-or-later
//! The Argon ONE UP's battery, through its Cellwise CW2217 fuel gauge.
//!
//! Facts: `ONEUP-0x64-IDENTITY`, `ONEUP-CURRENT-SIGN`, `ONEUP-GAUGE-BURST` (`observed`) and
//! `CW2217-*` (`documented`). Reads only, and only the registers in
//! [`argon_proto::cw2217::READS`]; the bus it holds cannot write at all.

use crate::ups::{Monitor, Poll, Watch};
use argon_hal::i2c::RegisterRead;
use argon_hal::{Error, Result};
use argon_proto::cw2217::{self, Flow, reg};
use argon_proto::ups::BatteryStatus;
use argon_proto::ups::policy::BatteryPolicy;
use std::time::Duration;

/// One reading of the gauge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// Charge, whole percent, 0-100.
    pub percent: u8,
    /// Cell voltage, microvolts.
    pub vcell_uv: u32,
    /// The current register, raw. Positive is charging; not in amperes (`ONEUP-RSENSE`).
    pub current: i16,
    /// Which way charge is moving.
    pub flow: Flow,
}

impl Reading {
    /// What the battery policy needs from it.
    #[must_use]
    pub const fn battery(&self) -> BatteryStatus {
        BatteryStatus {
            percent: self.percent,
            source: cw2217::power_source(self.flow),
        }
    }
}

/// Wear figures, read on request rather than every poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    /// Charge cycles the gauge has counted.
    pub cycles: u16,
    /// State of health, percent.
    pub soh: u8,
}

/// A CW2217 whose identity has been confirmed.
pub struct Cw2217<B: RegisterRead> {
    bus: B,
}

impl<B: RegisterRead> Cw2217<B> {
    /// Takes a bus bound to the gauge's address, and confirms what is there before trusting it.
    ///
    /// # Errors
    ///
    /// Fails on bus error, or if `VERSION` does not read [`cw2217::VERSION_VALUE`]: whatever
    /// answers there is then not the chip this driver was written for.
    pub fn identify(bus: B) -> Result<Self> {
        let mut gauge = Self { bus };
        let [version] = gauge.read::<1>(reg::VERSION)?;
        if version != cw2217::VERSION_VALUE {
            return Err(Error::Parse {
                what: "a CW2217 VERSION register (expected 0xa0)",
                got: format!("0x{version:02x}"),
            });
        }
        Ok(gauge)
    }

    /// Reads charge, voltage and current.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    pub fn read_now(&mut self) -> Result<Reading> {
        let (percent, _) = cw2217::soc(self.read(reg::SOC)?);
        let vcell_uv = cw2217::vcell_microvolts(self.read(reg::VCELL)?);
        let current = cw2217::current_raw(self.read(reg::CURRENT)?);
        Ok(Reading {
            percent,
            vcell_uv,
            current,
            flow: cw2217::flow(current),
        })
    }

    /// Reads the cycle count and state of health.
    ///
    /// # Errors
    ///
    /// Fails on bus error.
    pub fn health(&mut self) -> Result<Health> {
        let cycles = u16::from_be_bytes(self.read(reg::CYCLES)?);
        let [soh] = self.read::<1>(reg::SOH)?;
        Ok(Health { cycles, soh })
    }

    /// Where the gauge is, for logs.
    pub fn describe(&self) -> String {
        self.bus.describe()
    }

    /// Reads `N` bytes at `register`, if that read is one of [`cw2217::READS`].
    fn read<const N: usize>(&mut self, register: u8) -> Result<[u8; N]> {
        if !cw2217::is_permitted_read(register, N) {
            return Err(Error::WriteBlocked {
                what: format!("cw2217 read of {N} byte(s) at 0x{register:02x}"),
                reason: "not one of the documented read-only registers this driver may read",
            });
        }
        let mut buf = [0; N];
        self.bus.read_registers(register, &mut buf)?;
        Ok(buf)
    }
}

/// Polls the gauge and feeds the battery policy.
pub struct GaugeMonitor<B: RegisterRead> {
    gauge: Cw2217<B>,
    watch: Watch,
    last: Option<Reading>,
}

impl<B: RegisterRead> GaugeMonitor<B> {
    /// Creates a monitor.
    pub const fn new(gauge: Cw2217<B>, policy: BatteryPolicy) -> Self {
        Self {
            gauge,
            watch: Watch::new(policy),
            last: None,
        }
    }

    /// The last successful reading, with the detail the policy does not use.
    pub const fn last(&self) -> Option<Reading> {
        self.last
    }

    /// The gauge, for occasional reads outside the poll.
    pub const fn gauge_mut(&mut self) -> &mut Cw2217<B> {
        &mut self.gauge
    }
}

impl<B: RegisterRead> Monitor for GaugeMonitor<B> {
    fn poll(&mut self, uptime: Duration) -> Poll {
        let reading = self.gauge.read_now();
        self.last = reading.as_ref().ok().copied();
        self.watch.observe(reading.map(|r| r.battery()), uptime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon_proto::ups::PowerSource;
    use argon_proto::ups::policy::{Level, PolicyConfig};
    use std::collections::HashMap;

    /// A gauge's register file. Records every read, so tests can say what reached the bus.
    struct Fake {
        regs: HashMap<u8, u8>,
        reads: Vec<(u8, usize)>,
        fail: bool,
    }

    impl Fake {
        fn one_up(soc: u8, current: i16) -> Self {
            let c = current.to_be_bytes();
            let regs = HashMap::from([
                (0x00, 0xA0),
                (0x02, 0x36),
                (0x03, 0xF8),
                (0x04, soc),
                (0x05, 0x00),
                (0x0E, c[0]),
                (0x0F, c[1]),
                (0xA4, 0x00),
                (0xA5, 0x18),
                (0xA6, 0x64),
            ]);
            Self {
                regs,
                reads: Vec::new(),
                fail: false,
            }
        }
    }

    impl RegisterRead for Fake {
        fn read_registers(&mut self, register: u8, buf: &mut [u8]) -> Result<()> {
            if self.fail {
                return Err(Error::Io(std::io::Error::other("nak")));
            }
            self.reads.push((register, buf.len()));
            for (i, b) in buf.iter_mut().enumerate() {
                *b = self
                    .regs
                    .get(&(register + u8::try_from(i).unwrap()))
                    .copied()
                    .unwrap_or(0);
            }
            Ok(())
        }
        fn describe(&self) -> String {
            "fake".into()
        }
    }

    #[test]
    fn a_chip_that_is_not_a_cw2217_is_not_trusted() {
        let mut fake = Fake::one_up(50, 0);
        fake.regs.insert(0x00, 0x42);
        assert!(Cw2217::identify(fake).is_err());
    }

    #[test]
    fn reads_what_was_read_on_the_one_up() {
        let mut g = Cw2217::identify(Fake::one_up(100, -3008)).unwrap();
        let r = g.read_now().unwrap();
        assert_eq!(r.percent, 100);
        assert_eq!(r.vcell_uv, 4_397_500);
        assert_eq!(r.current, -3008);
        assert_eq!(r.battery().source, PowerSource::Battery);
        assert_eq!(
            g.health().unwrap(),
            Health {
                cycles: 24,
                soh: 100
            }
        );
    }

    #[test]
    fn every_read_that_reaches_the_bus_is_a_permitted_one() {
        let mut g = Cw2217::identify(Fake::one_up(80, 2800)).unwrap();
        g.read_now().unwrap();
        g.health().unwrap();
        assert!(!g.bus.reads.is_empty());
        for &(r, n) in &g.bus.reads {
            assert!(
                cw2217::is_permitted_read(r, n),
                "0x{r:02x} x{n} reached the bus"
            );
        }
    }

    #[test]
    fn a_read_outside_the_list_is_refused_before_the_bus() {
        let mut g = Cw2217::identify(Fake::one_up(80, 0)).unwrap();
        let before = g.bus.reads.len();
        assert!(g.read::<1>(0x08).is_err(), "CONFIG was read");
        assert_eq!(
            g.bus.reads.len(),
            before,
            "the refused read reached the bus"
        );
    }

    #[test]
    fn a_draining_battery_is_confirmed_critical_and_a_failed_read_never_is() {
        let policy = BatteryPolicy::new(PolicyConfig::default()).unwrap();
        let up = Duration::from_secs(3_600);
        let mut m = GaugeMonitor::new(Cw2217::identify(Fake::one_up(8, -2200)).unwrap(), policy);
        assert_ne!(
            m.poll(up).decision.level,
            Level::Critical,
            "one reading confirmed it"
        );
        assert_eq!(m.poll(up).decision.level, Level::Critical);
        assert_eq!(m.last().unwrap().percent, 8);

        m.gauge.bus.fail = true;
        let p = m.poll(up);
        assert_eq!(p.decision.level, Level::Unknown);
        assert_eq!(p.consecutive_failures, 1);
        assert_eq!(m.last(), None, "a stale reading was kept as the last one");
    }

    #[test]
    fn low_charge_on_the_charger_is_not_on_battery() {
        let policy = BatteryPolicy::new(PolicyConfig::default()).unwrap();
        let up = Duration::from_secs(3_600);
        let mut m = GaugeMonitor::new(Cw2217::identify(Fake::one_up(5, 2800)).unwrap(), policy);
        for _ in 0..3 {
            assert_eq!(m.poll(up).decision.level, Level::OnMains);
        }
    }
}
