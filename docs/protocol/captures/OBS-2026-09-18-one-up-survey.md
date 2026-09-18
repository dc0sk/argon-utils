---
project: argon-utils
doc: protocol/captures/OBS-2026-09-18-one-up-survey
status: evidence
last_updated: 2026-09-18
---

# OBS-2026-09-18-one-up-survey

First look at an **Argon ONE UP** (the CM5 laptop), reached over SSH as `one-up-pi`.
**Passive, apart from one I2C presence scan and, later, one register read. Nothing was
written.** The vendor's
`argononeupd` service was running throughout and was left alone; its files were not read.

## Result

- **One I2C device on bus 1, at `0x64`. Nothing at `0x1a`.** So the ONE UP's controller is
  not where the ONE-family MCU sits, and nothing learned about the `0x1a` protocol carries
  over by default. It is a CW2217 fuel gauge -- see *Identification* below.
- **No kernel battery driver.** `/sys/class/power_supply/` is empty: whatever reports the
  battery, the kernel is not bound to it, and the `0x64` scan was not refused as busy.
- **The fan is the kernel's**, as on the ONE V5 with a Pi 5: `pwm-fan` cooling device, hwmon
  `pwmfan`, RP1 line `FAN_PWM` held by the kernel.
- **The power key is the Pi's own**: input `pwr_button`, line `PWR_GPIO` held by
  `pwr_button`.
- **GPIO27 is held by the vendor daemon** (consumer `argon`, pull-up, edge events on both
  edges) -- consistent with the lid switch the plan placed there, not yet confirmed as one.
- **No UPS on USB.** No `/dev/serial/by-id`, and the five hidraw nodes are the keyboard and a
  USB audio adapter. The PWR UPS protocol does not apply here.

## Identification (later the same day)

A forum post citing the ONE UP's block diagram named the part a Cellwise CW2217, whose fixed
address is `0x64` by its datasheet (FACTS: `[FORUM-ONEUP-CW2217]`, `[DS-CW2217]`). With the
operator's go-ahead, one register was read at 2026-09-18T18:36:17Z:

| Transaction | Result |
|---|---|
| `S 64+W 00 Sr 64+R data P` (SMBus read-byte-data, register `0x00`) | `0xA0` |

`0xA0` is the value the datasheet fixes for the CW2217's VERSION register in every mode. The
written byte only sets the register pointer: the datasheet's write needs a data byte after
it. **The device at `0x64` is a CW2217** (`ONEUP-0x64-IDENTITY`, now `observed`). The vendor
daemon was running; nothing else was read at that point.

## Battery registers (2026-09-18T18:55:33Z)

With the operator's go-ahead: every register below read three times, 2 s apart, one SMBus
read-byte-data each. Nothing written. Vendor daemon running. Charger state at the time not
recorded.

| Register | Raw (3 reads) | Decoded per [DS-CW2217] |
|---|---|---|
| `0x00` VERSION | `a0 a0 a0` | CW2217 |
| `0x02`-`0x03` VCELL | `36f8 36f6 36f7` | 4.3975 / 4.3969 / 4.3972 V |
| `0x04`-`0x05` SOC | `6400` ×3 | 100.00 % |
| `0x06` TEMP | `82` ×3 | 25.0 °C -- **the register's power-on default**, see below |
| `0x08` CONFIG | `00` ×3 | active: neither SLEEP nor RESTART set |
| `0x0E`-`0x0F` CURRENT | `fffb fffe 0000` | -5, -2, 0 LSB: effectively zero, within noise |
| `0xA4`-`0xA5` cycles | `0018` ×3 | 24 charge cycles |
| `0xA6` SOH | `64` ×3 | 100 % |

Reading it:

- **Full and at rest.** 100 % with current at zero is a charged battery with charging finished,
  or no charger and a near-zero load -- the second is unlikely with the machine running. The
  current's sign convention (positive = charging) is documented but not yet seen on this board.
- **The temperature is not trusted.** `0x82` is exactly the datasheet's reset value, and it did
  not move. Either the pack is at 25.0 °C, or nothing drives the TS pin and the register
  holds its default. Not reported as a measurement until it is seen to change.
- **One cell, or a scaled one.** VCELL reads 4.40 V, a single Li-ion cell's range (and a high
  one, suggesting a 4.4 V-class cell). Argon rates the pack at 55 Wh and 4800 mAh, which
  implies about 11.5 V nominal -- three cells in series. The CW2217 measures one cell. How it
  is wired to the pack is `unknown`.
- Multi-byte values were read a byte at a time. The datasheet does not say whether the pair
  is latched, so a value could tear across an update; the three samples agree to within
  noise, and a driver should read both bytes in one transaction or read twice and compare.

## On battery (2026-09-18T19:00:59Z)

The operator unplugged the charger; six samples 2 s apart. Two-byte values were read in one
`i2c_rdwr` transaction (`S 64+W reg Sr 64+R d0 d1 P`) and again byte by byte; every pair agreed.

| Sample | VCELL | SOC | CURRENT (LSB) | CURRENT raw |
|---|---|---|---|---|
| 0 | 4.3597 V | 100.00 % | -3008 | `f440` |
| 1 | 4.3684 V | 100.00 % | -2263 | `f729` |
| 2 | 4.3700 V | 100.00 % | -2092 | `f7d4` |
| 3 | 4.3678 V | 100.00 % | -2257 | `f72f` |
| 4 | 4.3697 V | 100.00 % | -2103 | `f7c9` |
| 5 | 4.3675 V | 100.00 % | -2266 | `f726` |

- **Discharge reads negative on this board**, as documented. `ONEUP-CURRENT-SIGN` is
  observed for discharge; charging (positive) is still to be seen.
- About -2200 LSB is 3.5 mV across the sense resistor: 0.35 A if it is the datasheet's
  typical 10 mΩ. With the resistor and the pack wiring unknown, no ampere or watt figure
  is claimed.
- The cell voltage sags about 30 mV under load (4.397 V at rest).
- The auto-incrementing two-byte read the datasheet implies ("the number of bytes per
  transfer is unrestricted") works: it returns the same pair as two single reads.

## Charger back (2026-09-18T19:02:16Z)

The operator plugged the charger back in, about a minute after the samples above; eight samples
2 s apart, two-byte values in one transaction.

| Sample | VCELL | SOC | CURRENT (LSB) | raw |
|---|---|---|---|---|
| 0 | 4.4316 V | 100.00 % | +2875 | `0b3b` |
| 1 | 4.4319 V | 100.00 % | +2845 | `0b1d` |
| 2 | 4.4319 V | 100.00 % | +2817 | `0b01` |
| 3 | 4.4316 V | 100.00 % | +2796 | `0aec` |
| 4 | 4.4316 V | 100.00 % | +2763 | `0acb` |
| 5 | 4.4319 V | 100.00 % | +2736 | `0ab0` |
| 6 | 4.4316 V | 100.00 % | +2708 | `0a94` |
| 7 | 4.4313 V | 100.00 % | +2688 | `0a80` |

- **Charging reads positive on this board**, as documented: the sign is now observed both ways.
- The current falls steadily, about 1 % per sample, at a flat 4.432 V: the shape of a charger
  topping up at constant voltage and tapering as the cell fills.
- SOC stayed at 100.00 % throughout the discharge and recharge -- the gauge's percentage does
  not move for a minute's draw, so the current is the only quick indicator of which way
  power is flowing.

## Method

All over SSH as the unprivileged login user (groups include `i2c`, `gpio`).

| What | How |
|---|---|
| Identity | `/proc/device-tree/model`: *Raspberry Pi Compute Module 5 Rev 1.0*; kernel 6.18.50+rpt-rpi-2712; Debian 13 |
| I2C adapters | `/sys/bus/i2c/devices`: `i2c-1` (DesignWare), `i2c-13`, `i2c-14` (brcmstb). No kernel-bound clients on any |
| Power supply, hwmon, cooling | sysfs listings: hwmon `cpu_thermal`, `nvme`, `rp1_adc`, `pwmfan`, `rpi_volt` |
| USB, hidraw, input | `lsusb`; `HID_NAME` per hidraw; `/sys/class/input/*/name` |
| GPIO | `gpioinfo` (reads line state and consumers; requests nothing) |
| Services | `systemctl`: `argononeupd.service`, enabled, running (`python3 /etc/argon/argononeupd.py SERVICE`) |
| Boot config | `config.txt` non-comment lines; `rpi-eeprom-config`: `PSU_MAX_CURRENT=5000`, `BOOT_UART=1`, `BOOT_ORDER=0xf2461`; no `POWER_OFF_ON_HALT`, no `WAKE_ON_GPIO` set |
| I2C presence | bus 1, `0x03`-`0x77`, in `i2cdetect`'s default mode (i2c-tools is not installed; the same operations with `smbus2`): a one-byte **read** with no data sent at `0x30`-`0x37` and `0x50`-`0x5F`, where EEPROMs that a quick write can upset live; a **quick write** (address and write bit, zero data bytes) elsewhere. Buses 13 and 14 were not scanned |

A quick write carries no data byte, so there is no register number or command for a device
to act on -- the reason it is the one operation discovery permits (plan, section 3.3a).

## What this does not tell us

- Current in amperes: the ONE UP's sense resistor is unknown. Nor has the current been seen
  away from zero, so its sign on this board is unconfirmed.
- How the single-cell gauge relates to what is rated as a three-cell pack.
- Whether GPIO27 is the lid switch. Watching it would need the vendor daemon stopped, since
  it holds the line.
- Anything about charging, wake or the ONE UP's power path.
