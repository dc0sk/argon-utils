---
project: argon-utils
doc: protocol/captures/OBS-2026-09-16-v5-fan-is-kernel-controlled
status: evidence
last_updated: 2026-09-16
---

# OBS-2026-09-16-v5-fan-is-kernel-controlled

**On an Argon ONE V5 with a Raspberry Pi 5, the case fan is driven by the Pi's own kernel
thermal governor through `pwm-fan`, and there is no Argon MCU on the I2C bus at all.**

This corrects a central assumption of the project plan, which took the I2C MCU at `0x1a` to
be the fan controller on every ONE-family case.

## Evidence

### Nothing responds at 0x1a

A quick-write scan of `i2c-1` — the transaction `i2cdetect` uses, which transfers no data
byte and so is safe on any firmware generation:

```
$ sudo i2cdetect -y -q 1
30: -- -- -- -- -- -- -- -- -- -- -- -- 3c -- -- --
```

`0x3c` is the OLED. **`0x1a` is absent.** A direct single-byte write to `0x1a` returns
`EREMOTEIO` (errno 121) — an unacknowledged address.

### The fan is demonstrably running, under kernel control

```
/sys/class/hwmon/hwmon3/name        pwmfan
/sys/class/hwmon/hwmon3/pwm1        75
/sys/class/hwmon/hwmon3/fan1_input  2960        # RPM, i.e. physically spinning

/sys/class/thermal/thermal_zone0/type       cpu-thermal
/sys/class/thermal/thermal_zone0/cdev0      -> cooling_device0, type pwm-fan, state 1/4
trip_point_1_temp  50000 mC  active
trip_point_2_temp  60000 mC  active
trip_point_3_temp  67500 mC  active
trip_point_0_temp 110000 mC  critical
```

The fan is on the Raspberry Pi 5's own 4-pin fan header, with PWM control and a tachometer,
bound to the CPU thermal zone as a four-state cooling device. The device tree node
`cooling_fan` is `okay`.

### The vendor's daemon appears to be doing nothing here

`argononed` holds two file descriptors on `/dev/i2c-1` and has logged nothing since boot. Its
fan loop catches I/O errors and sleeps, so on a machine with no device at `0x1a` it fails
silently forever. The fan on this machine works because of the kernel, not because of the
vendor's software.

## Why this was missed earlier

An idle reading, generalised. At the start of the session `pwm1` and `fan1_input` both read
`0`, and that was recorded as "no fan on the Pi 5 header, so no conflict today". The machine
was at 47 °C — below the 50 °C trip point — so the fan was simply off.

The reading was accurate. The conclusion drawn from it was not, and it was never re-checked
under load. The finding surfaced only because a fan write was attempted and returned
`EREMOTEIO`, which prompted looking again.

## Provenance

| | |
|---|---|
| Host | Raspberry Pi 5 Model B Rev 1.1, revision `e04171`, in an Argon ONE V5 |
| OS | Debian 13 trixie, kernel 6.18.39+rpt-rpi-2712 |
| Date | 2026-09-16 |

## What this does and does not establish

**Established for this machine:** there is no I2C device at `0x1a`; the fan is kernel-driven
through `pwm-fan`; the OLED is present at `0x3c`.

**Not established:** whether every ONE V5 is wired this way, or whether some variant carries
an MCU. One machine is one machine. The V5 being a Pi 5 case, and the Pi 5 being the first Pi
with a fan header, makes routing the fan there the obvious design — but that is reasoning,
not evidence.

**Unaffected:** the Pi 4-era cases. ONE V2/V3, EON and the Fan HAT predate the fan header, and
the documented `0x1a` protocol remains the only way to drive their fans. The user owns a
Pi 4-era ONE V2, which is the unit that can confirm this.

## Consequences

1. **A second fan backend is needed**: `pwm-fan` via hwmon, alongside the I2C MCU. The
   capability model already anticipated this — `FanControl` is a trait precisely so the
   daemon's control loop never learns which one it has.
2. **On this machine, taking over fan control means taking it from the kernel**, not from the
   vendor's daemon. That is a different and more delicate operation: the thermal governor is
   a working, well-tested controller with a critical trip point at 110 °C, and replacing it
   needs a clear reason. "Because we can" is not one.
3. **`argonctl doctor` must report this**, so nobody else spends time on a fan MCU that is not
   there. It already warns when a `pwm-fan` cooling device is active; that warning needs to
   fire on the evidence above rather than on an idle reading.
4. **The OLED at `0x3c` is confirmed present**, which is a positive result for task T7.
