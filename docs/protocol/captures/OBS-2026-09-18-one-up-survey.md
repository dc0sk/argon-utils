---
project: argon-utils
doc: protocol/captures/OBS-2026-09-18-one-up-survey
status: evidence
last_updated: 2026-09-18
---

# OBS-2026-09-18-one-up-survey

First look at an **Argon ONE UP** (the CM5 laptop), reached over SSH as `one-up-pi`.
**Passive, apart from one I2C presence scan. No register was read or written.** The vendor's
`argononeupd` service was running throughout and was left alone; its files were not read.

## Result

- **One I2C device on bus 1, at `0x64`. Nothing at `0x1a`.** So the ONE UP's controller is
  not where the ONE-family MCU sits, and nothing learned about the `0x1a` protocol carries
  over by default. What `0x64` is -- a fuel gauge, an Argon MCU, something else -- is
  `unknown`.
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

- What the device at `0x64` is, or how to read the battery from it. Identifying it needs
  either published documentation (Argon's, or a datasheet once the part is known from the
  board) or a decision to read a register -- which sends a register byte first, the
  hazard ADR-002 describes, on a device whose dialect is unknown.
- Whether GPIO27 is the lid switch. Watching it would need the vendor daemon stopped, since
  it holds the line.
- Anything about charging, wake or the ONE UP's power path.
